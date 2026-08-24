//! Frontmatter-and-body document format.
//!
//! A page on disk is YAML frontmatter between `---` fences, then markdown:
//!
//! ```text
//! ---
//! title: Storage engine
//! tier: semantic
//! ---
//!
//! We chose SQLite because the index is rebuildable.
//! ```

use anamnesis_core::page::Frontmatter;

use crate::{Result, WikiError};

/// Fence that opens and closes the frontmatter block.
const FENCE: &str = "---";

/// A page read back from disk.
#[derive(Debug, Clone)]
pub struct ParsedPage {
    /// Metadata from the frontmatter block.
    pub frontmatter: Frontmatter,
    /// Markdown body following the frontmatter.
    pub body: String,
}

/// Render frontmatter and body into the on-disk document format.
pub fn render_document(frontmatter: &Frontmatter, body: &str) -> Result<String> {
    let yaml = serde_yaml::to_string(frontmatter).map_err(|error| WikiError::Malformed {
        path: frontmatter.title.clone(),
        reason: format!("frontmatter could not be serialized: {error}"),
    })?;

    let mut out = String::with_capacity(yaml.len() + body.len() + 16);
    out.push_str(FENCE);
    out.push('\n');
    out.push_str(&yaml);
    if !yaml.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(FENCE);
    out.push_str("\n\n");
    out.push_str(body.trim_start());
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Split a document into frontmatter and body.
///
/// A page without frontmatter is an error rather than a page with defaults:
/// silently inventing metadata would let a corrupted file reindex as a valid,
/// wrong page.
pub fn parse_document(path: &str, text: &str) -> Result<ParsedPage> {
    let malformed = |reason: &str| WikiError::Malformed {
        path: path.to_owned(),
        reason: reason.to_owned(),
    };

    // Tolerate a UTF-8 BOM and leading blank lines from hand editing.
    let text = text.trim_start_matches('\u{feff}');
    let rest = text
        .trim_start_matches(['\n', '\r'])
        .strip_prefix(FENCE)
        .ok_or_else(|| malformed("missing opening frontmatter fence"))?;
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
        .ok_or_else(|| malformed("opening fence must be alone on its line"))?;

    let (yaml, body) = split_at_closing_fence(rest)
        .ok_or_else(|| malformed("missing closing frontmatter fence"))?;

    let frontmatter: Frontmatter = serde_yaml::from_str(yaml)
        .map_err(|error| malformed(&format!("frontmatter is not valid: {error}")))?;

    Ok(ParsedPage {
        frontmatter,
        body: body.to_owned(),
    })
}

/// Extract `[[target]]` wikilinks from a page body, in the order they appear,
/// deduplicated by target.
///
/// A target is taken verbatim between the brackets and trimmed; whatever
/// resolves it to a page (or fails to) is the retrieval layer's job, not this
/// parser's.
pub fn extract_links(body: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else {
            break;
        };
        let target = after[..end].trim();
        if !target.is_empty() && !targets.iter().any(|t: &String| t == target) {
            targets.push(target.to_owned());
        }
        rest = &after[end + 2..];
    }
    targets
}

/// Find the closing fence and return the YAML before it and the body after.
fn split_at_closing_fence(rest: &str) -> Option<(&str, &str)> {
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == FENCE {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return Some((yaml, body.trim_start_matches(['\n', '\r'])));
        }
        offset += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::page::{Entity, PagePath, PageStatus, Tier};

    fn frontmatter() -> Frontmatter {
        let mut fm = Frontmatter::new(
            "Storage engine",
            vec![Entity::parse("sqlite").unwrap(), Entity::parse("fts5").unwrap()],
        )
        .unwrap();
        fm.tier = Tier::Semantic;
        fm.status = PageStatus::Active;
        fm.pinned = true;
        fm
    }

    #[test]
    fn a_rendered_document_parses_back_to_the_same_metadata() {
        let original = frontmatter();
        let text = render_document(&original, "Body text here.").unwrap();
        let parsed = parse_document("decisions/x.md", &text).unwrap();

        assert_eq!(parsed.frontmatter.title, original.title);
        assert_eq!(parsed.frontmatter.tier, Tier::Semantic);
        assert!(parsed.frontmatter.pinned);
        assert_eq!(parsed.frontmatter.entities.len(), 2);
        assert_eq!(parsed.body.trim(), "Body text here.");
    }

    #[test]
    fn documents_open_and_close_with_a_fence() {
        let text = render_document(&frontmatter(), "Body").unwrap();
        assert!(text.starts_with("---\n"));
        assert_eq!(text.matches("\n---\n").count(), 1);
    }

    #[test]
    fn a_body_containing_a_fence_is_not_truncated() {
        // Horizontal rules and nested frontmatter examples are ordinary markdown.
        let body = "Intro\n\n---\n\nSection after a horizontal rule.";
        let text = render_document(&frontmatter(), body).unwrap();
        let parsed = parse_document("x.md", &text).unwrap();
        assert!(parsed.body.contains("Section after a horizontal rule."));
    }

    #[test]
    fn missing_frontmatter_is_an_error_not_a_default() {
        let err = parse_document("x.md", "Just a body, no metadata.\n");
        assert!(err.is_err());
    }

    #[test]
    fn an_unterminated_block_is_rejected() {
        let err = parse_document("x.md", "---\ntitle: Broken\n\nbody without a closing fence\n");
        assert!(err.is_err());
    }

    #[test]
    fn invalid_metadata_is_rejected_rather_than_coerced() {
        // `../` in a supersedes path must not survive a round trip through disk.
        let text = "---\ntitle: Bad\nsupersedes: ../../etc/passwd.md\n---\n\nbody\n";
        assert!(parse_document("x.md", text).is_err());
    }

    #[test]
    fn a_bom_and_leading_blank_lines_are_tolerated() {
        let text = format!(
            "\u{feff}\n{}",
            render_document(&frontmatter(), "Body").unwrap()
        );
        let parsed = parse_document("x.md", &text).unwrap();
        assert_eq!(parsed.frontmatter.title, "Storage engine");
    }

    #[test]
    fn wikilinks_are_extracted_in_order_and_deduplicated() {
        let body = "See [[decisions/0001-storage.md]] and [[gotchas/windows-bom.md]], \
                     again [[decisions/0001-storage.md]].";
        assert_eq!(
            extract_links(body),
            vec![
                "decisions/0001-storage.md".to_owned(),
                "gotchas/windows-bom.md".to_owned(),
            ]
        );
    }

    #[test]
    fn a_body_with_no_links_extracts_nothing() {
        assert!(extract_links("Plain text, no brackets here.").is_empty());
    }

    #[test]
    fn an_unterminated_bracket_is_ignored_rather_than_panicking() {
        assert!(extract_links("broken [[link without a close").is_empty());
    }

    #[test]
    fn supersedes_round_trips_as_a_path() {
        let mut fm = frontmatter();
        fm.supersedes = Some(PagePath::parse("decisions/0001-old.md").unwrap());
        let text = render_document(&fm, "Body").unwrap();
        let parsed = parse_document("x.md", &text).unwrap();
        assert_eq!(
            parsed.frontmatter.supersedes.map(|p| p.as_str().to_owned()),
            Some("decisions/0001-old.md".to_owned())
        );
    }
}
