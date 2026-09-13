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
//! something the rule masks. It never prints a value. `--apply` rewrites them,
//! and checkpoints the index so the file holds no old page with the value.
//!
//! What it does not rewrite is said instead of done, and looked at first so
//! what is said is true. Wiki pages are a git repository whose history keeps
//! every version, so a page holding a secret — now, or in any commit — is named
//! for a person to deal with. Backups and copies of the index are archives
//! taken before the rewrite; each is checked and named with what it still
//! holds, and none is deleted.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Read;
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
        // On every `--apply`, not only one that changed a row: a checkpoint an
        // earlier run could not finish is finished by running it again.
        if apply {
            store.checkpoint()?;
        }
    }
    print_line("index", &index, "observation row(s)");

    // The rows are not the file. Read as bytes, the database and its log are
    // checked for a credential the rows no longer show: the old page a
    // rewritten row was on, until a checkpoint overwrites it.
    let wal = PathBuf::from(format!("{}-wal", data.db_file().display()));
    let leftover = holds(&data.db_file(), &redactor) + holds(&wal, &redactor);
    if leftover > 0 && index.changed == 0 {
        println!(
            "  {:<10} a credential outside its rows — old pages not yet overwritten",
            "index file"
        );
    }

    let (pages, page_rules) = scan_wiki(&data.wiki(), &redactor);
    if pages.is_empty() {
        println!("  {:<10} nothing", "wiki");
    } else {
        println!(
            "  {:<10} {} page(s): {} — not rewritten: edit or forget them",
            "wiki",
            pages.len(),
            describe_rules(&page_rules)
        );
        for page in &pages {
            println!("             {}", page.display());
        }
    }

    // Separate from the pages on disk: a page masked or forgotten since is
    // clean in the working tree and whole in every commit before that.
    let history = scan_wiki_history(&data.wiki(), &redactor);
    if !history.is_empty() {
        println!(
            "  {:<10} {} page(s) had a version holding a value, and git keeps it:",
            "history",
            history.len()
        );
        for page in &history {
            println!("             {page}");
        }
    }

    for problem in &refused {
        println!("  ✗ {problem}");
    }

    let copies: Vec<(PathBuf, usize)> = copies(&data)
        .into_iter()
        .map(|path| {
            let held = holds(&path, &redactor);
            (path, held)
        })
        .filter(|(_, held)| *held > 0)
        .collect();
    if !copies.is_empty() {
        println!(
            "  {:<10} {} archive(s) or copies of the index still hold credentials:",
            "copies",
            copies.len()
        );
        for (path, held) in &copies {
            println!("             {} ({held} record(s))", path.display());
        }
    }

    println!();
    let pending = raw.changed > 0 || index.changed > 0;
    if !pending && leftover == 0 {
        println!("  Nothing in the spool or the index holds anything today's rules would mask.");
    } else if !apply {
        println!("  Nothing has been written. Run with --apply to mask them.");
    } else if leftover == 0 {
        println!("  Rewritten. Every value is gone from the spool, the index and its file.");
    } else {
        println!("  The index file still holds old pages: something was reading it while");
        println!("  this ran. Stop the server and run `anamnesis redact --apply` again.");
    }
    if !history.is_empty() || !copies.is_empty() {
        println!();
        println!("  Not touched: history and archives are yours to rewrite or delete. If a");
        println!("  value is a credential, revoking it is what makes every copy harmless.");
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

fn describe_rules(rules: &BTreeMap<&'static str, usize>) -> String {
    rules
        .iter()
        .map(|(rule, count)| format!("{rule} ×{count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn merged(one: &Redaction, two: &Redaction) -> BTreeMap<&'static str, usize> {
    let mut total = Redaction::default();
    total.absorb(one);
    total.absorb(two);
    total.rules
}

/// The rules' hits on `text`, when they would change it.
///
/// Compared, not asked, for the reason `Store::redact_observations` gives:
/// text already masked matches the assignment rule again and comes out the
/// same, and that is not a value left to find.
fn unmasked(redactor: &Redactor, text: &str) -> Option<Vec<&'static str>> {
    let redacted = redactor.redact(text);
    (!redacted.is_clean() && redacted.text() != text).then(|| redacted.hits().to_vec())
}

/// Wiki pages holding something the rules mask, and per-rule page counts.
fn scan_wiki(root: &Path, redactor: &Redactor) -> (Vec<PathBuf>, BTreeMap<&'static str, usize>) {
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
                && let Some(hits) = unmasked(redactor, &text)
            {
                rules.count(&hits);
                pages.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
            }
        }
    }
    pages.sort();
    (pages, rules.rules)
}

