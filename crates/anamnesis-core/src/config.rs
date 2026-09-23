//! Configuration read from the per-project marker file.
//!
//! The marker describes one project and is deliberately file-only: no
//! environment layer is merged in here. A stray `ANAMNESIS_*` variable meant
//! for the server must not be able to re-scope a repository, and unknown keys
//! are rejected rather than ignored so a typo surfaces instead of silently
//! sending memory to the wrong project.
//!
//! With one exception, and it was bought with an outage. A marker file is read
//! by whatever build happens to be running, and those two move apart: on
//! 2026-09-01 this repository's marker gained a `[sessions]` table hours
//! before the installed server was rebuilt, and the older server — doing
//! exactly what the paragraph above says — answered `400` to every event of
//! every session for three hours. The events were fine. The configuration was
//! fine. The only thing wrong was that one of them was newer.
//!
//! So an unknown *table* is a feature this build does not have yet: it is
//! reported and skipped, and everything else in the file still applies. An
//! unknown *scalar* at the top level is still an error, because that is the
//! shape a typo takes — `workspace = "x"` written outside `[scope]` — and
//! catching those is why the rule exists. Nothing inside a known table is
//! relaxed at all: `[scope]` still rejects what it does not recognise, and a
//! silently wrong scope remains impossible.

use std::collections::BTreeMap;
use std::path::Path;

use figment::Figment;
use figment::providers::{Format, Toml};

use crate::decay::DecayParams;
use crate::error::{CoreError, Result};
use crate::scope::{ProjectName, WorkspaceName};
use crate::sweep::SweepPolicy;

/// Contents of a `.anamnesis.toml` marker file.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MarkerConfig {
    /// Explicit workspace and project overrides.
    pub scope: ScopeConfig,
    /// Which events never reach the spool.
    pub capture: CaptureConfig,
    /// How quickly unused pages are forgotten.
    pub decay: DecayConfig,
    /// Per-operator memory slots.
    pub slots: SlotsConfig,
    /// When a session nobody closed is summarised anyway.
    pub sessions: SessionsConfig,
    /// Automatic learning proposals.
    pub auto_improve: AutoImproveConfig,
    /// What a prompt is handed back from the pages this project already has.
    pub recall: RecallConfig,

    /// Everything in the file this build has no name for.
    ///
    /// Kept rather than rejected so that a marker written for a newer
    /// anamnesis still describes the project to an older one. What is in here
    /// is *not* applied — this build has no code for it — which is why
    /// [`MarkerConfig::unrecognized`] exists and why `anamnesis status` says
    /// so where somebody is already asking whether memory is working.
    #[serde(flatten)]
    pub extra: BTreeMap<String, figment::value::Value>,
}

impl MarkerConfig {
    /// Load a marker file from disk.
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = Figment::new().merge(Toml::file(path)).extract()?;
        config.audit(&path.display().to_string())?;
        Ok(config)
    }

    /// Parse a marker file from a string, for tests and dry runs.
    pub fn from_toml(source: &str) -> Result<Self> {
        let config: Self = Figment::new().merge(Toml::string(source)).extract()?;
        config.audit("marker file")?;
        Ok(config)
    }

    /// Tables in the file this build does not understand, in the order a
    /// person would read them.
    pub fn unrecognized(&self) -> Vec<&str> {
        self.extra
            .iter()
            .filter(|(_, value)| value.as_dict().is_some())
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Decide what to do about everything this build has no name for.
    ///
    /// A table is a feature from a newer build: said out loud and skipped, so
    /// that the rest of the file — the scope, the exclusions, the decay
    /// settings — goes on working. Anything else is a key in the wrong place,
    /// which is what a typo looks like, and it is still refused.
    fn audit(&self, origin: &str) -> Result<()> {
        for (name, value) in &self.extra {
            if value.as_dict().is_none() {
                return Err(CoreError::UnknownSetting {
                    key: name.clone(),
                    origin: origin.to_owned(),
                });
            }
        }

        // Debug, not warn, and the level is the whole decision: a marker file
        // is read once per captured event, so a warning here would be a line
        // per tool call — hundreds a session, saying the same thing, in the
        // log somebody reads when they are looking for something else. The
        // loud place for this is `anamnesis status`, which is where the
        // question "why did that setting do nothing" is actually asked.
        let unknown = self.unrecognized();
        if !unknown.is_empty() {
            tracing::debug!(
                tables = unknown.join(", "),
                marker = origin,
                "ignoring marker tables this build does not understand"
            );
        }
        Ok(())
    }
}

