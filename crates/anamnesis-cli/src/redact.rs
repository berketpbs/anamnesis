//! `anamnesis redact`: today's redaction rules, over what is already stored.
//!
//! Capture redacts once, with the rules it has. When a rule is added — and
//! they are added when a secret is found to have gone through — nothing ever
//! applies it to what was captured before. On the machine this project is
//! developed on, an AI Studio key typed into a prompt on 2026-09-02 went
//! through the rules of the day, and was still in the raw spool, the index and
//! every backup eleven days later, while every check of new captures showed the
//! rule working.
//!
//! This runs the current rules over every stored observation — the spool
//! files and the index rows — and reports, per rule, how many records carry
//! something the rule masks. It never prints a value. `--apply` rewrites them.
//!
//! What it does not rewrite is said instead of done. Wiki pages are a git
//! repository whose history keeps every version, so a page holding a secret is
//! named for a person to deal with (`anamnesis forget`, or an edit, and the
//! history if it matters). Backups are archives taken before the rewrite and
//! still hold the originals; they are listed, not deleted.

use std::path::{Path, PathBuf};

use anamnesis_core::audit::Action;
use anamnesis_core::datadir::DataDir;
use anamnesis_core::sanitize::Redactor;
use anamnesis_store::{RawSpool, Redaction, Store};

use crate::audit::note;

/// `anamnesis redact [--apply]`.
pub fn cmd_redact(apply: bool, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let data = DataDir::resolve(data_dir)?;
    let redactor = Redactor::new();

    println!(
        "🧹 {} with today's redaction rules — {}",
        if apply { "Redacting" } else { "Checking" },
        data.root().display()
    );
    println!();

    // The spool first: it is the copy that outlives the index, and a rewrite
    // interrupted between the two leaves the durable copy clean.
    let spool = RawSpool::new(data.raw());
    let mut raw = Redaction::default();
    let mut raw_files = 0usize;
    let mut refused = Vec::new();
    for path in spool.files()? {
        match spool.redact_file(&path, &redactor, apply) {
            Ok(found) => {
                if found.changed > 0 {
                    raw_files += 1;
                }
                raw.absorb(&found);
            }
            Err(error) => refused.push(format!("{}: {error}", path.display())),
        }
    }
    print_line(
        "raw spool",
        &raw,
        &format!("line(s) in {raw_files} file(s)"),
    );

    let mut index = Redaction::default();
    if data.db_file().exists() {
        let store = Store::open(data.db_file())?;
        store.migrate()?;
        index = store.redact_observations(&redactor, apply)?;
        if apply && (raw.changed > 0 || index.changed > 0) {
            note(
                &store,
                None,
                Action::Redacted,
                data.root().display().to_string(),
                Some(format!(
                    "{} spool line(s), {} index row(s): {}",
                    raw.changed,
                    index.changed,
                    describe_rules(&merged(&raw, &index))
                )),
            );
        }
    }
    print_line("index", &index, "observation row(s)");

    let (pages, page_rules) = scan_wiki(&data.wiki(), &redactor);
    if pages.is_empty() {
        println!("  {:<10} nothing", "wiki");
    } else {
        println!(
            "  {:<10} {} page(s): {} — not rewritten: the wiki is a git repository and \
             its history keeps every version",
            "wiki",
            pages.len(),
            describe_rules(&page_rules)
        );
        for page in &pages {
            println!("             {}", page.display());
        }
    }

    for problem in &refused {
        println!("  ✗ {problem}");
    }

    println!();
    if raw.changed == 0 && index.changed == 0 {
        println!("  Nothing stored holds anything today's rules would mask.");
    } else if apply {
        println!("  Rewritten. Every value is gone from the spool and the index.");
        let backups = backups(&data);
        if !backups.is_empty() {
            println!();
            println!(
                "  {} backup(s) were taken before this and still hold the originals.",
                backups.len()
            );
            println!(
                "  Take a new one with `anamnesis backup`, then delete these if nothing else needs them:"
            );
            for backup in backups {
                println!("    {}", backup.display());
            }
        }
    } else {
        println!("  Nothing has been written. Run with --apply to mask them.");
    }
    Ok(())
}

fn print_line(label: &str, found: &Redaction, unit: &str) {
    if found.changed == 0 {
        println!("  {label:<10} nothing, of {} examined", found.examined);
    } else {
        println!(
            "  {label:<10} {} {unit}: {}",
            found.changed,
            describe_rules(&found.rules)
        );
    }
}

fn describe_rules(rules: &std::collections::BTreeMap<&'static str, usize>) -> String {
    rules
        .iter()
        .map(|(rule, count)| format!("{rule} ×{count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn merged(one: &Redaction, two: &Redaction) -> std::collections::BTreeMap<&'static str, usize> {
    let mut total = Redaction::default();
    total.absorb(one);
    total.absorb(two);
    total.rules
}

/// Wiki pages holding something the rules mask, and per-rule page counts.
fn scan_wiki(
    root: &Path,
    redactor: &Redactor,
) -> (
    Vec<PathBuf>,
    std::collections::BTreeMap<&'static str, usize>,
) {
    let mut pages = Vec::new();
    let mut rules = Redaction::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != ".git") {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|ext| ext == "md")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                let redacted = redactor.redact(&text);
                if !redacted.is_clean() {
                    rules.count(redacted.hits());
                    pages.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
                }
            }
        }
    }
    pages.sort();
    (pages, rules.rules)
}

/// Archives in the data directory's `backups/`, which predate any rewrite.
fn backups(data: &DataDir) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(data.root().join("backups"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.to_string_lossy().ends_with(".tar.gz"))
        .collect();
    found.sort();
    found
}
