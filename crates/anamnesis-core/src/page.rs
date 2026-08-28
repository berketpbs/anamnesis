//! Wiki pages: their paths, frontmatter, and status.

use jiff::Timestamp;

use crate::error::{CoreError, Result};
use crate::ids::{PageId, ProjectId};

/// Longest permitted page path, in bytes.
pub const MAX_PATH_LEN: usize = 255;

/// Most entities one page may declare.
pub const MAX_ENTITIES: usize = 10;

/// Longest permitted entity, in bytes.
pub const MAX_ENTITY_LEN: usize = 64;

/// Wiki namespaces whose pages outrank ordinary ones during retrieval.
pub const AUTHORITY_NAMESPACES: [&str; 4] = ["_rules", "decisions", "procedures", "gotchas"];

/// A validated, project-relative page path such as `decisions/0001-storage.md`.
///
/// Paths become filesystem locations under the wiki root, so validation here is
/// a containment boundary: no page written through this type can escape its
/// project directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct PagePath(String);

impl PagePath {
    /// Validate a project-relative path.
    pub fn parse(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        let invalid = |reason: &'static str| CoreError::InvalidPagePath {
            path: value.to_owned(),
            reason,
        };

        if trimmed.is_empty() {
            return Err(invalid("must not be empty"));
        }
        if trimmed.len() > MAX_PATH_LEN {
            return Err(invalid("longer than 255 bytes"));
        }
        if trimmed.contains('\\') {
            return Err(invalid("must use forward slashes"));
        }
        if trimmed.starts_with('/') {
            return Err(invalid("must be relative to the project"));
        }
        if trimmed.contains(':') {
            return Err(invalid("must not contain a drive or scheme separator"));
        }
        if !trimmed.ends_with(".md") {
            return Err(invalid("must name a markdown file"));
        }
        if trimmed.chars().any(|c| c.is_control()) {
            return Err(invalid("must not contain control characters"));
        }

        for component in trimmed.split('/') {
            if component.is_empty() {
                return Err(invalid("must not contain empty path components"));
            }
            if component == "." || component == ".." {
                return Err(invalid("must not contain relative path components"));
            }
            if component.ends_with('.') || component.ends_with(' ') {
                return Err(invalid("components must not end with a dot or space"));
            }
        }

        Ok(Self(trimmed.to_owned()))
    }

    /// Borrow the path as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Leading directory component, if the page sits in one.
    pub fn namespace(&self) -> Option<&str> {
        self.0.split_once('/').map(|(head, _)| head)
    }

    /// Whether this page sits in a namespace that outranks ordinary pages.
    pub fn is_authoritative(&self) -> bool {
        self.namespace()
            .is_some_and(|ns| AUTHORITY_NAMESPACES.contains(&ns))
    }
}

impl std::fmt::Display for PagePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for PagePath {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// A canonical name a page is about.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct Entity(String);

impl Entity {
    /// Validate and normalize an entity name.
    pub fn parse(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        let invalid = |reason: &'static str| CoreError::InvalidName {
            kind: "entity",
            value: value.to_owned(),
            reason,
        };

        if trimmed.is_empty() {
            return Err(invalid("must not be empty"));
        }
        if trimmed.len() > MAX_ENTITY_LEN {
            return Err(invalid("longer than 64 bytes"));
        }
        if trimmed.chars().any(|c| c.is_control()) {
            return Err(invalid("must not contain control characters"));
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Borrow the entity as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Entity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Entity {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// Which temporal tier a page belongs to.
///
/// The tier is one bounded signal applied *after* relevance candidates are
/// generated — never an independent retriever and never an absolute override,
/// or a targeted search for something said in one session would be buried by
/// durable pages that merely outrank it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tier {
    /// The session in flight. Retained for forensics, excluded from recall.
    Working,
    /// One session's summary: what was touched, tried, and decided.
    #[default]
    Episodic,
    /// Distilled durable knowledge. Superseded rather than deleted.
    Semantic,
    /// A pattern seen often enough across episodes to be worth naming.
    Procedural,
}

impl Tier {
    /// Canonical lowercase identifier, as stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Procedural => "procedural",
        }
    }

