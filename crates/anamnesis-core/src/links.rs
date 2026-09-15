//! Where a `[[wiki link]]` points.
//!
//! The index used to accept two spellings of a link, both measured from the
//! root of the scope: `[[gotchas/windows-bom]]` and `[[gotchas/windows-bom.md]]`.
//! That is not how anybody writes one. The wiki is meant to be read and edited
//! in Obsidian, and Obsidian resolves `[[windows-bom]]` to the one page of that
//! name wherever it is; an agent writing a page beside another in `gotchas/`
//! names its neighbour the same way. On the machine this project runs on, on
//! 2026-09-15, the index held eleven links it could not resolve, and eight of
//! them named a page that existed — all eight written from one page in
//! `gotchas/` to another. Nothing errored. The link-neighbour stream never saw
//! those edges, and `anamnesis improve` proposed writing
//! `the-model-lives-in-the-servers-environment-not-the-clis.md`, a page the
//! wiki had held for nine days.
//!
//! So a link is resolved the way the person who wrote it meant it, in this
//! order, and the first rule that finds a page wins:
//!
//! 1. **From the root**, as before. A link that already resolved keeps
//!    resolving to the same page; nothing that worked changes.
//! 2. **Beside the page that makes the link.** Two pages named `setup.md` in
//!    different folders are ordinary, and the one next to the link is the one
//!    its author was looking at.
//! 3. **By the end of its path, when exactly one page ends that way.** Two
//!    candidates and no neighbour among them is a link that does not say which
//!    page it means, and it is left unresolved rather than guessed: a wrong
//!    edge is worse than a missing one, because nothing reports it.
//!
//! A link may carry an alias (`[[page|what to show]]`) or a heading
//! (`[[page#section]]`); neither is part of the page's name, and both are set
//! aside before anything is looked up.
//!
//! The rules are here, with the lookups left to the caller, because two
//! callers apply them to different things — the index to its rows, the wiki
//! browser to the files — and the browser's live links are meant to be exactly
//! the edges retrieval sees.

/// The page extension every wiki page is stored with.
const EXTENSION: &str = ".md";

/// The part of a written link that names a page.
///
/// `[[windows-bom|the BOM trap]]` and `[[windows-bom#why]]` both name
/// `windows-bom`. A pipe escaped for a Markdown table (`\|`) is an alias too.
pub fn link_target(raw: &str) -> &str {
    let end = raw.find(['|', '#']).unwrap_or(raw.len());
    raw[..end].trim().trim_end_matches('\\').trim()
}

/// The last segment of a link target or page path, without its extension.
///
/// Every rule in [`resolve`] finds a page whose path ends in the same name the
/// link does, so this is what a caller can compare to decide whether a newly
/// written page could be what an unresolved link was waiting for.
pub fn link_stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_suffix(EXTENSION).unwrap_or(name)
}

