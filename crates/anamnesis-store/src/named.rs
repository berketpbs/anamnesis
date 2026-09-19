//! Recall that needs no model: the pages a prompt names.
//!
//! [`Store::pages_like`] answers a prompt with the pages whose vector is near
//! it, which needs an embedder running beside the server. Without one, recall
//! said nothing at all — and running no model is a setup this system promises
//! to support, not a degraded one. The fused query cannot stand in: rank
//! fusion keeps positions and throws scores away, so a prompt about nothing the
//! project knows comes back with the same score at the top as one about its
//! centre.
//!
//! This answers from words, with a score that means something on its own. A
//! prompt names things; a word the project has written on one page names that
//! page, and a word it has written on half of them names nothing. So each word
//! of the prompt is weighed by how few of the project's pages carry it, and a
//! page is scored by the share of that weight it carries — its **coverage**,
//! from 0 to 1, comparable across prompts and projects in a way a rank never
//! is. A word the project has never written at all still counts against every
//! page, as much as a word on none of them would, which is what keeps a
//! question about somebody else's subject quiet even when its ordinary words
//! are all over this project's pages: a page has to carry more of what the
//! prompt names than the project is missing.
//!
//! Two kinds of word are read as more than their letters. A word shaped like a
//! name — `min_similarity`, `query.rs`, `v1.2.1`, `crates/anamnesis-store` —
//! is searched for as the phrase its parts make, because that is how the index
//! stores it, and it identifies something on its own. And a long word the
//! project has never written is tried again without its last letters, because
//! Turkish puts on the end of a word what English puts in front of it:
//! `değerini` is `değer` as much as `the value` is `value`.
//!
//! Like [`Store::pages_like`], nothing here records an access.

use std::collections::{HashMap, HashSet};

use anamnesis_core::ids::{PageId, ProjectId};
use anamnesis_core::retrieval::tokenize;
use rusqlite::params;

use crate::convert::parse_id;
use crate::query::PageHit;
use crate::{Result, Store};

/// The most words of a prompt that are weighed; the rest are ignored.
///
/// Each costs one full-text lookup, and a prompt is what someone typed: past
/// this, it is a paste, and what it is about is in its first words.
const MAX_TERMS: usize = 32;

/// How strict [`Store::pages_named_by`] is about what counts as naming a page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Naming {
    /// A word on more than this share of the project's pages is the project's
    /// vocabulary rather than a subject, and weighs nothing either way.
    pub common_share: f64,
    /// A word on at most this share of the pages — and never fewer than one
    /// page — identifies something on its own.
    pub rare_share: f64,
    /// The share of what the prompt names a page has to carry to be recalled.
    pub min_coverage: f64,
    /// Try a long word the project has never written again with its ending
    /// taken off.
    pub stems: bool,
    /// Let a session summary be recalled by the ordinary words of the prompt,
    /// and not only by a name it carries.
    ///
    /// Off by default, because a session summary is written in the words of
    /// the conversation it summarises: a page titled `nerede kalmıştık` is the
    /// echo of a prompt, and every later `nerede kalmıştık` names it exactly.
    /// On the live index this was measured on, 22 of the 23 blocks recall by
    /// naming gave to real prompts came from session pages, and nearly all of
    /// them were that echo. What a session is worth recalling for is what it
    /// touched — a file, a command, a version — and those are names. Decisions,
    /// notes and gotchas are written about a subject, and any word of it will
    /// do.
    pub episodes: bool,
}

impl Default for Naming {
    fn default() -> Self {
        Self {
            common_share: 0.2,
            rare_share: 0.05,
            min_coverage: 0.5,
            stems: true,
            episodes: false,
        }
    }
}

/// One word of a prompt, as it is looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Term {
    /// Tokens as the index stores them: one for a word, several for a name.
    parts: Vec<String>,
    /// Shaped like a name rather than a word.
    shaped: bool,
}

impl Term {
    /// The full-text expression that finds this term.
    fn expression(&self, prefix: bool) -> String {
        let quoted = format!("\"{}\"", self.parts.join(" ").replace('"', "\"\""));
        if prefix { format!("{quoted}*") } else { quoted }
    }
}

