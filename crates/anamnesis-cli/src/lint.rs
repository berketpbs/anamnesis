//! What `anamnesis lint` answers: which pages in this wiki are not worth what
//! they cost to keep.
//!
//! `doctor` judges the machinery — whether the moments are wired, whether
//! results are arriving, whether the server is the build you think it is. This
//! judges the output. The two fail independently: capture can be perfect and
//! the wiki still full of pages that say nothing, and a wiki can look
//! reasonable while the capture behind it has been blind for weeks.
//!
//! The rules here were not chosen from a list of things one could check. Each
//! one fires on this project's own wiki, and the first one is the complaint
//! that started this work: a page written from a seven-hour session, 686
//! events long, that reads in three sentences.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anamnesis_core::datadir::DataDir;
use anamnesis_core::page::Tier;
use anamnesis_core::scope::resolve_scope;
use anamnesis_store::Store;
use anamnesis_wiki::Wiki;
use jiff::Timestamp;

/// Below this many characters a page is not thin, it is empty.
///
/// A frontmatter block and a heading come to about a hundred characters before
/// anything has been said. Two hundred is roughly one sentence past that.
const EMPTY_BODY_CHARS: usize = 200;

/// Characters of page a session's events have to earn.
///
/// Deliberately low: a page is a summary, not a transcript, and the ratio is
/// looking for pages that lost their session rather than pages that were
/// merely concise. At four characters per event, this project's largest
/// session — 686 events — would have to reach 2,700 characters, and the page
/// it actually produced was 1,594.
const CHARS_PER_EVENT: usize = 4;

/// Events below which the ratio says nothing.
///
/// A short session that produced a short page produced the right page. The
/// rule is about the sessions where the two disagree by an order of magnitude.
const RATIO_FLOOR_EVENTS: usize = 60;

/// Days after which an episodic page nobody has read is worth questioning.
const STALE_DAYS: f64 = 30.0;

/// How bad a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth knowing, not worth acting on today.
    Note,
    /// The page is costing more than it returns.
    Weak,
}

/// One judgement about one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// How bad it is.
    pub severity: Severity,
    /// Which rule fired.
    pub rule: &'static str,
    /// The page, by its project-relative path.
    pub page: String,
    /// What is wrong, in one line, with the numbers it was judged on.
    pub message: String,
}

/// One page, reduced to what the rules read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageFacts {
    /// Project-relative path.
    pub path: String,
    /// Title from the frontmatter.
    pub title: String,
    /// Which tier it was written at.
    pub tier: Tier,
    /// Characters of markdown, frontmatter excluded.
    pub body_chars: usize,
    /// Events recorded in the session that produced it, when it came from one.
    ///
    /// `None` for a page nobody's session wrote — a bootstrap page, or one
    /// somebody wrote by hand. Those are not judged on coverage: there is
    /// nothing they failed to cover.
    pub session_events: Option<usize>,
    /// How many times retrieval has handed this page to anybody.
    pub reads: u32,
    /// When it was last written.
    pub written_at: Timestamp,
}

/// Judge a wiki, worst first.
pub fn lint(pages: &[PageFacts], now: Timestamp) -> Vec<Finding> {
    let mut findings = Vec::new();

    for page in pages {
        findings.extend(judge_page(page, now));
    }
    findings.extend(judge_duplicate_titles(pages));

    findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.page.cmp(&b.page))
            .then_with(|| a.rule.cmp(b.rule))
    });
    findings
}