/// Resolve a link written on the page at `from` to the path of the page it
/// names, by the rules in the module documentation.
///
/// The caller answers two questions about its pages: `exists`, whether a page
/// is stored at exactly this path, and `ending_with`, which pages have a path
/// that is this suffix or ends with `/` and this suffix. Closures rather than
/// a trait, so this crate stays free of anything shaped like storage.
pub fn resolve<E>(
    from: &str,
    raw: &str,
    mut exists: impl FnMut(&str) -> Result<bool, E>,
    mut ending_with: impl FnMut(&str) -> Result<Vec<String>, E>,
) -> Result<Option<String>, E> {
    let target = link_target(raw).trim_start_matches('/');
    if target.is_empty() {
        return Ok(None);
    }
    let spellings: Vec<String> = if target.ends_with(EXTENSION) {
        vec![target.to_owned()]
    } else {
        vec![target.to_owned(), format!("{target}{EXTENSION}")]
    };

    for spelling in &spellings {
        if exists(spelling)? {
            return Ok(Some(spelling.clone()));
        }
    }

    if let Some((folder, _)) = from.rsplit_once('/') {
        for spelling in &spellings {
            let beside = format!("{folder}/{spelling}");
            if exists(&beside)? {
                return Ok(Some(beside));
            }
        }
    }

    let mut ending = Vec::new();
    for spelling in &spellings {
        for path in ending_with(spelling)? {
            if !ending.contains(&path) {
                ending.push(path);
            }
        }
    }
    Ok(match ending.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;

    struct Pages(&'static [&'static str]);

    fn resolved(pages: &Pages, from: &str, raw: &str) -> Option<String> {
        resolve::<Infallible>(
            from,
            raw,
            |path| Ok(pages.0.contains(&path)),
            |suffix| {
                let slashed = format!("/{suffix}");
                Ok(pages
                    .0
                    .iter()
                    .filter(|path| **path == suffix || path.ends_with(&slashed))
                    .map(|path| (*path).to_owned())
                    .collect())
            },
        )
        .expect("infallible")
    }

    /// The live wiki's case: one gotcha naming its neighbour by name alone.
    #[test]
    fn a_page_named_by_its_name_alone_is_found_beside_the_link() {
        let pages = Pages(&[
            "gotchas/a-model-that-answers-a-ping-can-still-refuse-the-work.md",
            "gotchas/the-model-lives-in-the-servers-environment-not-the-clis.md",
        ]);
        assert_eq!(
            resolved(
                &pages,
                "gotchas/a-model-that-answers-a-ping-can-still-refuse-the-work.md",
                "the-model-lives-in-the-servers-environment-not-the-clis.md"
            )
            .as_deref(),
            Some("gotchas/the-model-lives-in-the-servers-environment-not-the-clis.md")
        );
    }

    /// The two spellings that resolved before still resolve, to the same page.
    #[test]
    fn a_path_from_the_root_resolves_as_it_always_did() {
        let pages = Pages(&["gotchas/windows-bom.md", "sessions/today.md"]);
        for raw in ["gotchas/windows-bom", "gotchas/windows-bom.md"] {
            assert_eq!(
                resolved(&pages, "sessions/today.md", raw).as_deref(),
                Some("gotchas/windows-bom.md"),
                "{raw}"
            );
        }
    }

    /// From anywhere, a name only one page has is that page.
    #[test]
    fn a_name_only_one_page_has_resolves_from_any_folder() {
        let pages = Pages(&["gotchas/windows-bom.md", "sessions/today.md"]);
        assert_eq!(
            resolved(&pages, "sessions/today.md", "windows-bom").as_deref(),
            Some("gotchas/windows-bom.md")
        );
        assert_eq!(
            resolved(&pages, "top.md", "windows-bom.md").as_deref(),
            Some("gotchas/windows-bom.md"),
            "from a page at the root, which has no folder to look beside"
        );
    }

    #[test]
    fn a_name_two_pages_share_resolves_beside_the_link_or_not_at_all() {
        let pages = Pages(&["gotchas/setup.md", "decisions/setup.md", "gotchas/x.md"]);
        assert_eq!(
            resolved(&pages, "gotchas/x.md", "setup").as_deref(),
            Some("gotchas/setup.md"),
            "the neighbour is the one its author was looking at"
        );
        assert_eq!(
            resolved(&pages, "sessions/today.md", "setup"),
            None,
            "two candidates and no neighbour: a wrong edge would be worse than none"
        );
    }

    /// A root page and a neighbour of the same name: the root wins, because
    /// that is what the link resolved to before this rule existed.
    #[test]
    fn what_resolved_before_is_not_moved_by_a_neighbour() {
        let pages = Pages(&["setup.md", "gotchas/setup.md"]);
        assert_eq!(
            resolved(&pages, "gotchas/x.md", "setup").as_deref(),
            Some("setup.md")
        );
    }

    #[test]
    fn a_partial_path_resolves_by_how_a_path_ends() {
        let pages = Pages(&["archive/gotchas/windows-bom.md", "sessions/today.md"]);
        assert_eq!(
            resolved(&pages, "sessions/today.md", "gotchas/windows-bom").as_deref(),
            Some("archive/gotchas/windows-bom.md")
        );
    }

    /// Ending with a name is a whole segment, not a string that happens to
    /// finish the same way.
    #[test]
    fn a_name_matches_whole_segments_only() {
        let pages = Pages(&["gotchas/not-windows-bom.md"]);
        assert_eq!(resolved(&pages, "sessions/today.md", "windows-bom"), None);
    }

    #[test]
    fn an_alias_and_a_heading_are_not_part_of_the_name() {
        let pages = Pages(&["gotchas/windows-bom.md"]);
        for raw in [
            "windows-bom|the BOM trap",
            "windows-bom#why it breaks",
            "windows-bom\\|in a table",
            " gotchas/windows-bom.md | spaced ",
        ] {
            assert_eq!(
                resolved(&pages, "sessions/today.md", raw).as_deref(),
                Some("gotchas/windows-bom.md"),
                "{raw}"
            );
        }
        assert_eq!(
            resolved(&pages, "sessions/today.md", "#only-a-heading"),
            None
        );
        assert_eq!(resolved(&pages, "sessions/today.md", "|"), None);
    }

    #[test]
    fn a_stem_is_the_last_segment_without_its_extension() {
        assert_eq!(link_stem("gotchas/windows-bom.md"), "windows-bom");
        assert_eq!(link_stem("windows-bom"), "windows-bom");
        assert_eq!(link_stem("a/b/c.md"), "c");
        assert_eq!(
            link_stem(link_target("gotchas/windows-bom|alias")),
            "windows-bom"
        );
    }
}