/// What the prompt says, as terms worth looking up.
fn prompt_terms(prompt: &str) -> Vec<Term> {
    let mut terms: Vec<Term> = Vec::new();
    for word in prompt.split_whitespace() {
        // A suffix after an apostrophe belongs to the word in front of it:
        // `anamnesis'in` is about `anamnesis`, and `don't` is about `don`.
        let word = word.split(['\'', '’']).next().unwrap_or(word);
        let parts = tokenize(word);
        let found = match parts.len() {
            0 => continue,
            1 => {
                let part = &parts[0];
                let has_digit = part.chars().any(|c| c.is_numeric());
                let has_letter = part.chars().any(char::is_alphabetic);
                if part.chars().count() < 3 && !(has_digit && has_letter) {
                    continue;
                }
                if is_function_word(part) {
                    continue;
                }
                vec![Term {
                    shaped: has_digit && has_letter,
                    parts,
                }]
            }
            _ => {
                // `e.g.` is two letters and a habit, not a name; `rrf_k` is a
                // name with a one-letter part.
                if parts.iter().all(|part| part.chars().count() < 2) {
                    continue;
                }
                vec![Term {
                    shaped: is_name_shaped(word),
                    parts,
                }]
            }
        };
        for term in found {
            if !terms.contains(&term) {
                terms.push(term);
            }
        }
    }
    terms.truncate(MAX_TERMS);
    terms
}

/// Words that hold a sentence together and name nothing, in the two languages
/// prompts here arrive in.
///
/// They are left out of a prompt altogether rather than weighed, because what
/// they would say is about the project's prose rather than its subjects: a
/// page that never happens to write `why` is no less an answer to a question
/// that starts with it. Every other word the project has never written counts
/// against every page — that is what keeps a question about something else
/// quiet — so this list is what stands between a plain question and a false
/// "this project knows nothing about it". Kept to words that carry no subject
/// in any project — `use`, `need`, `still` — and away from ones that are a
/// subject somewhere: `test`, `deploy`, `run` and `call` stay in.
#[rustfmt::skip]
const FUNCTION_WORDS: &[&str] = &[
    "able", "about", "acaba", "actually", "after", "again", "ait", "all", "also", "ama",
    "and", "any", "anything", "are", "aren", "artık", "bak", "bana", "bazı", "because",
    "been", "before", "being", "ben", "beni", "benim", "between", "bile", "bir", "biraz",
    "birkaç", "biz", "bizim", "both", "bunu", "bunun", "burada", "but", "can", "cannot",
    "come", "comes", "could", "daha", "değil", "did", "didn", "diye", "does", "doesn",
    "doing", "don", "done", "down", "during", "each", "even", "every", "everything", "evet",
    "eğer", "fakat", "few", "for", "from", "further", "gene", "get", "gets", "getting",
    "gibi", "give", "given", "going", "got", "göre", "had", "hadn", "hangi", "has", "hasn",
    "have", "haven", "having", "hayır", "hem", "hep", "her", "here", "hers", "him", "his",
    "hiç", "how", "ile", "into", "isn", "its", "için", "just", "kadar", "keep", "kept",
    "kind", "know", "knows", "let", "like", "look", "looks", "lot", "lots", "made", "make",
    "makes", "making", "many", "may", "maybe", "might", "misin", "more", "most", "much",
    "must", "mustn", "musun", "müsün", "mısın", "nasıl", "neden", "need", "needed", "needs",
    "neler", "nerde", "nerede", "nereden", "nereye", "nor", "not", "nothing", "now", "off",
    "olan", "olarak", "once", "only", "onu", "onun", "other", "our", "ours", "out", "over",
    "own", "please", "put", "really", "same", "sana", "see", "seem", "seems", "seen", "sen",
    "seni", "shall", "she", "should", "shouldn", "some", "something", "sort", "still",
    "such", "take", "tell", "than", "that", "the", "their", "them", "then", "there",
    "these", "they", "thing", "things", "think", "this", "those", "through", "told", "too",
    "took", "tried", "try", "under", "until", "upon", "use", "used", "uses", "using", "var",
    "very", "want", "wants", "was", "wasn", "way", "well", "went", "were", "weren", "what",
    "when", "where", "which", "while", "who", "whom", "whose", "why", "will", "with", "won",
    "would", "wouldn", "yani", "yap", "yet", "yine", "yok", "you", "your", "yours", "çok",
    "çünkü", "şey", "şimdi", "şu", "şunu", "şöyle",
];

