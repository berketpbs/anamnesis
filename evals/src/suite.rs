//! What an eval suite is, and where one comes from.
//!
//! A suite is a corpus and a set of questions asked of it. Both are checked in
//! as text, because a score is only worth reading if the thing it scored is
//! the same on every machine — an eval whose corpus is whatever happens to be
//! in someone's real memory measures their week, not the retrieval code.

use anamnesis_core::page::{Entity, PagePath, Tier};
use serde::Deserialize;

use crate::EvalError;

/// A corpus, the questions asked of it, and the bar it has to clear.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    /// Short name, printed with the results.
    pub name: String,
    /// What this suite is trying to find out.
    pub description: String,
    /// How many results a case is scored over.
    ///
    /// Not a detail: an agent reads the first few hits and stops, so a page
    /// ranked eleventh is a page nobody sees. The default matches what
    /// `memory_query` returns when it is not told otherwise.
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// The bar this suite has to clear for `--check` to pass.
    #[serde(default)]
    pub thresholds: Thresholds,
    /// The pages the questions are asked of.
    #[serde(default, rename = "page")]
    pub pages: Vec<FixturePage>,
    /// The questions.
    #[serde(default, rename = "case")]
    pub cases: Vec<Case>,
}

/// Results a suite is expected to reach.
///
/// Checked in beside the cases, so a change that costs the system recall has
/// to say so in the diff rather than in a number nobody looks at.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Thresholds {
    /// Lowest acceptable mean reciprocal rank.
    pub min_mrr: f64,
    /// Lowest acceptable share of cases whose answer appears at all.
    pub min_recall: f64,
}

impl Default for Thresholds {
    /// Zero: a suite that states no bar cannot fail one. Declaring the numbers
    /// is how a suite opts into being a gate, and every suite in this repo
    /// does.
    fn default() -> Self {
        Self {
            min_mrr: 0.0,
            min_recall: 0.0,
        }
    }
}

/// One page of a suite's corpus.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixturePage {
    /// Project-relative path, e.g. `decisions/0001-sqlite.md`.
    pub path: String,
    /// Frontmatter title.
    pub title: String,
    /// Markdown body, wikilinks and all.
    pub body: String,
    /// Canonical names the page is about.
    #[serde(default)]
    pub entities: Vec<String>,
    /// Temporal tier. Defaults to `episodic`, as a session page is.
    #[serde(default)]
    pub tier: String,
    /// Declared authoritative on its subject.
    #[serde(default)]
    pub canonical: bool,
    /// Exempt from decay.
    #[serde(default)]
    pub pinned: bool,
}

impl FixturePage {
    /// The validated path this page is written to.
    pub fn page_path(&self) -> Result<PagePath, EvalError> {
        PagePath::parse(&self.path).map_err(EvalError::from)
    }

    /// The validated entities this page declares.
    pub fn parsed_entities(&self) -> Result<Vec<Entity>, EvalError> {
        self.entities
            .iter()
            .map(|name| Entity::parse(name).map_err(EvalError::from))
            .collect()
    }

    /// The tier this page belongs to.
    ///
    /// An unrecognised name is refused rather than defaulted. A suite that
    /// says `semantik` means something by it, and quietly filing the page as
    /// episodic would change what the case is measuring without saying so.
    pub fn parsed_tier(&self) -> Result<Tier, EvalError> {
        if self.tier.trim().is_empty() {
            return Ok(Tier::Episodic);
        }
        match self.tier.trim().to_ascii_lowercase().as_str() {
            "working" => Ok(Tier::Working),
            "episodic" => Ok(Tier::Episodic),
            "semantic" => Ok(Tier::Semantic),
            "procedural" => Ok(Tier::Procedural),
            other => Err(EvalError::Suite(format!(
                "unknown tier {other:?} on {}; expected working, episodic, semantic, or procedural",
                self.path
            ))),
        }
    }
}

/// One question, and the pages that would answer it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    /// What someone would type.
    pub query: String,
    /// Paths that answer it, best first.
    pub relevant: Vec<String>,
    /// Why this case is here, for whoever reads a failure.
    #[serde(default)]
    pub note: String,
}

/// Results are scored over this many hits when a suite does not say.
fn default_limit() -> usize {
    5
}