    /// Recover a tier from its stored form, the inverse of [`Self::as_str`].
    ///
    /// Falls back to [`Self::Episodic`], the default tier, rather than
    /// erroring: the schema's `CHECK` means an unrecognised value cannot come
    /// from a database this code wrote.
    pub fn from_storage(raw: &str) -> Self {
        match raw {
            "working" => Self::Working,
            "semantic" => Self::Semantic,
            "procedural" => Self::Procedural,
            _ => Self::Episodic,
        }
    }

    /// Read a tier a person typed, refusing anything that is not one.
    ///
    /// The counterpart to [`Self::from_storage`], and deliberately stricter
    /// than it. A value out of the database has already passed a `CHECK` and
    /// can be defaulted safely; a value out of a command line or a tool call
    /// has not, and `--tier sematic` means something by it. Filing that page
    /// as episodic would put it where the decay sweep can reach it, which is
    /// the opposite of what was asked.
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "working" => Ok(Self::Working),
            "episodic" => Ok(Self::Episodic),
            "semantic" => Ok(Self::Semantic),
            "procedural" => Ok(Self::Procedural),
            other => Err(CoreError::InvalidName {
                kind: "tier",
                value: other.to_owned(),
                reason: "expected working, episodic, semantic, or procedural",
            }),
        }
    }

    /// Whether pages in this tier are offered to ordinary recall.
    pub fn is_recallable(&self) -> bool {
        !matches!(self, Self::Working)
    }

    /// Whether pages in this tier persist until superseded rather than decaying.
    pub fn is_durable(&self) -> bool {
        matches!(self, Self::Semantic | Self::Procedural)
    }
}

/// How much a page should be trusted when answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PageStatus {
    /// Current and safe to answer from.
    #[default]
    Active,
    /// Kept for the record; searchable but not authoritative.
    Historical,
    /// Known wrong. Retrievable so contradictions stay visible.
    DoNotAnswerFrom,
    /// Replaced by a newer page.
    Superseded,
}

impl PageStatus {
    /// Canonical lowercase identifier, as stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Historical => "historical",
            Self::DoNotAnswerFrom => "do-not-answer-from",
            Self::Superseded => "superseded",
        }
    }

    /// Read a status a person typed, refusing anything that is not one.
    ///
    /// Strict for the same reason [`Tier::parse`] is: `--status historic`
    /// would otherwise leave a page active, and the difference between those
    /// two is whether an agent answers from it.
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "historical" => Ok(Self::Historical),
            "do-not-answer-from" => Ok(Self::DoNotAnswerFrom),
            "superseded" => Ok(Self::Superseded),
            other => Err(CoreError::InvalidName {
                kind: "status",
                value: other.to_owned(),
                reason: "expected active, historical, do-not-answer-from, or superseded",
            }),
        }
    }

    /// Recover a status from its stored form, the inverse of [`Self::as_str`].
    ///
    /// Falls back to [`Self::Active`] for the same reason [`Tier::from_storage`]
    /// falls back to its default.
    pub fn from_storage(raw: &str) -> Self {
        match raw {
            "historical" => Self::Historical,
            "do-not-answer-from" => Self::DoNotAnswerFrom,
            "superseded" => Self::Superseded,
            _ => Self::Active,
        }
    }

    /// Whether an answer may be grounded in this page.
    pub fn is_answerable(&self) -> bool {
        matches!(self, Self::Active)
    }
}

/// YAML frontmatter carried at the top of every page.
///
/// This is the wiki-side view: everything here is authored, and the markdown
/// file is its source of truth. Index-side statistics that the wiki cannot
/// know — access counts, last-read times, whether a newer page has superseded
/// this one — live in the database instead, and are rebuilt, not restored.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Frontmatter {
    /// Human-facing title.
    pub title: String,
    /// Which temporal tier the page belongs to.
    pub tier: Tier,
    /// Trust level.
    pub status: PageStatus,
    /// Exempt from the decay sweep.
    pub pinned: bool,
    /// Declared the authoritative page on its subject.
    pub canonical: bool,
    /// Page this one replaces, if any.
    ///
    /// Supersession is recorded rather than the old page being deleted, so a
    /// later reader can still see what was believed before and why it changed.
    pub supersedes: Option<PagePath>,
    /// Importance assigned when written; the first term of the decay score.
    pub salience: f64,
    /// Canonical names this page is about.
    pub entities: Vec<Entity>,
    /// When the page should be forgotten.
    pub expires_at: Option<Timestamp>,
}