fn is_function_word(word: &str) -> bool {
    FUNCTION_WORDS.contains(&word)
}

/// Whether a word joins its parts the way a name does.
///
/// A hyphen alone does not: `long-run` and `e-posta` are words that happen to
/// have one. An underscore, a dot, a slash, a colon, a hash or a backslash
/// between letters is how code, paths and versions are written.
fn is_name_shaped(word: &str) -> bool {
    let inner = word.trim_matches(|c: char| !c.is_alphanumeric());
    inner.contains(['_', '.', '/', ':', '#', '\\', '@'])
        || (inner.chars().any(char::is_numeric) && inner.chars().any(char::is_alphabetic))
}

/// How much a word says about which page it is on, by the number of pages
/// that carry it. BM25's form, which stays positive however common the word.
fn weight(pages: usize, of: usize) -> f64 {
    let pages = pages as f64;
    let of = of as f64;
    (1.0 + (of - pages + 0.5) / (pages + 0.5)).ln()
}

/// A term as it stands in this project.
#[derive(Debug, Clone)]
struct Weighed {
    term: Term,
    /// The pages that carry it.
    pages: HashSet<PageId>,
}

impl Store {
    /// The pages of a project a prompt names, best first, each scored by its
    /// coverage — the share of what the prompt names that the page carries.
    ///
    /// Empty when the prompt names nothing the project has written about,
    /// which is the answer most prompts should get. See the module
    /// documentation for how naming is judged.
    pub fn pages_named_by(
        &self,
        project_id: ProjectId,
        prompt: &str,
        limit: usize,
        naming: &Naming,
    ) -> Result<Vec<PageHit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let terms = prompt_terms(prompt);
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let standing = {
            let conn = self.connection();
            let project = project_id.to_string();
            // The working tier is the session in flight and never recalled.
            let live: usize = conn.query_row(
                "SELECT COUNT(*) FROM pages
                 WHERE project_id = ?1 AND is_latest = 1
                   AND status != 'superseded' AND tier != 'working'",
                params![project],
                |row| row.get::<_, i64>(0),
            )? as usize;
            if live == 0 {
                return Ok(Vec::new());
            }

            let mut statement = conn.prepare(
                "SELECT p.id FROM pages_fts
                 JOIN pages p ON p.rowid = pages_fts.rowid
                 WHERE pages_fts MATCH ?1
                   AND p.project_id = ?2 AND p.is_latest = 1
                   AND p.status != 'superseded' AND p.tier != 'working'",
            )?;
            let mut carrying = |expression: String| -> Result<HashSet<PageId>> {
                let rows = statement
                    .query_map(params![expression, project], |row| row.get::<_, String>(0))?;
                let mut found = HashSet::new();
                for row in rows {
                    found.insert(parse_id(row?));
                }
                Ok(found)
            };

            let episodes: HashSet<PageId> = if naming.episodes {
                HashSet::new()
            } else {
                let mut statement = conn.prepare(
                    "SELECT id FROM pages
                     WHERE project_id = ?1 AND is_latest = 1
                       AND status != 'superseded' AND tier = 'episodic'",
                )?;
                let rows = statement.query_map(params![project], |row| row.get::<_, String>(0))?;
                let mut found = HashSet::new();
                for row in rows {
                    found.insert(parse_id(row?));
                }
                found
            };

            let mut weighed = Vec::with_capacity(terms.len());
            for term in terms {
                let mut pages = carrying(term.expression(false))?;
                if pages.is_empty() && naming.stems && !term.shaped && term.parts.len() == 1 {
                    pages = stemmed(&term, &mut carrying)?;
                }
                weighed.push(Weighed { term, pages });
            }
            score_pages(&weighed, live, &episodes, naming)
        };

        let mut standing = standing;
        standing.truncate(limit);
        if standing.is_empty() {
            return Ok(Vec::new());
        }
        self.hits_for(standing)
    }
}