/// Explicit scope overrides. Either field may be omitted, in which case the
/// value is inferred from the repository.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScopeConfig {
    /// Workspace this project belongs to.
    pub workspace: Option<WorkspaceName>,
    /// Pinned project name. Overrides git-derived identity.
    pub project: Option<ProjectName>,
}

/// Capture exclusions applied before events reach the spool.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureConfig {
    /// Glob patterns whose file events are dropped entirely.
    pub ignore_paths: Vec<String>,
}

/// What a session is handed back when it submits a prompt.
///
/// On by default, and the reason is measured rather than assumed: across
/// twenty-four long-run eval sessions with the MCP server connected and its
/// tools allowed, an agent called a memory tool once. A memory nothing reads
/// is a memory that changed nothing, so the question gets asked for it. The
/// numbers are small because this is paid for on every prompt of every
/// session; see [`crate::brief`] for what the block looks like and what is
/// still open about it.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecallConfig {
    /// Whether a prompt is answered with the pages that match it.
    pub on_prompt: bool,
    /// How many pages at most.
    pub pages: usize,
    /// How many characters of each page's snippet.
    pub snippet_chars: usize,
    /// How close a page has to be to the prompt before it is worth
    /// interrupting one with.
    ///
    /// Measured rather than chosen. Sixteen prompts with `nomic-embed-text`
    /// over two corpora — this machine's 88 pages and a four-page test project
    /// — split cleanly: every prompt the project had nothing to say about
    /// peaked at 0.542 or below, and every prompt it did started at 0.573.
    /// `0.55` is between them.
    ///
    /// A distance above the corpus's median was tried first, on the reasoning
    /// that a similarity means nothing on its own and a relative one needs no
    /// knowledge of an embedder's scale. It separated the 88-page corpus
    /// better and the four-page one not at all — with four pages the median is
    /// one page away from the top, and an off-topic prompt beat it by 0.087
    /// where an on-topic one beat it by 0.083. The scale-dependence is real,
    /// which is why this is in the marker: another embedder is another number.
    /// See [`crate::brief`] and `Store::pages_like`.
    ///
    /// **On real prompts it separates much less than those sixteen said.**
    /// Measured on 2026-09-18 over the 201 distinct prompts this machine's
    /// own sessions had recorded, against its 93 pages: every prompt of five
    /// words or more had a page above 0.55, and in a random forty of them the
    /// best page was about the prompt's subject for twenty and about something
    /// else for the other twenty, at scores from 0.63 to 0.81. It cannot simply
    /// be raised: in the long-run eval the page a probe's knowledge was
    /// planted in scored 0.64 to 0.73 against that probe, inside the same
    /// band. See `docs/measurements/2026-09-18-recall-on-real-prompts.md`.
    pub min_similarity: f64,
    /// Answer from the words a prompt names when there is no embedder to
    /// answer from, or it fails.
    ///
    /// Running no model is a setup this system supports, and until this
    /// recall said nothing in it. Naming is quieter than the cosine gate — it
    /// cannot see a paraphrase — and that is the trade it makes: over 201 real
    /// prompts on this machine's own pages it gave a block 8 times where the
    /// cosine gate gave one 184 times, and asked of a project they were not
    /// about, 213 prompts got none. See `Store::pages_named_by`.
    pub by_name: bool,
    /// The share of what a prompt names that a page has to carry before it is
    /// offered by name.
    ///
    /// Unlike `min_similarity` this is not a number about a model: it is a
    /// share, from 0 to 1, and a page at 0.5 carries as much of what the prompt
    /// names as the project is missing.
    pub min_coverage: f64,
    /// How many words a prompt needs before it is asked about at all.
    ///
    /// A reply like `devam et`, `continue` or `onay` carries nothing to look
    /// up, and against a corpus written in its language it still lands near
    /// everything: on this machine's pages the one-word `yaptım` had 57 pages
    /// above the similarity gate. All twenty-two one- and two-word prompts
    /// in the measurement above were replies of that kind, and ten of them
    /// were answered with a block. Three is where that measurement stops
    /// finding them without costing a prompt that had a subject; three- and
    /// four-word prompts are a mix, and are left to the similarity gate.
    ///
    /// Words by Unicode's rules rather than by spaces, so a sentence in a
    /// script written without them is counted a word per character rather
    /// than as one. See [`words_in`].
    pub min_words: usize,
    /// How many of the project's decisions a starting session is handed,
    /// beside the handoff. 0 hands none.
    ///
    /// The handoff says what the session before this one did, and a prompt is
    /// answered with the pages close to it; a decision taken three sessions
    /// ago in conversation reaches neither. It was never in the last session,
    /// and a prompt about the work it constrains need not look like the page
    /// that records it — in the long-run eval a planted page scored 0.64 to
    /// 0.73 against its probe, inside the band an unrelated page reaches. So
    /// the decisions the project holds are named at the start, newest first,
    /// a title each, whatever the work turns out to be. See
    /// [`crate::brief::standing`].
    pub on_start: usize,
}