impl Default for Frontmatter {
    fn default() -> Self {
        Self {
            title: String::new(),
            tier: Tier::default(),
            status: PageStatus::default(),
            pinned: false,
            canonical: false,
            supersedes: None,
            salience: 1.0,
            entities: Vec::new(),
            expires_at: None,
        }
    }
}

impl Frontmatter {
    /// Build frontmatter for a page, enforcing the entity budget.
    ///
    /// Entities feed an inverse-frequency weighted retrieval stream; letting a
    /// page declare fifty of them would let one page dominate that stream.
    pub fn new(title: impl Into<String>, entities: Vec<Entity>) -> Result<Self> {
        if entities.len() > MAX_ENTITIES {
            return Err(CoreError::InvalidName {
                kind: "entity list",
                value: entities.len().to_string(),
                reason: "a page may declare at most 10 entities",
            });
        }
        Ok(Self {
            title: title.into(),
            entities,
            ..Self::default()
        })
    }

    /// Whether the page has passed its expiry at the given instant.
    pub fn is_expired_at(&self, now: Timestamp) -> bool {
        self.expires_at.is_some_and(|expiry| expiry <= now)
    }
}

/// A wiki page: frontmatter plus markdown body.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Page {
    /// Derived identifier.
    pub id: PageId,
    /// Project the page belongs to.
    pub project_id: ProjectId,
    /// Project-relative path.
    pub path: PagePath,
    /// Parsed frontmatter.
    pub frontmatter: Frontmatter,
    /// Markdown body, excluding frontmatter.
    pub body: String,
    /// Commit the page was last written in, once known.
    pub git_commit: Option<String>,
}

