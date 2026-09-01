//! Opening the things a command works on.
//!
//! Nearly every command needs the same three: the scope the working directory
//! resolves to, the index, and the wiki. They are opened here so that a
//! command that is about pages, or sessions, or proposals is not also about
//! where those live — and so that "which project am I in" has exactly one
//! answer per process.

use std::path::PathBuf;

use anamnesis_core::datadir::DataDir;
use anamnesis_core::scope::resolve_scope;
use anamnesis_store::Store;

/// Open the index and wiki for the project containing the current directory.
///
/// Every read-only command needs the same three things, and getting them
/// wrong (a data dir that does not exist, a scope resolved from the wrong
/// directory) is the usual reason a command reports nothing rather than
/// failing outright.
/// The workspace-wide scope this project inherits from.
///
/// One per workspace, derived rather than looked up, so every process that
/// asks for it lands on the same rows. Its root is where its pages live: there
/// is no repository behind it for a relative path to resolve against.
pub fn global_scope(
    project: &anamnesis_core::scope::ResolvedScope,
    data: &DataDir,
) -> anamnesis_core::scope::ResolvedScope {
    anamnesis_core::scope::ResolvedScope::global(
        &project.scope.workspace,
        data.wiki_global(&project.scope.workspace),
    )
}

/// The scope, the data directory, and the index for the working directory.
///
/// Fails rather than creating anything: a command that runs before `anamnesis
/// init` should say so, not quietly start a second memory somewhere.
pub fn open_project(
    data_dir: Option<PathBuf>,
) -> anyhow::Result<(anamnesis_core::scope::ResolvedScope, DataDir, Store)> {
    let cwd = std::env::current_dir()?;
    let scope = resolve_scope(&cwd)?;
    let data = DataDir::resolve(data_dir)?;
    if !data.db_file().exists() {
        anyhow::bail!(
            "no memory at {} — run `anamnesis init` first",
            data.root().display()
        );
    }
    let store = Store::open(data.db_file())?;
    store.migrate()?;
    Ok((scope, data, store))
}
