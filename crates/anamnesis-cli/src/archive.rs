//! Taking memory somewhere else, and bringing it back.
//!
//! The wiki carries its own git history, so the *compiled* half of memory has
//! always been recoverable. The other half has not. `db/` is rebuildable from
//! the wiki and the transcripts, but `raw/` is rebuildable from nothing: the
//! observations a page was compiled from live in exactly one place, on one
//! disk, in a directory nobody backs up because it is not in any repository.
//!
//! Two rules shape this module.
//!
//! **The index is copied through SQLite, not through the filesystem.** It runs
//! in WAL mode, so at any instant the committed database is spread across
//! `anamnesis.db` and a `-wal` file beside it. Copying the first without the
//! second yields a file that opens, reports a plausible schema version, and is
//! missing whatever was written most recently — a backup that is quietly
//! stale, discovered on the day it is needed. [`Store::snapshot_to`] takes a
//! consistent copy while the server is running and writing.
//!
//! **Restore refuses before it writes.** It reads the manifest, checks the
//! archive against it, and says what it would do; `--apply` carries it out,
//! and a data directory that already holds memory is refused unless somebody
//! says `--force`. Restoring over a live memory is the one operation here that
//! cannot be undone by running the other one.

use std::io::Write;
use std::path::{Path, PathBuf};

use anamnesis_core::datadir::DataDir;
use anamnesis_store::Store;

/// Name of the manifest inside the archive.
const MANIFEST: &str = "manifest.json";

/// Format the archive is written in.
///
/// Read back before anything is unpacked. An archive from a future version may
/// hold directories this build would silently drop on the floor, and "restored
/// successfully, minus a directory you did not know about" is the failure this
/// number exists to prevent.
const FORMAT_VERSION: u32 = 1;

/// What an archive says about itself.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Manifest {
    /// Format of the archive, not the version of anamnesis that wrote it.
    format: u32,
    /// Version of anamnesis that wrote it, for a person reading the file.
    written_by: String,
    /// When it was written.
    written_at: String,
    /// Schema version of the index inside, when there was an index.
    schema: Option<i64>,
    /// Directories the archive carries, in the order they are unpacked.
    contents: Vec<String>,
}

/// Write the whole of memory to one archive.
///
/// `models/` and `logs/` are left out deliberately: the first is a download
/// that any machine can repeat, and the second is a record of one machine's
/// afternoons. Everything a person cannot get back is in.
pub fn cmd_backup(out: Option<PathBuf>, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let data = DataDir::resolve(data_dir)?;
    if !data.root().exists() {
        anyhow::bail!(
            "there is no memory at {} — run `anamnesis init` first",
            data.root().display()
        );
    }

    let out = out.unwrap_or_else(|| {
        default_archive_name(&jiff::Timestamp::now().strftime("%Y%m%d-%H%M%S").to_string())
    });
    if out.exists() {
        anyhow::bail!(
            "{} already exists; name a different file rather than overwriting a backup",
            out.display()
        );
    }

    println!("💾 Backing up {}", data.root().display());
    println!();

    // The index first, and through SQLite: see the module comment for why a
    // file copy is the wrong tool for a database that is being written to.
    let staged = tempfile::Builder::new()
        .prefix("anamnesis-index-")
        .suffix(".db")
        .tempfile()?;
    let mut schema = None;
    let index = data.db_file();
    if index.exists() {
        let store = Store::open(&index)?;
        schema = store.schema_version()?;
        store.snapshot_to(staged.path())?;
        println!(
            "  index      {} (schema {})",
            human_size(std::fs::metadata(staged.path())?.len()),
            schema.map_or_else(|| "none".to_owned(), |v| v.to_string())
        );
    } else {
        println!("  index      none yet");
    }

    let mut contents = Vec::new();
    if index.exists() {
        contents.push("db".to_owned());
    }
    for (name, path) in [("raw", data.raw()), ("wiki", data.wiki())] {
        if path.exists() {
            contents.push(name.to_owned());
            println!("  {name:<10} {}", human_size(directory_size(&path)));
        } else {
            println!("  {name:<10} none yet");
        }
    }

    let manifest = Manifest {
        format: FORMAT_VERSION,
        written_by: env!("CARGO_PKG_VERSION").to_owned(),
        written_at: jiff::Timestamp::now().to_string(),
        schema,
        contents: contents.clone(),
    };

    let file = std::fs::File::create(&out)?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);

    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, MANIFEST, manifest_bytes.as_slice())?;

    if index.exists() {
        builder.append_path_with_name(
            staged.path(),
            format!("db/{}", anamnesis_core::datadir::DB_FILE_NAME),
        )?;
    }
    if data.raw().exists() {
        builder.append_dir_all("raw", data.raw())?;
    }
    // Including `.git`, because the wiki's history *is* the wiki: page
    // restores and checkpoints read it, and an archive that dropped it would
    // restore a memory that had forgotten how it got here.
    if data.wiki().exists() {
        builder.append_dir_all("wiki", data.wiki())?;
    }
    builder.into_inner()?.finish()?.flush()?;

    println!();
    println!(
        "  → {} ({})",
        out.display(),
        human_size(std::fs::metadata(&out)?.len())
    );
    println!();
    println!("  models/ and logs/ were left out: one is a download, the other is one machine's.");
    Ok(())
}