impl Default for RecallConfig {
    fn default() -> Self {
        Self {
            on_prompt: true,
            pages: 3,
            snippet_chars: 240,
            min_similarity: 0.55,
            by_name: true,
            min_coverage: 0.5,
            min_words: 3,
            on_start: 5,
        }
    }
}

/// How many words `text` has, by Unicode's word boundaries (UAX #29).
///
/// Punctuation and spacing are not words, and a script written without spaces
/// is not one long word: each ideograph stands on its own, which is what keeps
/// a whole question in Chinese or Japanese from counting as a one-word reply.
pub fn words_in(text: &str) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    text.unicode_words().count()
}

/// Retention tuning for the decay sweep.
///
/// Expressed in half-lives rather than the exponential rates the formula
/// actually uses: "unread pages lose half their weight every 30 days" is a
/// sentence someone can hold an opinion about, and `lambda = 0.0231` is not.
/// The conversion happens in [`Self::policy`], which is also where a value
/// that would make the sweep nonsense is refused.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DecayConfig {
    /// Retention score below which an unexempt page is forgotten.
    pub threshold: f64,
    /// Days for the age term to halve.
    pub age_half_life_days: f64,
    /// Days for the access term to halve once a page stops being read.
    pub access_half_life_days: f64,
    /// Weight of the access term relative to the age term.
    pub access_weight: f64,
}

impl Default for DecayConfig {
    fn default() -> Self {
        let params = DecayParams::default();
        Self {
            threshold: SweepPolicy::DEFAULT_THRESHOLD,
            age_half_life_days: DecayParams::AGE_HALF_LIFE_DAYS,
            access_half_life_days: DecayParams::ACCESS_HALF_LIFE_DAYS,
            access_weight: params.sigma,
        }
    }
}

impl DecayConfig {
    /// Turn the configured half-lives into the policy a sweep runs with.
    ///
    /// Rejects rather than clamps. A half-life of zero divides by zero and
    /// makes every page immortal; a negative one makes pages *gain* weight
    /// with age. Both are typos, and the failure mode of guessing is a sweep
    /// that deletes the wrong pages while reporting nothing unusual.
    pub fn policy(&self) -> Result<SweepPolicy> {
        Self::positive("decay.age_half_life_days", self.age_half_life_days)?;
        Self::positive("decay.access_half_life_days", self.access_half_life_days)?;
        Self::not_negative("decay.access_weight", self.access_weight)?;
        Self::not_negative("decay.threshold", self.threshold)?;

        Ok(SweepPolicy {
            params: DecayParams {
                lambda: std::f64::consts::LN_2 / self.age_half_life_days,
                sigma: self.access_weight,
                mu: std::f64::consts::LN_2 / self.access_half_life_days,
            },
            threshold: self.threshold,
        })
    }

    /// Reject a value that is not a finite number greater than zero.
    fn positive(key: &'static str, value: f64) -> Result<()> {
        if value.is_finite() && value > 0.0 {
            return Ok(());
        }
        Err(CoreError::InvalidSetting {
            key,
            value,
            reason: "must be a finite number greater than zero",
        })
    }

    /// Reject a value that is not a finite number of zero or more.
    fn not_negative(key: &'static str, value: f64) -> Result<()> {
        if value.is_finite() && value >= 0.0 {
            return Ok(());
        }
        Err(CoreError::InvalidSetting {
            key,
            value,
            reason: "must be a finite number of zero or more",
        })
    }
}