/// Paths of pages that held something the rules mask in any commit the
/// wiki's repository can reach. Each distinct version is read once.
fn scan_wiki_history(root: &Path, redactor: &Redactor) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    // Opened, never initialised: this looks at a repository and must not
    // create one where there is none.
    let Ok(repo) = git2::Repository::open(root) else {
        return found;
    };
    let Ok(mut walk) = repo.revwalk() else {
        return found;
    };
    if walk.push_glob("*").is_err() {
        return found;
    }
    let mut seen = HashSet::new();
    for oid in walk.flatten() {
        let Ok(tree) = repo.find_commit(oid).and_then(|commit| commit.tree()) else {
            continue;
        };
        let _ = tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
            if entry.kind() == Some(git2::ObjectType::Blob)
                && seen.insert(entry.id())
                && let Ok(blob) = repo.find_blob(entry.id())
                && let Ok(text) = std::str::from_utf8(blob.content())
                && unmasked(redactor, text).is_some()
            {
                found.insert(format!("{dir}{}", entry.name().unwrap_or("?")));
            }
            git2::TreeWalkResult::Ok
        });
    }
    found
}

/// Archives and copies of the index that a rewrite does not reach: backups,
/// an archive written into the data directory itself, and copies of the
/// database set aside beside it.
fn copies(data: &DataDir) -> Vec<PathBuf> {
    let listed = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect()
    };
    let mut found: Vec<PathBuf> = listed(&data.root().join("backups"))
        .into_iter()
        .chain(listed(data.root()))
        .filter(|path| path.to_string_lossy().ends_with(".tar.gz"))
        .collect();

    // `anamnesis.db.bak-…`, `anamnesis.db.before`: the database's own
    // name with something after it, other than SQLite's `-wal` and `-shm`.
    if let (Some(dir), Some(name)) = (
        data.db_file().parent(),
        data.db_file()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
    ) {
        let prefix = format!("{name}.");
        found.extend(listed(dir).into_iter().filter(|path| {
            path.file_name()
                .is_some_and(|file| file.to_string_lossy().starts_with(&prefix))
        }));
    }
    found.sort();
    found
}

