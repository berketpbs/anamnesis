//! `anamnesis abstracts`: give an eval suite's pages abstracts, written by a
//! model that is shown each page and nothing else.
//!
//! A suite file holds its questions beside its pages, so whoever writes the
//! abstracts by hand has read what will be asked, and a line that names what a
//! question asks about is an answer placed where only the abstract stream can
//! see it. Asking a model page by page keeps the questions out of the one place
//! the line is made. See `anamnesis_consolidate::write_abstract`.
//!
//! Pages that already carry an abstract are left as they are, so a run cut
//! short — a daily quota, a network — is finished by running it again, and
//! nothing that was written is asked for twice.

use std::path::Path;

use anamnesis_consolidate::write_abstract;
use anamnesis_evals::Suite;

/// What one pass over a suite's pages did.
#[derive(Debug, Default, PartialEq, Eq)]
struct Filled {
    /// Pages given an abstract, with it.
    added: Vec<(String, String)>,
    /// Pages that already had one.
    kept: usize,
    /// Pages the model did not give a usable abstract, and why.
    refused: Vec<(String, String)>,
}

/// Give every `[[page]]` in a suite document without an abstract the one
/// `ask` writes from its title and body.
///
/// The key goes after `title`, where somebody reading the page looks for what
/// it is about, rather than after a body that can run to a hundred lines.
/// Everything else in the document — comments, the questions, key order — is
/// left as it was.
fn fill(
    document: &mut toml_edit::DocumentMut,
    mut ask: impl FnMut(&str, &str) -> Result<String, String>,
) -> Filled {
    let mut filled = Filled::default();
    let Some(pages) = document
        .get_mut("page")
        .and_then(|item| item.as_array_of_tables_mut())
    else {
        return filled;
    };

    for page in pages.iter_mut() {
        let text = |key: &str| {
            page.get(key)
                .and_then(|item| item.as_str())
                .unwrap_or_default()
                .to_owned()
        };
        let path = text("path");
        if !text("abstract").trim().is_empty() {
            filled.kept += 1;
            continue;
        }

        match ask(&text("title"), &text("body")) {
            Ok(line) => {
                page.insert("abstract", toml_edit::value(line.as_str()));
                page.sort_values_by(|a, _, b, _| rank(a.get()).cmp(&rank(b.get())));
                filled.added.push((path, line));
            }
            Err(reason) => filled.refused.push((path, reason)),
        }
    }
    filled
}

/// Where a key sits in a page table: `path`, `title`, `abstract`, then the rest
/// in the order they were written.
fn rank(key: &str) -> u8 {
    match key {
        "path" => 0,
        "title" => 1,
        "abstract" => 2,
        _ => 3,
    }
}