impl Page {
    /// Assemble a page, deriving its identifier from project and path.
    pub fn new(
        project_id: ProjectId,
        path: PagePath,
        frontmatter: Frontmatter,
        body: impl Into<String>,
    ) -> Self {
        Self {
            id: PageId::derive(project_id, &path),
            project_id,
            path,
            frontmatter,
            body: body.into(),
            git_commit: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_paths() {
        for good in [
            "decisions/0001-storage.md",
            "_rules/code-style.md",
            "notes.md",
            "a/b/c/deep.md",
        ] {
            assert!(PagePath::parse(good).is_ok(), "{good:?} should be accepted");
        }
    }

    #[test]
    fn rejects_escapes_and_absolutes() {
        for bad in [
            "../escape.md",
            "a/../../etc/passwd.md",
            "/abs/page.md",
            "C:/windows/page.md",
            "a\\b.md",
            "a//b.md",
            "page.txt",
            "",
            "   ",
        ] {
            assert!(PagePath::parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn authority_namespaces_are_recognised() {
        assert!(
            PagePath::parse("decisions/x.md")
                .unwrap()
                .is_authoritative()
        );
        assert!(PagePath::parse("_rules/x.md").unwrap().is_authoritative());
        assert!(!PagePath::parse("notes/x.md").unwrap().is_authoritative());
        assert!(!PagePath::parse("x.md").unwrap().is_authoritative());
    }

    #[test]
    fn entity_budget_is_enforced() {
        let entities: Vec<Entity> = (0..11)
            .map(|i| Entity::parse(&format!("entity-{i}")).unwrap())
            .collect();
        assert!(Frontmatter::new("Too many", entities).is_err());
    }

    #[test]
    fn entities_reject_oversized_names() {
        assert!(Entity::parse(&"x".repeat(65)).is_err());
        assert!(Entity::parse(&"x".repeat(64)).is_ok());
        assert!(Entity::parse("  spaced  ").unwrap().as_str() == "spaced");
    }

    #[test]
    fn expiry_is_evaluated_against_a_supplied_instant() {
        let now: Timestamp = "2026-08-19T00:00:00Z".parse().unwrap();
        let mut fm = Frontmatter::new("Page", Vec::new()).unwrap();
        assert!(!fm.is_expired_at(now));

        fm.expires_at = Some("2026-08-18T23:59:59Z".parse().unwrap());
        assert!(fm.is_expired_at(now));

        fm.expires_at = Some("2026-08-19T00:00:01Z".parse().unwrap());
        assert!(!fm.is_expired_at(now));
    }

    #[test]
    fn working_tier_is_kept_but_not_recalled() {
        assert!(!Tier::Working.is_recallable());
        assert!(Tier::Episodic.is_recallable());
    }

    #[test]
    fn durable_tiers_are_the_distilled_ones() {
        assert!(Tier::Semantic.is_durable());
        assert!(Tier::Procedural.is_durable());
        assert!(!Tier::Episodic.is_durable());
    }

    #[test]
    fn tier_round_trips_through_serde() {
        let json = serde_json::to_string(&Tier::Procedural).unwrap();
        assert_eq!(json, "\"procedural\"");
        assert_eq!(
            serde_json::from_str::<Tier>(&json).unwrap(),
            Tier::Procedural
        );
    }

    #[test]
    fn tier_round_trips_through_storage() {
        // The database column and the enum have to agree in both directions:
        // a variant whose `as_str` no longer parses back would silently
        // reindex every page under the wrong tier.
        for tier in [
            Tier::Working,
            Tier::Episodic,
            Tier::Semantic,
            Tier::Procedural,
        ] {
            assert_eq!(Tier::from_storage(tier.as_str()), tier);
        }
        assert_eq!(Tier::from_storage("nonsense"), Tier::default());
    }

    #[test]
    fn page_status_round_trips_through_storage() {
        for status in [
            PageStatus::Active,
            PageStatus::Historical,
            PageStatus::DoNotAnswerFrom,
            PageStatus::Superseded,
        ] {
            assert_eq!(PageStatus::from_storage(status.as_str()), status);
        }
        assert_eq!(PageStatus::from_storage("nonsense"), PageStatus::default());
    }

    #[test]
    fn supersession_names_the_page_it_replaces() {
        let mut fm = Frontmatter::new("Storage, revised", Vec::new()).unwrap();
        fm.status = PageStatus::Active;
        fm.supersedes = Some(PagePath::parse("decisions/0001-storage.md").unwrap());
        assert_eq!(
            fm.supersedes.as_ref().map(PagePath::as_str),
            Some("decisions/0001-storage.md")
        );
    }

    #[test]
    fn default_frontmatter_carries_full_salience() {
        assert!((Frontmatter::default().salience - 1.0).abs() < f64::EPSILON);
        assert_eq!(Frontmatter::default().tier, Tier::Episodic);
    }

    #[test]
    fn only_active_pages_are_answerable() {
        assert!(PageStatus::Active.is_answerable());
        assert!(!PageStatus::DoNotAnswerFrom.is_answerable());
        assert!(!PageStatus::Historical.is_answerable());
    }

    #[test]
    fn page_identity_follows_project_and_path() {
        let project = ProjectId::from_uuid(uuid::Uuid::nil());
        let path = PagePath::parse("decisions/x.md").unwrap();
        let page = Page::new(project, path.clone(), Frontmatter::default(), "body");
        assert_eq!(page.id, PageId::derive(project, &path));
    }

    /// The distinction between the two readers: a value out of the database
    /// has passed a CHECK and can be defaulted, a value a person typed has
    /// not. Filing `sematic` as episodic would put a page the author meant to
    /// keep where the decay sweep can reach it.
    #[test]
    fn a_typed_tier_is_refused_where_a_stored_one_is_defaulted() {
        assert_eq!(Tier::from_storage("sematic"), Tier::Episodic);
        assert!(Tier::parse("sematic").is_err());

        assert_eq!(Tier::parse("semantic").expect("tier"), Tier::Semantic);
        assert_eq!(Tier::parse("  SEMANTIC ").expect("tier"), Tier::Semantic);
    }

    #[test]
    fn a_typed_status_is_refused_where_a_stored_one_is_defaulted() {
        assert_eq!(PageStatus::from_storage("historic"), PageStatus::Active);
        assert!(PageStatus::parse("historic").is_err());

        assert_eq!(
            PageStatus::parse("do-not-answer-from").expect("status"),
            PageStatus::DoNotAnswerFrom
        );
    }

    /// The refusal has to say what would have been accepted, because the
    /// person reading it has just mistyped one of four words.
    #[test]
    fn the_refusal_lists_what_it_wanted() {
        let error = Tier::parse("durable").expect_err("refused").to_string();
        assert!(error.contains("procedural"), "{error}");

        let error = PageStatus::parse("stale").expect_err("refused").to_string();
        assert!(error.contains("do-not-answer-from"), "{error}");
    }
}