/// Per-operator memory slot settings.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SlotsConfig {
    /// Give each authenticated operator an isolated slot.
    pub per_user: bool,
}

/// What happens to a session the harness never closed.
///
/// Consolidation is driven by `SessionEnd`, and that event is not guaranteed:
/// an editor that crashes, a machine that reboots, a process someone kills
/// sends nothing. Without this the session stays open forever and its
/// observations never become a page — the transcript survives under `raw/`,
/// but nothing reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionsConfig {
    /// Minutes of silence after which an open session is summarised anyway.
    ///
    /// Zero turns this off for the project. The default is long on purpose:
    /// the cost of waiting is a page that arrives late, and the cost of being
    /// hasty is summarising a session that was only at lunch.
    pub stale_after_minutes: u32,
    /// Seconds of silence after which an open session is written up for the
    /// session that has just started beside it.
    ///
    /// Measured in seconds because it measures a pause, where
    /// `stale_after_minutes` measures an afternoon. Silence alone cannot say a
    /// session is over — on this project's own history, live sessions went
    /// quiet for more than two minutes 417 times — so this never ends a
    /// session. It refreshes the page and leaves a note beside it for whoever
    /// just sat down; the session ends when it ends.
    ///
    /// The difference from the reaper is what else is known: somebody started
    /// working in the same slot. Zero turns it off.
    ///
    /// It applies to a session caught mid-turn. One whose last event is its
    /// agent's answer is waiting for its person rather than working, and is
    /// written up at once — somebody who read that answer and switched agents
    /// is carrying on from it.
    pub handover_after_seconds: u32,
}

impl Default for SessionsConfig {
    fn default() -> Self {
        Self {
            stale_after_minutes: 720,
            // Two minutes. Of the handovers in this project's own history,
            // 24% began within a minute of the session before them going
            // quiet and 31% within five, so waiting longer misses the people
            // this is for; and a working agent is heard from every few
            // seconds, because every tool call is two events.
            handover_after_seconds: 120,
        }
    }
}

/// Automatic learning proposal settings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutoImproveConfig {
    /// Whether proposals are generated at all.
    pub enabled: bool,
    /// Hold proposals for human approval before they are applied.
    pub require_approval: bool,
    /// Background scheduling. A single table, never a list.
    pub scheduler: SchedulerConfig,
}

impl Default for AutoImproveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            require_approval: true,
            scheduler: SchedulerConfig::default(),
        }
    }
}