/// Put an archive back, after saying what that would mean.
pub fn cmd_restore(
    archive: &Path,
    apply: bool,
    force: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let data = DataDir::resolve(data_dir)?;
    let manifest = read_manifest(archive)?;

    println!("♻  Restoring {}", archive.display());
    println!();
    println!(
        "  written    {} by anamnesis {}",
        manifest.written_at, manifest.written_by
    );
    println!(
        "  carries    {}",
        if manifest.contents.is_empty() {
            "nothing".to_owned()
        } else {
            manifest.contents.join(", ")
        }
    );
    println!(
        "  schema     {}",
        manifest
            .schema
            .map_or_else(|| "none".to_owned(), |v| v.to_string())
    );
    println!("  into       {}", data.root().display());

    let occupied = occupied_by(&data);
    if !occupied.is_empty() {
        println!();
        println!(
            "  ⚠ {} already holds memory ({})",
            data.root().display(),
            occupied.join(", ")
        );
        if !force {
            println!();
            println!(
                "  Nothing has been written. Restoring here would replace it — pass --force to say so,"
            );
            println!("  or --data-dir <path> to restore somewhere else and look first.");
            return Ok(());
        }
    }

    if !apply {
        println!();
        println!("  Nothing has been written. Run again with --apply to carry this out.");
        return Ok(());
    }

    data.ensure_layout()?;
    let unpacked = unpack(archive, data.root())?;

    println!();
    println!(
        "  {unpacked} entr{} restored",
        if unpacked == 1 { "y" } else { "ies" }
    );

    // Opened afterwards rather than trusted: an archive that unpacked cleanly
    // can still hold an index this build cannot read, and finding that out now
    // is the difference between a failed restore and a memory that fails on
    // the next hook.
    let index = data.db_file();
    if index.exists() {
        let store = Store::open(&index)?;
        let schema = store.schema_version()?;
        // Recorded into the memory that was just restored, which is the only
        // place it could be recorded: this line is how a reader learns that
        // everything before it came out of an archive rather than happening.
        crate::audit::note(
            &store,
            None,
            anamnesis_core::audit::Action::Restored,
            archive.display().to_string(),
            Some(format!(
                "{unpacked} entries, written {}",
                manifest.written_at
            )),
        );
        println!(
            "  index opens, schema {}",
            schema.map_or_else(|| "none".to_owned(), |v| v.to_string())
        );
    }
    println!();
    println!("  Run `anamnesis status` to see what came back.");
    Ok(())
}

/// Read the manifest without unpacking anything else.
fn read_manifest(archive: &Path) -> anyhow::Result<Manifest> {
    let file = std::fs::File::open(archive)
        .map_err(|error| anyhow::anyhow!("cannot read {}: {error}", archive.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for entry in tar.entries()? {
        let entry = entry?;
        if entry.path()?.as_os_str() == MANIFEST {
            let manifest: Manifest = serde_json::from_reader(entry)?;
            if manifest.format > FORMAT_VERSION {
                anyhow::bail!(
                    "this archive is format {} and this build understands {FORMAT_VERSION}; \
                     upgrade anamnesis rather than restoring part of it",
                    manifest.format
                );
            }
            return Ok(manifest);
        }
    }
    anyhow::bail!(
        "{} has no {MANIFEST} — it was not written by `anamnesis backup`",
        archive.display()
    )
}

/// Unpack every entry the manifest promised, and nothing outside the root.
fn unpack(archive: &Path, root: &Path) -> anyhow::Result<usize> {
    let file = std::fs::File::open(archive)?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut count = 0usize;

    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path.as_os_str() == MANIFEST {
            continue;
        }
        // An archive is a file from anywhere, and the name of an entry inside
        // one is the oldest way to write outside the directory it was told to
        // unpack into. Checked here rather than left to the tar crate, which
        // skips such an entry and reports success: a restore that silently
        // dropped part of itself is the failure this whole module exists to
        // prevent, so it is an error and it says which entry.
        if escapes_root(&path) {
            anyhow::bail!(
                "refusing an archive entry that points outside the data directory: {}",
                path.display()
            );
        }
        if !entry.unpack_in(root)? {
            anyhow::bail!("could not restore {} from the archive", path.display());
        }
        count += 1;
    }
    Ok(count)
}

/// Whether an entry name would leave the directory it is unpacked into.
///
/// Absolute paths and `..` are the two ways, and a name can mix them with
/// ordinary components (`wiki/../../elsewhere`), so every component is looked
/// at rather than only the first.
fn escapes_root(path: &Path) -> bool {
    path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        })
}