impl Suite {
    /// Parse a suite from TOML.
    pub fn from_toml(source: &str) -> Result<Self, EvalError> {
        let suite: Self =
            toml_edit::de::from_str(source).map_err(|error| EvalError::Suite(error.to_string()))?;
        suite.validate()?;
        Ok(suite)
    }

    /// Read a suite from a file.
    pub fn load(path: &std::path::Path) -> Result<Self, EvalError> {
        let source = std::fs::read_to_string(path).map_err(|error| {
            EvalError::Suite(format!("could not read {}: {error}", path.display()))
        })?;
        Self::from_toml(&source)
    }

    /// Refuse a suite that cannot mean what it says.
    ///
    /// All three of these are typos whose silent reading is a score that looks
    /// fine: a case naming a page the corpus does not contain can never be
    /// answered, and would drag the suite down forever while looking like a
    /// retrieval failure.
    fn validate(&self) -> Result<(), EvalError> {
        if self.pages.is_empty() {
            return Err(EvalError::Suite(format!(
                "suite {:?} has no pages to search",
                self.name
            )));
        }
        if self.cases.is_empty() {
            return Err(EvalError::Suite(format!(
                "suite {:?} asks no questions",
                self.name
            )));
        }
        if self.limit == 0 {
            return Err(EvalError::Suite(format!(
                "suite {:?} scores over zero results",
                self.name
            )));
        }

        for page in &self.pages {
            page.page_path()?;
            page.parsed_entities()?;
            page.parsed_tier()?;
        }

        let paths: Vec<&str> = self.pages.iter().map(|page| page.path.as_str()).collect();
        for case in &self.cases {
            if case.relevant.is_empty() {
                return Err(EvalError::Suite(format!(
                    "case {:?} names no relevant page",
                    case.query
                )));
            }
            for wanted in &case.relevant {
                if !paths.contains(&wanted.as_str()) {
                    return Err(EvalError::Suite(format!(
                        "case {:?} expects {wanted:?}, which the corpus does not contain",
                        case.query
                    )));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
name = "tiny"
description = "one page, one question"

[[page]]
path = "decisions/0001-sqlite.md"
title = "Why SQLite"
body = "We chose SQLite."

[[case]]
query = "which database"
relevant = ["decisions/0001-sqlite.md"]
"#;

    #[test]
    fn a_minimal_suite_parses_with_its_defaults() {
        let suite = Suite::from_toml(MINIMAL).expect("parse");
        assert_eq!(suite.limit, 5);
        assert_eq!(suite.pages.len(), 1);
        assert_eq!(suite.cases.len(), 1);
        assert_eq!(suite.pages[0].parsed_tier().expect("tier"), Tier::Episodic);
        // A suite that states no bar cannot fail one.
        assert_eq!(suite.thresholds.min_mrr, 0.0);
    }

    /// The failure this check exists for: the case can never be answered, and
    /// the suite reports it as a retrieval miss forever.
    #[test]
    fn a_case_expecting_a_page_the_corpus_lacks_is_refused() {
        let source = MINIMAL.replace(
            "decisions/0001-sqlite.md\"]",
            "decisions/0002-postgres.md\"]",
        );
        let error = Suite::from_toml(&source).expect_err("should refuse");
        assert!(error.to_string().contains("does not contain"), "{error}");
    }

    #[test]
    fn a_misspelled_tier_is_refused_rather_than_defaulted() {
        let source = MINIMAL.replace(
            "title = \"Why SQLite\"",
            "title = \"Why SQLite\"\ntier = \"semantik\"",
        );
        let error = Suite::from_toml(&source).expect_err("should refuse");
        assert!(error.to_string().contains("unknown tier"), "{error}");
    }

    #[test]
    fn a_suite_with_no_questions_is_refused() {
        let source = MINIMAL.split("[[case]]").next().expect("prefix").to_owned();
        let error = Suite::from_toml(&source).expect_err("should refuse");
        assert!(error.to_string().contains("asks no questions"), "{error}");
    }

    /// Unknown keys are an error here for the same reason they are in the
    /// marker file: a misspelled `relevent` would silently score nothing.
    #[test]
    fn an_unknown_key_is_refused() {
        let source = MINIMAL.replace("relevant = ", "relevent = ");
        assert!(Suite::from_toml(&source).is_err());
    }
}