/// Background scheduler settings for auto-improvement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SchedulerConfig {
    /// Whether the scheduler runs.
    pub enabled: bool,
    /// Minutes between runs.
    pub interval_minutes: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: 60,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_marker_yields_defaults() {
        let config = MarkerConfig::from_toml("").expect("parses");
        assert!(config.scope.workspace.is_none());
        assert!(config.scope.project.is_none());
        assert!(config.auto_improve.require_approval);
        assert!(!config.slots.per_user);
        assert_eq!(
            config.sessions,
            SessionsConfig::default(),
            "a marker written before this setting existed keeps the default"
        );
    }

    /// The default is long on purpose. A session that has merely gone quiet
    /// looks exactly like one that died, so being hasty would summarise
    /// somebody's lunch break.
    #[test]
    fn abandoned_sessions_are_left_for_hours_by_default() {
        assert_eq!(SessionsConfig::default().stale_after_minutes, 720);
    }

    /// Two minutes, and measured against the other threshold rather than on
    /// its own: these answer different questions. A handover is a pause
    /// somebody else walked in on; a reaped session is one nobody came back
    /// to. If the two ever meet in the middle, the first is ending sessions
    /// the second was written to wait out.
    #[test]
    fn a_handover_is_a_pause_and_a_reaping_is_an_afternoon() {
        let sessions = SessionsConfig::default();
        assert_eq!(sessions.handover_after_seconds, 120);
        assert!(
            u64::from(sessions.handover_after_seconds) * 10
                < u64::from(sessions.stale_after_minutes) * 60
        );
    }

    #[test]
    fn a_project_can_set_or_disable_the_handover_threshold() {
        let set = MarkerConfig::from_toml(
            "[sessions]
handover_after_seconds = 30
",
        )
        .expect("parses");
        assert_eq!(set.sessions.handover_after_seconds, 30);
        assert_eq!(
            set.sessions.stale_after_minutes,
            SessionsConfig::default().stale_after_minutes,
            "the other threshold keeps its default"
        );

        let off = MarkerConfig::from_toml(
            "[sessions]
handover_after_seconds = 0
",
        )
        .expect("parses");
        assert_eq!(off.sessions.handover_after_seconds, 0);
    }

    #[test]
    fn a_project_can_set_or_disable_the_stale_threshold() {
        let set = MarkerConfig::from_toml(
            "[sessions]
stale_after_minutes = 45
",
        )
        .expect("parses");
        assert_eq!(set.sessions.stale_after_minutes, 45);

        let off = MarkerConfig::from_toml(
            "[sessions]
stale_after_minutes = 0
",
        )
        .expect("parses");
        assert_eq!(off.sessions.stale_after_minutes, 0);
    }

    /// The marker rejects what it does not know, so a typo surfaces instead of
    /// quietly meaning nothing.
    #[test]
    fn an_unknown_key_in_the_sessions_table_is_refused() {
        assert!(
            MarkerConfig::from_toml(
                "[sessions]
stale_after_mins = 45
"
            )
            .is_err()
        );
    }

    #[test]
    fn scheduler_is_a_single_table() {
        let config = MarkerConfig::from_toml(
            "[auto_improve.scheduler]\nenabled = true\ninterval_minutes = 30\n",
        )
        .expect("parses");
        assert!(config.auto_improve.scheduler.enabled);
        assert_eq!(config.auto_improve.scheduler.interval_minutes, 30);
    }

    #[test]
    fn scheduler_written_as_an_array_is_rejected() {
        // `[[auto_improve.scheduler]]` is a table *array*; accepting it silently
        // would mean the schedule is quietly ignored at runtime.
        let err = MarkerConfig::from_toml(
            "[[auto_improve.scheduler]]\nenabled = true\ninterval_minutes = 30\n",
        );
        assert!(err.is_err());
    }

    #[test]
    fn unknown_keys_are_reported_rather_than_ignored() {
        assert!(MarkerConfig::from_toml("[scope]\nworkspce = \"typo\"\n").is_err());
    }

    #[test]
    fn invalid_names_fail_at_load_time() {
        assert!(MarkerConfig::from_toml("[scope]\nproject = \"../escape\"\n").is_err());
    }

    #[test]
    fn decay_defaults_match_the_formula_defaults() {
        let config = MarkerConfig::from_toml("").expect("parses");
        let policy = config.decay.policy().expect("valid");
        assert_eq!(policy, SweepPolicy::default());
    }

    #[test]
    fn half_lives_become_rates() {
        let config = MarkerConfig::from_toml(
            "[decay]
age_half_life_days = 10.0
access_half_life_days = 5.0
",
        )
        .expect("parses");
        let policy = config.decay.policy().expect("valid");

        // A rate is only meaningful through what it does to a score: after one
        // half-life, half remains.
        let inputs = crate::decay::DecayInputs::unread(1.0, 10.0);
        let halved = crate::decay::retention_score(inputs, policy.params);
        assert!((halved - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_zero_half_life_is_refused_rather_than_dividing_by_zero() {
        let config = MarkerConfig::from_toml(
            "[decay]
age_half_life_days = 0.0
",
        )
        .expect("parses");
        let error = config.decay.policy().expect_err("rejected");
        assert!(error.to_string().contains("decay.age_half_life_days"));
    }

    #[test]
    fn a_negative_threshold_is_refused() {
        let config = MarkerConfig::from_toml(
            "[decay]
threshold = -1.0
",
        )
        .expect("parses");
        assert!(config.decay.policy().is_err());
    }

    #[test]
    fn a_misspelled_decay_key_is_reported() {
        // The sweep deletes pages. A tuning key that silently does nothing is
        // the difference between "forgets after a year" and "forgets today".
        assert!(
            MarkerConfig::from_toml(
                "[decay]
threshhold = 0.1
"
            )
            .is_err()
        );
    }

    #[test]
    fn full_marker_round_trips() {
        let config = MarkerConfig::from_toml(
            r#"
[scope]
workspace = "default"
project = "anamnesis"

[capture]
ignore_paths = ["target/**", "*.lock"]

[slots]
per_user = false

[auto_improve]
enabled = true
require_approval = true

[auto_improve.scheduler]
enabled = false
interval_minutes = 60

[decay]
threshold = 0.05
age_half_life_days = 30.0
access_half_life_days = 14.0
access_weight = 0.5
"#,
        )
        .expect("parses");

        assert_eq!(
            config.scope.project.expect("project set").as_str(),
            "anamnesis"
        );
        assert_eq!(config.capture.ignore_paths.len(), 2);
        assert_eq!(
            config.decay.policy().expect("valid"),
            SweepPolicy::default()
        );
    }

    /// The outage this rule was changed for, in miniature. On 2026-09-01 the
    /// marker gained `[sessions]` before the installed server was rebuilt, and
    /// the older server refused every event of every session for three hours —
    /// not because anything was wrong with the events, but because the file
    /// was newer than the binary reading it. A table this build has no name
    /// for must cost that table and nothing else.
    #[test]
    fn a_table_from_a_newer_anamnesis_costs_only_that_table() {
        let config = MarkerConfig::from_toml(
            r#"
[scope]
workspace = "default"
project = "anamnesis"

[capture]
ignore_paths = ["target/**"]

[a_feature_from_the_future]
stale_after_minutes = 720
"#,
        )
        .expect("a marker written for a newer build still describes this project");

        assert_eq!(config.unrecognized(), ["a_feature_from_the_future"]);
        assert_eq!(config.capture.ignore_paths.len(), 1);
        assert_eq!(
            config.scope.project.as_ref().expect("project set").as_str(),
            "anamnesis",
            "the scope was lost over a table that has nothing to do with it"
        );
    }

    /// And the rule it does not relax. A key outside every table is what a
    /// typo looks like — `workspace` written above `[scope]` rather than
    /// inside it — and accepting one would mean a project quietly recording
    /// into somebody else's memory.
    #[test]
    fn a_key_outside_every_table_is_still_refused() {
        let error = MarkerConfig::from_toml("workspace = \"somewhere-else\"\n")
            .expect_err("a stray key is a typo, not a feature");

        let message = error.to_string();
        assert!(message.contains("workspace"), "{message}");
    }

    /// Nor the rule inside a table. `[scope]` still rejects what it does not
    /// recognise: this is exactly where a silent mistake would send memory to
    /// the wrong project, and nothing about forward compatibility needs it
    /// relaxed.
    #[test]
    fn an_unknown_key_inside_a_known_table_is_still_refused() {
        let error = MarkerConfig::from_toml("[scope]\nprojekt = \"anamnesis\"\n")
            .expect_err("a typo inside a known table is still a typo");

        let message = error.to_string();
        assert!(message.contains("projekt"), "{message}");
    }

    /// A file with nothing unusual in it says so, so that `anamnesis status`
    /// stays silent on every machine whose binary keeps up with its marker.
    #[test]
    fn a_marker_this_build_understands_reports_nothing() {
        let config = MarkerConfig::from_toml("[scope]\nproject = \"anamnesis\"\n").expect("parses");

        assert!(config.unrecognized().is_empty());
    }

    /// The replies the measurement found, and the words a prompt with a
    /// subject spends. A version number is one word, not three.
    #[test]
    fn words_are_counted_the_way_a_reader_would() {
        assert_eq!(words_in("devam et"), 2);
        assert_eq!(words_in("onay"), 1);
        assert_eq!(words_in("  ...  "), 0);
        assert_eq!(words_in("tamamdır devam et"), 3);
        assert_eq!(words_in("v1.0.0 etiketini at"), 3);
        assert_eq!(words_in("nomic'i canlıya al"), 3);
    }

    /// Counted by spaces, a whole question in a script written without them
    /// is one word and would never be asked about.
    #[test]
    fn a_sentence_without_spaces_is_not_one_word() {
        let question = "如何部署到暂存环境";
        assert!(
            words_in(question) >= RecallConfig::default().min_words,
            "{}",
            words_in(question)
        );
    }

    #[test]
    fn a_project_can_ask_about_short_prompts_too() {
        assert_eq!(RecallConfig::default().min_words, 3);
        let config = MarkerConfig::from_toml("[recall]\nmin_words = 1\n").expect("parses");
        assert_eq!(config.recall.min_words, 1);
        assert_eq!(
            config.recall.min_similarity,
            RecallConfig::default().min_similarity,
            "setting one leaves the others at their defaults"
        );
    }
}