/// Everything one page can be judged on alone.
fn judge_page(page: &PageFacts, now: Timestamp) -> Vec<Finding> {
    let mut findings = Vec::new();

    if page.body_chars < EMPTY_BODY_CHARS {
        findings.push(Finding {
            severity: Severity::Weak,
            rule: "empty",
            page: page.path.clone(),
            message: format!(
                "{} characters of body — a page this short is a row in an index, not something \
                 a later session can act on",
                page.body_chars
            ),
        });
    } else if let Some(events) = page.session_events
        // Only the page that *is* the account of the session. A session also
        // leaves gotchas and decisions behind, and those are supposed to be
        // short: a gotcha is one claim, and judging it against the length of
        // the afternoon that produced it would report every good one as thin.
        // Run without this the rule fired six times on this wiki and was right
        // once.
        && page.tier == Tier::Episodic
        && events >= RATIO_FLOOR_EVENTS
        && page.body_chars < events * CHARS_PER_EVENT
    {
        // The complaint this command was written for, stated as a ratio: the
        // session happened, the page did not.
        findings.push(Finding {
            severity: Severity::Weak,
            rule: "thin",
            page: page.path.clone(),
            message: format!(
                "{} characters written from a session of {events} events — the work is not in \
                 the page",
                page.body_chars
            ),
        });
    }

    // Only episodic pages. A decision or a gotcha nobody has needed yet is
    // not stale, it is waiting, which is the whole point of writing it down.
    #[allow(clippy::cast_precision_loss)]
    let age_days = (now.as_second() - page.written_at.as_second()) as f64 / 86_400.0;
    if page.tier == Tier::Episodic && page.reads == 0 && age_days > STALE_DAYS {
        findings.push(Finding {
            severity: Severity::Note,
            rule: "stale",
            page: page.path.clone(),
            message: format!("{age_days:.0} days old and never read once"),
        });
    }

    findings
}

