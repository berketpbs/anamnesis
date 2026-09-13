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
    /// Pages not asked about, because an earlier page met a limit that the
    /// rest would have met too.
    unasked: Vec<String>,
}

/// Why `ask` gave no abstract, and whether to go on asking.
struct Refusal {
    reason: String,
    /// The failure was a rate limit or an overloaded model. Every page after
    /// it would spend a request finding the same thing.
    stop: bool,
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
    mut ask: impl FnMut(&str, &str) -> Result<String, Refusal>,
) -> Filled {
    let mut filled = Filled::default();
    let Some(pages) = document
        .get_mut("page")
        .and_then(|item| item.as_array_of_tables_mut())
    else {
        return filled;
    };

    let mut stopped = false;
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
        if stopped {
            filled.unasked.push(path);
            continue;
        }

        match ask(&text("title"), &text("body")) {
            Ok(line) => {
                page.insert("abstract", toml_edit::value(line.as_str()));
                page.sort_values_by(|a, _, b, _| rank(a.get()).cmp(&rank(b.get())));
                filled.added.push((path, line));
            }
            Err(refusal) => {
                stopped = refusal.stop;
                filled.refused.push((path, refusal.reason));
            }
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
pub fn cmd_abstracts(suite: &Path, write: bool, pace: std::time::Duration) -> anyhow::Result<()> {
    let source = std::fs::read_to_string(suite)?;
    // Loaded as a suite first, so a file that is not one is refused before a
    // single request is spent on it.
    let parsed = Suite::from_toml(&source)?;
    let mut document: toml_edit::DocumentMut = source.parse()?;

    println!("📝 Abstracts for {} ({})", parsed.name, suite.display());

    // Without fallbacks: a set of abstracts is measured as one writer's, and a
    // stand-in quietly taking over half the requests would make it two.
    let config = anamnesis_llm::LlmConfig::from_vars(crate::settings::var)?.without_fallbacks();
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
    let mut asked = 0usize;
    let filled = fill(&mut document, |title, body| {
        if asked > 0 && !pace.is_zero() {
            std::thread::sleep(pace);
        }
        asked += 1;
        runtime
            .block_on(write_abstract(
                provider.as_ref(),
                title,
                body,
                config.max_output_tokens,
            ))
            .map_err(|error| Refusal {
                stop: error.is_transient(),
                reason: error.to_string(),
            })
    });

    for (path, line) in &filled.added {
        println!("  + {path}");
        println!("      {line}");
    }
    for (path, reason) in &filled.refused {
        println!("  ✗ {path}");
        println!("      {reason}");
    }
    if !filled.unasked.is_empty() {
        println!(
            "  … {} not asked: the model said to wait, and each would have spent a request hearing it again",
            filled.unasked.len()
        );
    }
    println!();
    println!(
        "  {} written, {} already had one, {} refused, {} not asked",
        filled.added.len(),
        filled.kept,
        filled.refused.len(),
        filled.unasked.len()
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
    if !filled.refused.is_empty() || !filled.unasked.is_empty() {
        println!("  Run it again to ask about the pages still without one.");
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

    /// The same suite with a third page, so there is something after a refusal.
    fn three_pages() -> toml_edit::DocumentMut {
        SUITE
            .replacen(
                "[[case]]",
                "[[page]]\npath = \"notes/c.md\"\ntitle = \"Mast\"\nbody = \"The mast is guyed.\"\n\n[[case]]",
                1,
            )
            .parse()
            .expect("toml")
    }

    /// A reply that was not an abstract leaves that page without one — so the
    /// next run asks again rather than storing it — and the next page is still
    /// asked, since a bad answer about one page says nothing about another.
    #[test]
    fn an_unusable_reply_leaves_the_page_to_be_asked_again_and_goes_on() {
        let mut document = three_pages();
        let mut asked = 0;
        let filled = fill(&mut document, |title, _| {
            asked += 1;
            if title == "Relay clock" {
                Err(Refusal {
                    reason: "a heading".to_owned(),
                    stop: false,
                })
            } else {
                Ok("What holds the mast up.".to_owned())
            }
        });

        assert_eq!(asked, 2);
        assert_eq!(
            filled.refused,
            [("notes/a.md".to_owned(), "a heading".to_owned())]
        );
        assert_eq!(filled.added.len(), 1);
        assert!(filled.unasked.is_empty());
        let suite = Suite::from_toml(&document.to_string()).expect("suite");
        assert!(suite.pages[0].page_abstract.is_empty());
    }

    /// The run this was found on: a per-minute limit of five, and nine pages
    /// each spending a request to be told so. A limit stops the run; the pages
    /// after it are reported as not asked, and running again finishes them.
    #[test]
    fn a_rate_limit_stops_the_run_and_names_what_was_not_asked() {
        let mut document = three_pages();
        let mut asked = 0;
        let filled = fill(&mut document, |_, _| {
            asked += 1;
            Err(Refusal {
                reason: "the model answered 429".to_owned(),
                stop: true,
            })
        });

        assert_eq!(asked, 1, "one request to learn about the limit, not two");
        assert_eq!(filled.refused.len(), 1);
        assert_eq!(filled.unasked, ["notes/c.md"]);
        assert_eq!(filled.kept, 1);
    }
}