/// How many records in a copy hold a credential: archive members, or, for a
/// loose file, the file itself counted once. Read-only; an unreadable copy
/// counts as none, since this only decides what is named.
///
/// By shape only ([`Redactor::credentials_in`]): these are bytes — a database
/// file, JSON lines, a git object — and the rules that read context misread
/// them. A password in a `key = value` line of an old archive is not found
/// here; a provider key is.
fn holds(path: &Path, redactor: &Redactor) -> usize {
    let text_holds = |bytes: &[u8]| {
        !redactor
            .credentials_in(&String::from_utf8_lossy(bytes))
            .is_empty()
    };
    if !path.to_string_lossy().ends_with(".tar.gz") {
        return std::fs::read(path).map_or(0, |bytes| usize::from(text_holds(&bytes)));
    }
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let Ok(entries) = archive.entries() else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.header().entry_type().is_file())
        .filter(|entry| {
            entry
                .path()
                .is_ok_and(|member| !member.starts_with("models"))
        })
        .filter_map(|mut entry| {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).ok().map(|_| bytes)
        })
        .filter(|bytes| text_holds(bytes))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not a key: the shape the `AQ.` rule masks.
    const KEY: &str = "AQ.testonlynotarealkey0123456789abcdefghijklmn";

    fn commit(repo: &git2::Repository, path: &str, text: &str, message: &str) {
        let root = repo.workdir().expect("workdir");
        std::fs::create_dir_all(root.join(path).parent().expect("parent")).expect("dirs");
        std::fs::write(root.join(path), text).expect("write");
        let mut index = repo.index().expect("index");
        index.add_path(Path::new(path)).expect("add");
        index.write().expect("index write");
        let tree = repo
            .find_tree(index.write_tree().expect("tree"))
            .expect("find tree");
        let signature = git2::Signature::now("test", "test@example.com").expect("signature");
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )
        .expect("commit");
    }

    /// The case that went unreported: a page that once listed the key and was
    /// rewritten since. The working tree is clean, and the commit before is not.
    #[test]
    fn a_value_masked_on_the_page_is_still_found_in_its_history() {
        let dir = tempfile::tempdir().expect("dir");
        let repo = git2::Repository::init(dir.path()).expect("init");
        let page = "default/demo/sessions/2026-09-01-a.md";
        commit(&repo, page, &format!("- use {KEY} for gemini\n"), "session");
        commit(&repo, page, "- use a key for gemini\n", "recompile");
        commit(
            &repo,
            "default/demo/notes/other.md",
            "export API_KEY=[redacted]\n",
            "note",
        );

        let redactor = Redactor::new();
        assert!(
            scan_wiki(dir.path(), &redactor).0.is_empty(),
            "the tree is clean"
        );
        assert_eq!(
            scan_wiki_history(dir.path(), &redactor)
                .into_iter()
                .collect::<Vec<_>>(),
            [page],
            "only the version that held an unmasked value"
        );
    }

    #[test]
    fn a_wiki_that_is_not_a_repository_has_no_history_and_is_left_alone() {
        let dir = tempfile::tempdir().expect("dir");
        assert!(scan_wiki_history(dir.path(), &Redactor::new()).is_empty());
        assert!(
            !dir.path().join(".git").exists(),
            "a scan created a repository"
        );
    }

    /// Copies are named by what they still hold, so a clean fresh backup is
    /// not listed beside the ones that matter, and a copy of the index set
    /// aside by hand is found as well as an archive.
    #[test]
    fn copies_are_named_only_when_they_still_hold_a_value() {
        let dir = tempfile::tempdir().expect("dir");
        let data = DataDir::new(dir.path());
        std::fs::create_dir_all(data.root().join("backups")).expect("backups");
        std::fs::create_dir_all(data.db_file().parent().expect("db dir")).expect("db");

        let archive = |path: PathBuf, member: &str, text: &str| {
            let file = std::fs::File::create(path).expect("archive");
            let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
                file,
                flate2::Compression::fast(),
            ));
            let mut header = tar::Header::new_gnu();
            header.set_size(text.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, member, text.as_bytes())
                .expect("append");
            builder
                .into_inner()
                .expect("finish")
                .finish()
                .expect("gzip");
        };
        let old = data.root().join("backups").join("pre-1.tar.gz");
        let fresh = data.root().join("backups").join("post-2.tar.gz");
        archive(
            old.clone(),
            "raw/a.jsonl",
            &format!("{{\"body\":\"use {KEY}\"}}"),
        );
        archive(
            fresh.clone(),
            "raw/a.jsonl",
            "{\"body\":\"use [redacted:google-auth-key]\"}",
        );
        let set_aside = data.db_file().with_file_name(format!(
            "{}.bak-1",
            data.db_file().file_name().expect("name").to_string_lossy()
        ));
        std::fs::write(&set_aside, format!("binary\0junk {KEY} more")).expect("copy");
        std::fs::write(data.db_file(), format!("live {KEY}")).expect("live");

        let redactor = Redactor::new();
        let named: Vec<PathBuf> = copies(&data)
            .into_iter()
            .filter(|path| holds(path, &redactor) > 0)
            .collect();
        assert_eq!(named, [old, set_aside], "the live index is not a copy");
        assert_eq!(holds(&fresh, &redactor), 0);
    }
}