/// Pages that claim the same title.
///
/// Two pages with one title is one page nobody can cite: a search that ranks
/// both puts the reader in front of a choice they have no way to make.
fn judge_duplicate_titles(pages: &[PageFacts]) -> Vec<Finding> {
    let mut by_title: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for page in pages {
        let title = page.title.trim().to_lowercase();
        if title.is_empty() {
            continue;
        }
        by_title.entry(title).or_default().push(&page.path);
    }

    by_title
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .flat_map(|(title, paths)| {
            let shared = paths.join(", ");
            paths
                .into_iter()
                .map(|path| Finding {
                    severity: Severity::Note,
                    rule: "duplicate-title",
                    page: path.to_owned(),
                    message: format!("{shared} all claim the title {title:?}"),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Read this project's wiki and say what is wrong with it.
pub fn cmd_lint(data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let scope = resolve_scope(&cwd)?;
    let data = DataDir::resolve(data_dir)?;

    let store = Store::open(data.db_file())?;
    store.migrate()?;

    // Events per session, so a page can be judged against the session behind
    // it rather than against a length somebody picked.
    let mut events: BTreeMap<String, usize> = BTreeMap::new();
    for summary in store.recent_sessions(scope.project_id, 500)? {
        events.insert(
            summary.id.to_string(),
            usize::try_from(summary.observation_count).unwrap_or(0),
        );
    }

    let reads: BTreeMap<String, u32> = store
        .sweep_rows(scope.project_id)?
        .into_iter()
        .map(|row| (row.path.as_str().to_owned(), row.facts.access_count))
        .collect();

    let wiki = Wiki::open(data.wiki())?;
    let mut pages = Vec::new();
    for path in wiki.pages(&scope.scope)? {
        let parsed = wiki.read_page(&scope.scope, &path)?;
        let session = parsed.frontmatter.session.map(|id| id.to_string());
        pages.push(PageFacts {
            path: path.as_str().to_owned(),
            title: parsed.frontmatter.title.clone(),
            tier: parsed.frontmatter.tier,
            body_chars: parsed.body.trim().chars().count(),
            session_events: session.and_then(|id| events.get(&id).copied()),
            reads: reads.get(path.as_str()).copied().unwrap_or(0),
            written_at: written_at(&wiki, &scope.scope, &path),
        });
    }

    let findings = lint(&pages, Timestamp::now());

    println!("🧹 Anamnesis Wiki Lint");
    println!();
    println!("  Pages: {}", pages.len());
    println!("  Findings: {}", findings.len());
    println!();

    for finding in &findings {
        println!("  [{}] {}: {}", finding.rule, finding.page, finding.message);
    }

    if findings.is_empty() {
        println!("  Nothing to report.");
    }

    Ok(())
}

/// When a page was last written, from the file itself.
///
/// The modification time rather than the git log: this is a report about
/// content, it runs over every page, and asking git for each one's last commit
/// would turn a listing into a walk of the history per page. A file restored
/// from a backup reads as recently written, which overstates freshness — the
/// safe direction for a rule that only ever says "nobody has read this in a
/// month".
fn written_at(
    wiki: &Wiki,
    scope: &anamnesis_core::scope::Scope,
    path: &anamnesis_core::page::PagePath,
) -> Timestamp {
    std::fs::metadata(wiki.locate(scope, path))
        .and_then(|meta| meta.modified())
        .map(Timestamp::try_from)
        .and_then(|converted| {
            converted.map_err(|_| std::io::Error::other("timestamp out of range"))
        })
        .unwrap_or_else(|_| Timestamp::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(path: &str, body_chars: usize) -> PageFacts {
        PageFacts {
            path: path.to_owned(),
            title: path.to_owned(),
            tier: Tier::Episodic,
            body_chars,
            session_events: None,
            reads: 1,
            written_at: Timestamp::now(),
        }
    }

    /// The complaint this command exists for, as a number: a seven-hour
    /// session of 686 events summarised in 1,594 characters.
    #[test]
    fn a_page_that_lost_its_session_is_reported() {
        let facts = PageFacts {
            body_chars: 1_594,
            session_events: Some(686),
            ..page("sessions/2026-09-07-f7972247.md", 1_594)
        };

        let findings = lint(&[facts], Timestamp::now());

        assert_eq!(findings.len(), 1, "{findings:#?}");
        assert_eq!(findings[0].rule, "thin");
        assert!(findings[0].message.contains("686 events"), "{findings:#?}");
    }

    /// A short session that produced a short page produced the right page.
    /// The rule is about pages that lost their session, not concise ones.
    #[test]
    fn a_short_session_is_allowed_a_short_page() {
        let facts = PageFacts {
            body_chars: 300,
            session_events: Some(8),
            ..page("sessions/2026-09-08-911f8590.md", 300)
        };

        assert!(lint(&[facts], Timestamp::now()).is_empty());
    }

    /// A gotcha is one claim. It is written by a session, but it is not the
    /// account of that session, and judging it against the length of the
    /// afternoon behind it reports every good one as thin — which is what this
    /// rule did on the first run against this project's own wiki.
    #[test]
    fn a_durable_note_is_not_judged_against_the_session_that_wrote_it() {
        let facts = PageFacts {
            tier: Tier::Procedural,
            body_chars: 385,
            session_events: Some(279),
            ..page("gotchas/a-short-sharp-claim.md", 385)
        };

        assert!(lint(&[facts], Timestamp::now()).is_empty());
    }

    /// A page nobody's session wrote has no session to have lost. Bootstrap
    /// pages and hand-written ones are judged on what they say, not on a
    /// transcript they never had.
    #[test]
    fn a_page_without_a_session_is_not_judged_on_coverage() {
        let facts = PageFacts {
            body_chars: 533,
            session_events: None,
            ..page("bootstrap/contributors.md", 533)
        };

        assert!(lint(&[facts], Timestamp::now()).is_empty());
    }

    /// Below a certain length the page is not thin, it is absent — and it says
    /// so as `empty` rather than as a ratio, because the fix is different.
    #[test]
    fn an_all_but_empty_page_is_reported_as_empty_not_thin() {
        let facts = PageFacts {
            body_chars: 120,
            session_events: Some(400),
            ..page("sessions/2026-09-09-6cc63c1d.md", 120)
        };

        let findings = lint(&[facts], Timestamp::now());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "empty");
    }

    /// Two pages under one title is one page nobody can cite.
    #[test]
    fn pages_sharing_a_title_are_both_named() {
        let mut first = page("sessions/a.md", 900);
        let mut second = page("sessions/b.md", 900);
        first.title = "Fixing the parser".to_owned();
        second.title = "fixing the parser".to_owned();

        let findings = lint(&[first, second], Timestamp::now());

        assert_eq!(findings.len(), 2, "{findings:#?}");
        assert!(findings.iter().all(|f| f.rule == "duplicate-title"));
        assert!(findings[0].message.contains("sessions/a.md, sessions/b.md"));
    }

    /// A decision nobody has needed yet is not stale. It is waiting, which is
    /// what writing it down was for.
    #[test]
    fn only_episodic_pages_go_stale() {
        let old = Timestamp::now() - std::time::Duration::from_secs(60 * 86_400);
        let decision = PageFacts {
            tier: Tier::Semantic,
            reads: 0,
            written_at: old,
            ..page("decisions/one.md", 900)
        };
        let session = PageFacts {
            tier: Tier::Episodic,
            reads: 0,
            written_at: old,
            ..page("sessions/one.md", 900)
        };

        let findings = lint(&[decision, session], Timestamp::now());

        assert_eq!(findings.len(), 1, "{findings:#?}");
        assert_eq!(findings[0].rule, "stale");
        assert_eq!(findings[0].page, "sessions/one.md");
    }
}