/// The pages a long word's shorter forms find, for a word the project has
/// never written as it stands.
///
/// Two to four letters off, never below five: enough for the endings Turkish
/// stacks on a stem, and not so many that what is left is the start of every
/// other word.
fn stemmed(
    term: &Term,
    carrying: &mut impl FnMut(String) -> Result<HashSet<PageId>>,
) -> Result<HashSet<PageId>> {
    let letters: Vec<char> = term.parts[0].chars().collect();
    if letters.len() < 7 {
        return Ok(HashSet::new());
    }
    for cut in 2..=4 {
        let keep = letters.len() - cut;
        if keep < 5 {
            break;
        }
        let stem = Term {
            parts: vec![letters[..keep].iter().collect()],
            shaped: false,
        };
        let pages = carrying(stem.expression(true))?;
        if !pages.is_empty() {
            return Ok(pages);
        }
    }
    Ok(HashSet::new())
}

/// Score every page some term finds, and keep the ones that clear the gate,
/// best first.
fn score_pages(
    weighed: &[Weighed],
    live: usize,
    names_only: &HashSet<PageId>,
    naming: &Naming,
) -> Vec<(PageId, f64)> {
    let common = ((live as f64 * naming.common_share).ceil() as usize).max(2);
    let rare = ((live as f64 * naming.rare_share).floor() as usize).max(1);
    let absent = weight(0, live);

    // What the prompt names, in total: every term that could tell pages
    // apart, and every term the project has never written — a subject this
    // project does not have is still what the prompt is about. Words that only
    // hold the sentence together never got this far; see `FUNCTION_WORDS`.
    //
    // Missing ordinary words are kept apart, because a page the prompt names
    // outright does not answer to them: `src/query.rs neden değişti` is about
    // that file whether or not any page happens to say `değişti`. A missing
    // name is never excused — a file or a version this project has never
    // written is exactly the sign of a question about somebody else's code.
    let mut named = 0.0;
    let mut missing_words = 0.0;
    for entry in weighed {
        let pages = entry.pages.len();
        if pages == 0 {
            named += absent;
            if !entry.term.shaped {
                missing_words += absent;
            }
        } else if pages <= common {
            named += weight(pages, live);
        }
    }
    if named <= 0.0 {
        return Vec::new();
    }

    let informative: Vec<&Weighed> = weighed
        .iter()
        .filter(|entry| !entry.pages.is_empty() && entry.pages.len() <= common)
        .collect();
    let mut carried: HashMap<PageId, (f64, usize, usize, bool)> = HashMap::new();
    for entry in &informative {
        let pages = entry.pages.len();
        let strong = entry.term.shaped || pages <= rare;
        for page in &entry.pages {
            let slot = carried.entry(*page).or_insert((0.0, 0, 0, false));
            slot.0 += weight(pages, live);
            slot.1 += 1;
            if strong {
                slot.2 += 1;
            }
            slot.3 |= entry.term.shaped;
        }
    }

    let mut standing: Vec<(PageId, f64)> = carried
        .into_iter()
        .filter(|(page, (_, matched, strong, shaped))| {
            // Something on the page has to identify it, and one ordinary word
            // is not a subject however rare it happens to be here — unless it
            // was the only thing the prompt could have meant. A session
            // summary has to be named outright; see `Naming::episodes`.
            *strong >= 1
                && (*matched >= 2 || *shaped || informative.len() == 1)
                && (*shaped || !names_only.contains(page))
        })
        .map(|(page, (carried, _, _, shaped))| {
            let of = if shaped { named - missing_words } else { named };
            (page, carried / of)
        })
        .filter(|(_, coverage)| *coverage >= naming.min_coverage)
        .collect();
    standing.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    standing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{fixture, fixture_now};
    use anamnesis_core::page::{Frontmatter, Page, PagePath as CorePagePath, Tier};

    fn write(store: &Store, project: ProjectId, path: &str, tier: Tier, body: &str) {
        let title = path
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .trim_end_matches(".md");
        let mut frontmatter = Frontmatter::new(title, Vec::new()).expect("frontmatter");
        frontmatter.tier = tier;
        let page = Page::new(
            project,
            CorePagePath::parse(path).expect("path"),
            frontmatter,
            body,
        );
        store.upsert_page(&page, fixture_now()).expect("upsert");
    }

    /// Twenty pages, each about one thing, all written in the same house
    /// vocabulary — `service`, `config`, `deploy` — the way a real project's
    /// pages share its nouns.
    fn corpus() -> (tempfile::TempDir, Store, ProjectId) {
        let (dir, store, project, _) = fixture();
        let subjects = [
            (
                "decisions/storage.md",
                "the index lives in sqlite, one file beside the service",
            ),
            (
                "decisions/queue.md",
                "kafka carries events between the service and the workers",
            ),
            (
                "decisions/retries.md",
                "retries back off with jitter so clients do not stampede",
            ),
            (
                "gotchas/redis-eviction.md",
                "redis evicts keys under memory pressure without warning",
            ),
            (
                "gotchas/cookie.md",
                "the session cookie needs samesite lax or the payment return loses it",
            ),
            (
                "notes/timeouts.md",
                "every outbound call gives up after thirty seconds",
            ),
            (
                "notes/rollback.md",
                "a bad release is rolled back by redeploying the previous tag",
            ),
            (
                "notes/flaky.md",
                "the clock test fails in ci because the runner is in utc",
            ),
            (
                "notes/secrets.md",
                "an env file must never be committed; the hook refuses it",
            ),
            (
                "notes/değer.md",
                "varsayılan değer config dosyasında tutulur, kodda değil",
            ),
            (
                "procedures/handover.md",
                "at shift end the on call writes what is still open",
            ),
            (
                "procedures/migrations.md",
                "a migration expands first and contracts a release later",
            ),
        ];
        for (path, body) in subjects {
            write(
                &store,
                project,
                path,
                Tier::Semantic,
                &format!("{body}. See the service config before a deploy."),
            );
        }
        for n in 0..6 {
            write(
                &store,
                project,
                &format!("notes/filler-{n}.md"),
                Tier::Semantic,
                "general notes on the service, its config and how a deploy goes",
            );
        }
        write(
            &store,
            project,
            "sessions/2026-09-03-a1b2c3d4.md",
            Tier::Episodic,
            "nerede kalmıştık diye soruldu. src/query.rs düzenlendi, cargo test geçti.",
        );
        write(
            &store,
            project,
            "sessions/2026-09-04-e5f6a7b8.md",
            Tier::Episodic,
            "devam edildi; the service config was read and nothing changed.",
        );
        (dir, store, project)
    }

    fn named(store: &Store, project: ProjectId, prompt: &str) -> Vec<String> {
        store
            .pages_named_by(project, prompt, 3, &Naming::default())
            .expect("named")
            .into_iter()
            .map(|hit| hit.path.to_string())
            .collect()
    }

    #[test]
    fn a_prompt_naming_a_subject_finds_its_page() {
        let (_dir, store, project) = corpus();
        assert_eq!(
            named(&store, project, "why do we use kafka")[0],
            "decisions/queue.md"
        );
        assert_eq!(
            named(&store, project, "payment return loses the cookie")[0],
            "gotchas/cookie.md"
        );
    }

    #[test]
    fn a_subject_the_project_never_wrote_about_is_left_alone() {
        let (_dir, store, project) = corpus();
        // Every ordinary word here is on this project's pages; the subject is
        // not.
        assert!(named(&store, project, "how do I deploy the service to kubernetes").is_empty());
        assert!(named(&store, project, "what is the weather in istanbul").is_empty());
        // One word the project has on a single page does not make a prompt
        // about it when the rest of the prompt is about something else.
        assert!(named(&store, project, "jitter in the audio mixer latency").is_empty());
    }

    #[test]
    fn a_prompt_of_only_house_words_names_nothing() {
        let (_dir, store, project) = corpus();
        assert!(named(&store, project, "check the service config").is_empty());
    }

    #[test]
    fn a_session_is_recalled_by_what_it_touched_and_not_by_what_was_said() {
        let (_dir, store, project) = corpus();
        assert!(named(&store, project, "nerede kalmıştık").is_empty());
        assert!(named(&store, project, "devam edelim").is_empty());
        assert_eq!(
            named(&store, project, "src/query.rs neden değişti")[0],
            "sessions/2026-09-03-a1b2c3d4.md"
        );

        let episodes = Naming {
            episodes: true,
            ..Naming::default()
        };
        let hits = store
            .pages_named_by(project, "nerede kalmıştık", 3, &episodes)
            .expect("named");
        assert_eq!(hits[0].path.as_str(), "sessions/2026-09-03-a1b2c3d4.md");
    }

    #[test]
    fn a_name_the_project_never_wrote_is_never_excused() {
        let (_dir, store, project) = corpus();
        // A word the pages happen not to use is let off beside a name they
        // carry…
        assert_eq!(
            named(&store, project, "src/query.rs değişti")[0],
            "sessions/2026-09-03-a1b2c3d4.md"
        );
        // …but a name they do not carry is somebody else's code.
        assert!(named(&store, project, "kafka lag in src/billing.rs").is_empty());
    }

    #[test]
    fn a_turkish_word_finds_its_stem() {
        let (_dir, store, project) = corpus();
        assert_eq!(
            named(&store, project, "varsayılan değerini nereye")[0],
            "notes/değer.md"
        );
    }

    #[test]
    fn coverage_is_a_share() {
        let (_dir, store, project) = corpus();
        let hits = store
            .pages_named_by(project, "sqlite kafka jitter", 3, &Naming::default())
            .expect("named");
        for hit in &hits {
            assert!(hit.score > 0.0 && hit.score <= 1.0, "{}", hit.score);
        }
        let hits = store
            .pages_named_by(project, "sqlite", 3, &Naming::default())
            .expect("named");
        assert_eq!(hits.len(), 1);
        assert!((hits[0].score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn nothing_is_recorded_as_read() {
        let (_dir, store, project) = corpus();
        assert!(!named(&store, project, "kafka").is_empty());
        let reads: i64 = store
            .connection()
            .query_row("SELECT SUM(access_count) FROM pages", [], |row| row.get(0))
            .expect("reads");
        assert_eq!(reads, 0);
    }

    #[test]
    fn a_limit_of_nothing_or_a_prompt_of_nothing_asks_nothing() {
        let (_dir, store, project) = corpus();
        let naming = Naming::default();
        assert!(
            store
                .pages_named_by(project, "kafka", 0, &naming)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .pages_named_by(project, "  ", 3, &naming)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .pages_named_by(project, "is it, or not?", 3, &naming)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn function_words_are_dropped_before_anything_is_weighed() {
        assert!(prompt_terms("why is it not there and where was it").is_empty());
        assert!(prompt_terms("neden bunu için nasıl").is_empty());
    }

    fn parts(terms: &[Term]) -> Vec<String> {
        terms.iter().map(|term| term.parts.join(" ")).collect()
    }

    #[test]
    fn a_name_is_looked_up_as_the_phrase_its_parts_make() {
        let terms = prompt_terms("why does min_similarity block query.rs in v1.2.1?");
        assert_eq!(
            parts(&terms),
            ["min similarity", "block", "query rs", "v1 2 1"]
        );
        let shaped: Vec<bool> = terms.iter().map(|term| term.shaped).collect();
        assert_eq!(shaped, [true, false, true, true]);
    }

    #[test]
    fn a_suffix_after_an_apostrophe_is_dropped_and_short_words_are_skipped() {
        let terms = prompt_terms("anamnesis'in rrf_k değeri bu mu, e.g. v2?");
        assert_eq!(parts(&terms), ["anamnesis", "rrf k", "değeri", "v2"]);
    }

    #[test]
    fn a_hyphenated_word_is_a_phrase_but_not_a_name() {
        let terms = prompt_terms("the long-run eval");
        assert_eq!(parts(&terms), ["long run", "eval"]);
        assert!(!terms[0].shaped);
    }

    #[test]
    fn a_word_on_fewer_pages_weighs_more() {
        assert!(weight(1, 100) > weight(10, 100));
        assert!(weight(0, 100) > weight(1, 100));
        assert!(weight(100, 100) > 0.0);
    }
}