/// Which parts of a data directory already hold something.
fn occupied_by(data: &DataDir) -> Vec<String> {
    let mut held = Vec::new();
    if data.db_file().exists() {
        held.push("an index".to_owned());
    }
    if data.wiki().exists() && directory_size(&data.wiki()) > 0 {
        held.push("a wiki".to_owned());
    }
    if data.raw().exists() && directory_size(&data.raw()) > 0 {
        held.push("transcripts".to_owned());
    }
    held
}

/// Total size of a directory tree, for a line somebody reads.
fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_size(&entry.path()),
            Ok(_) => entry.metadata().map(|meta| meta.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

/// Where a backup goes when nobody said.
fn default_archive_name(stamp: &str) -> PathBuf {
    PathBuf::from(format!("anamnesis-backup-{stamp}.tar.gz"))
}

/// Bytes, rounded the way a person reads them.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A data directory with something in every part of it.
    fn populated() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = DataDir::resolve(Some(dir.path().to_path_buf())).expect("data dir");
        data.ensure_layout().expect("layout");

        let store = Store::open(data.db_file()).expect("store");
        store.migrate().expect("migrate");

        std::fs::create_dir_all(data.wiki().join("default/widget")).expect("wiki dir");
        std::fs::write(
            data.wiki().join("default/widget/a-page.md"),
            "# a page\n\nwhat the session decided\n",
        )
        .expect("page");
        std::fs::create_dir_all(data.raw().join("default/widget/2026-09-01")).expect("raw dir");
        std::fs::write(
            data.raw().join("default/widget/2026-09-01/session.jsonl"),
            "{\"kind\":\"user-prompt\"}\n",
        )
        .expect("transcript");
        dir
    }

    fn data_of(dir: &tempfile::TempDir) -> DataDir {
        DataDir::resolve(Some(dir.path().to_path_buf())).expect("data dir")
    }

    /// The whole point: everything a person cannot get back comes back. The
    /// transcript is the one that matters — the wiki has git and the index can
    /// be rebuilt, but `raw/` exists in exactly one place.
    #[test]
    fn a_backup_restores_the_index_the_wiki_and_the_transcripts() {
        let source = populated();
        let out = source.path().join("backup.tar.gz");
        cmd_backup(Some(out.clone()), Some(source.path().to_path_buf())).expect("backup");

        let target = tempfile::tempdir().expect("target");
        cmd_restore(&out, true, false, Some(target.path().to_path_buf())).expect("restore");

        let restored = data_of(&target);
        assert!(restored.db_file().exists(), "the index did not come back");
        assert_eq!(
            std::fs::read_to_string(restored.wiki().join("default/widget/a-page.md"))
                .expect("page"),
            "# a page\n\nwhat the session decided\n"
        );
        assert_eq!(
            std::fs::read_to_string(
                restored
                    .raw()
                    .join("default/widget/2026-09-01/session.jsonl")
            )
            .expect("transcript"),
            "{\"kind\":\"user-prompt\"}\n"
        );

        let store = Store::open(restored.db_file()).expect("open restored index");
        assert!(
            store.schema_version().expect("schema").is_some(),
            "the restored index has no schema"
        );
    }

    /// Restoring is the one operation here that cannot be undone by running
    /// the other one, so a data directory that already holds memory is left
    /// exactly as it was until somebody says otherwise.
    #[test]
    fn restoring_over_existing_memory_is_refused_until_forced() {
        let source = populated();
        let out = source.path().join("backup.tar.gz");
        cmd_backup(Some(out.clone()), Some(source.path().to_path_buf())).expect("backup");

        let target = populated();
        std::fs::write(
            data_of(&target).wiki().join("default/widget/a-page.md"),
            "# a page\n\nsomething else entirely\n",
        )
        .expect("their page");

        cmd_restore(&out, true, false, Some(target.path().to_path_buf())).expect("refused");
        assert_eq!(
            std::fs::read_to_string(data_of(&target).wiki().join("default/widget/a-page.md"))
                .expect("page"),
            "# a page\n\nsomething else entirely\n",
            "an unforced restore overwrote a page"
        );

        cmd_restore(&out, true, true, Some(target.path().to_path_buf())).expect("forced");
        assert_eq!(
            std::fs::read_to_string(data_of(&target).wiki().join("default/widget/a-page.md"))
                .expect("page"),
            "# a page\n\nwhat the session decided\n",
            "a forced restore left the old page in place"
        );
    }

    /// Saying what would happen has to mean nothing happened.
    #[test]
    fn a_restore_without_apply_writes_nothing() {
        let source = populated();
        let out = source.path().join("backup.tar.gz");
        cmd_backup(Some(out.clone()), Some(source.path().to_path_buf())).expect("backup");

        let target = tempfile::tempdir().expect("target");
        cmd_restore(&out, false, false, Some(target.path().to_path_buf())).expect("dry run");

        assert!(
            !data_of(&target).db_file().exists(),
            "a dry run created an index"
        );
        assert!(
            !data_of(&target).wiki().exists(),
            "a dry run created a wiki"
        );
    }

    /// An archive is a file from anywhere, and the name of an entry inside one
    /// is the oldest way to write outside the directory it was unpacked into.
    /// The name is written into the header by hand because the tar crate
    /// refuses to *produce* one — which is exactly why a reader cannot assume
    /// nobody else will.
    #[test]
    fn an_entry_pointing_outside_the_data_directory_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = dir.path().join("hostile.tar.gz");

        {
            let file = std::fs::File::create(&archive).expect("create");
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);

            let manifest = serde_json::to_vec(&Manifest {
                format: FORMAT_VERSION,
                written_by: "0.0.0".to_owned(),
                written_at: "2026-09-01T00:00:00Z".to_owned(),
                schema: None,
                contents: vec!["wiki".to_owned()],
            })
            .expect("manifest");
            let mut header = tar::Header::new_gnu();
            header.set_size(manifest.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, MANIFEST, manifest.as_slice())
                .expect("append manifest");

            let payload = b"somewhere else\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o644);
            let name = b"../escaped.md";
            header.as_old_mut().name[..name.len()].copy_from_slice(name);
            header.set_cksum();
            builder
                .append(&header, payload.as_slice())
                .expect("append entry");
            builder.into_inner().expect("tar").finish().expect("gz");
        }

        let target = tempfile::tempdir().expect("target");
        let refused = cmd_restore(&archive, true, false, Some(target.path().to_path_buf()));

        let message = refused
            .expect_err("an escaping entry was unpacked")
            .to_string();
        assert!(
            message.contains("points outside the data directory"),
            "the refusal has to name what was wrong, not report a failed write: {message}"
        );
        assert!(
            !dir.path().join("escaped.md").exists(),
            "an escaping entry landed outside the data directory"
        );
    }

    /// The rule on its own, including the shape that hides a `..` behind
    /// ordinary components.
    #[test]
    fn an_entry_name_that_leaves_the_directory_is_recognised() {
        assert!(escapes_root(Path::new("../escaped.md")));
        assert!(escapes_root(Path::new("wiki/../../escaped.md")));
        assert!(escapes_root(Path::new("/etc/passwd")));

        assert!(!escapes_root(Path::new("wiki/default/widget/a-page.md")));
        assert!(!escapes_root(Path::new("db/anamnesis.db")));
    }

    /// An archive from a newer anamnesis may carry directories this build has
    /// no name for, and restoring the parts it recognises would be a restore
    /// that quietly left something behind.
    #[test]
    fn an_archive_from_a_newer_format_is_refused_rather_than_half_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = dir.path().join("future.tar.gz");

        let file = std::fs::File::create(&archive).expect("create");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let manifest = serde_json::to_vec(&Manifest {
            format: FORMAT_VERSION + 1,
            written_by: "99.0.0".to_owned(),
            written_at: "2026-09-01T00:00:00Z".to_owned(),
            schema: Some(99),
            contents: vec!["db".to_owned(), "something-new".to_owned()],
        })
        .expect("manifest");
        let mut header = tar::Header::new_gnu();
        header.set_size(manifest.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, MANIFEST, manifest.as_slice())
            .expect("append");
        builder.into_inner().expect("tar").finish().expect("gz");

        let target = tempfile::tempdir().expect("target");
        let refused = cmd_restore(&archive, true, false, Some(target.path().to_path_buf()));

        assert!(refused.is_err(), "an archive from the future was restored");
        assert!(
            refused
                .unwrap_err()
                .to_string()
                .contains("upgrade anamnesis"),
            "the refusal did not say what to do about it"
        );
    }

    /// A file that is not one of ours says so, rather than failing somewhere
    /// deeper with a message about gzip.
    #[test]
    fn a_file_that_is_not_a_backup_is_named_as_such() {
        let dir = tempfile::tempdir().expect("tempdir");
        let not_a_backup = dir.path().join("notes.txt");
        std::fs::write(&not_a_backup, "just some notes").expect("write");

        let target = tempfile::tempdir().expect("target");
        let refused = cmd_restore(
            &not_a_backup,
            true,
            false,
            Some(target.path().to_path_buf()),
        );

        assert!(refused.is_err());
    }

    #[test]
    fn sizes_are_rounded_the_way_a_person_reads_them() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