/// Write abstracts for a suite's pages.
pub fn cmd_abstracts(suite: &Path, write: bool) -> anyhow::Result<()> {
    let source = std::fs::read_to_string(suite)?;
    // Loaded as a suite first, so a file that is not one is refused before a
    // single request is spent on it.
    let parsed = Suite::from_toml(&source)?;
    let mut document: toml_edit::DocumentMut = source.parse()?;

    println!("📝 Abstracts for {} ({})", parsed.name, suite.display());

    let config = anamnesis_llm::LlmConfig::from_env()?;
    let Some(provider) = config.build()? else {
        println!();
        println!("  No model is configured in this shell, so nothing was asked.");
        println!("  Set ANAMNESIS_LLM_PROVIDER and its key here, or run this under");
        println!("  the same environment the server has.");
        return Ok(());
    };
    println!(
        "   model {} ({}), shown each page's title and body and nothing else",
        provider.model(),
        provider.name()
    );
    println!();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let filled = fill(&mut document, |title, body| {
        runtime.block_on(write_abstract(
            provider.as_ref(),
            title,
            body,
            config.max_output_tokens,
        ))
    });

    for (path, line) in &filled.added {
        println!("  + {path}");
        println!("      {line}");
    }
    for (path, reason) in &filled.refused {
        println!("  ✗ {path}");
        println!("      {reason}");
    }
    println!();
    println!(
        "  {} written, {} already had one, {} refused",
        filled.added.len(),
        filled.kept,
        filled.refused.len()
    );

    if filled.added.is_empty() {
        return Ok(());
    }
    if !write {
        println!();
        println!("  Nothing has been written. Re-run with --write to keep these.");
        println!("  (Each run asks again, and a model does not answer twice alike.)");
        return Ok(());
    }

    let updated = document.to_string();
    // The file is checked back in as a suite before it replaces the old one:
    // a document this command made unloadable would take every eval with it.
    Suite::from_toml(&updated)?;
    std::fs::write(suite, updated)?;
    println!();
    println!("  Written to {}.", suite.display());
    if !filled.refused.is_empty() {
        println!("  Run it again to ask for the pages that were refused.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUITE: &str = r#"# a suite, with a comment that has to survive
name = "tiny"
description = "two pages"

[[page]]
path = "notes/a.md"
title = "Relay clock"
tier = "semantic"
body = "The relay syncs at dawn."

[[page]]
path = "notes/b.md"
title = "Soil probe"
abstract = "Written by somebody already."
body = "The probe reads moisture."

[[case]]
query = "when does the relay sync"
relevant = ["notes/a.md"]
"#;

    /// The model is asked about the one page without an abstract, is shown
    /// that page's title and body, and never the question the suite asks.
    #[test]
    fn only_pages_without_an_abstract_are_asked_about_and_only_their_text_is_shown() {
        let mut document: toml_edit::DocumentMut = SUITE.parse().expect("toml");
        let mut shown = Vec::new();

        let filled = fill(&mut document, |title, body| {
            shown.push(format!("{title} | {body}"));
            Ok("How the relay keeps its clock.".to_owned())
        });

        assert_eq!(shown, ["Relay clock | The relay syncs at dawn."]);
        assert_eq!(filled.kept, 1);
        assert_eq!(
            filled.added,
            [(
                "notes/a.md".to_owned(),
                "How the relay keeps its clock.".to_owned()
            )]
        );
    }

    /// The line lands after the title, the comment and the questions are still
    /// there, and the document still loads as the suite it was.
    #[test]
    fn the_abstract_goes_after_the_title_and_the_rest_is_left_alone() {
        let mut document: toml_edit::DocumentMut = SUITE.parse().expect("toml");
        fill(&mut document, |_, _| {
            Ok("How the relay keeps its clock.".to_owned())
        });
        let written = document.to_string();

        assert!(
            written.contains(
                "title = \"Relay clock\"\nabstract = \"How the relay keeps its clock.\"\ntier = \"semantic\""
            ),
            "{written}"
        );
        assert!(written.starts_with("# a suite, with a comment that has to survive"));
        assert!(written.contains("query = \"when does the relay sync\""));
        let suite = Suite::from_toml(&written).expect("still a suite");
        assert_eq!(
            suite.pages[0].page_abstract,
            "How the relay keeps its clock."
        );
        assert_eq!(suite.pages[1].page_abstract, "Written by somebody already.");
    }

    /// A refusal leaves the page without an abstract, so the next run asks
    /// about it again, rather than storing whatever came back.
    #[test]
    fn a_refused_page_is_left_to_be_asked_again() {
        let mut document: toml_edit::DocumentMut = SUITE.parse().expect("toml");
        let filled = fill(&mut document, |_, _| Err("429 quota".to_owned()));

        assert_eq!(
            filled.refused,
            [("notes/a.md".to_owned(), "429 quota".to_owned())]
        );
        assert!(filled.added.is_empty());
        let suite = Suite::from_toml(&document.to_string()).expect("suite");
        assert!(suite.pages[0].page_abstract.is_empty());
    }
}
