//! The path to write into files that name this binary.
//!
//! Hooks, the MCP registration and the service all start anamnesis by a path
//! written down once, and they go on using it after the binary is upgraded.
//! `current_exe` is not that path everywhere. On Linux it is read from
//! `/proc/self/exe`, which has every symlink resolved: a Homebrew install run as
//! `/home/linuxbrew/.linuxbrew/bin/anamnesis` reports
//! `…/Cellar/anamnesis/1.1.1/bin/anamnesis`, and `brew upgrade` deletes that
//! directory. The settings file would go on looking exactly right while every
//! hook failed to start. A Nix profile is the same shape.
//!
//! So the path the binary was invoked by is preferred when it leads to the
//! same file: the link a package manager keeps pointing at whichever version
//! is current. When it does not — a shim that launches the binary rather than
//! linking to it, a name that is not on `PATH` — `current_exe` stands. On
//! Windows `current_exe` already keeps a junction as it was invoked (measured
//! through one standing in for Scoop's `current`), so there this changes
//! nothing unless a real symlink was followed.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The path to name this binary by in anything that outlives this run.
pub fn stable_path() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let invoked = std::env::args_os().next();
    let search = std::env::var_os("PATH");
    Ok(invoked_path(&exe, invoked.as_deref(), search.as_deref(), same_file).unwrap_or(exe))
}

/// [`stable_path`] as the text a command line carries, `anamnesis` when there
/// is no path to give.
pub fn stable_command() -> String {
    stable_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "anamnesis".to_owned())
}

/// The path this binary was invoked by, when it differs from `exe` and `same`
/// says both lead to one file.
///
/// `invoked` is `argv[0]`: a path when it holds a separator, resolved against
/// the working directory, and otherwise a name looked up on `search` the way a
/// shell would have found it.
fn invoked_path(
    exe: &Path,
    invoked: Option<&OsStr>,
    search: Option<&OsStr>,
    same: impl Fn(&Path, &Path) -> bool,
) -> Option<PathBuf> {
    let invoked = Path::new(invoked?);
    let candidate = if invoked.components().count() > 1 {
        std::path::absolute(invoked).ok()?
    } else {
        std::env::split_paths(search?)
            .filter(|dir| dir.is_absolute())
            .flat_map(|dir| spellings(invoked).map(move |name| dir.join(name)))
            .find(|path| path.is_file())?
    };
    (candidate != exe && same(&candidate, exe)).then_some(candidate)
}

/// The file names a bare command could be on disk. Windows finds `anamnesis`
/// as `anamnesis.exe`; everywhere else the name is the file.
fn spellings(name: &Path) -> impl Iterator<Item = PathBuf> {
    let bare = name.to_path_buf();
    let exe = (cfg!(windows) && name.extension().is_none()).then(|| name.with_extension("exe"));
    exe.into_iter().chain(std::iter::once(bare))
}

/// Whether two paths reach the same file once every link is followed.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never(_: &Path, _: &Path) -> bool {
        false
    }

    fn always(_: &Path, _: &Path) -> bool {
        true
    }

    #[test]
    fn nothing_invoked_leaves_current_exe() {
        let exe = std::env::temp_dir().join("anamnesis");
        assert_eq!(invoked_path(&exe, None, None, always), None);
    }

    #[test]
    fn a_path_that_leads_elsewhere_is_not_taken() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("real");
        let other = dir.path().join("other");
        assert_eq!(
            invoked_path(&exe, Some(other.as_os_str()), None, never),
            None
        );
    }

    #[test]
    fn the_invoked_path_is_taken_when_it_is_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("Cellar").join("anamnesis");
        let link = dir.path().join("bin").join("anamnesis");
        assert_eq!(
            invoked_path(&exe, Some(link.as_os_str()), None, always),
            Some(link)
        );
    }

    #[test]
    fn invoking_the_binary_itself_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("anamnesis");
        assert_eq!(
            invoked_path(&exe, Some(exe.as_os_str()), None, always),
            None
        );
    }

    /// A bare name is looked up on `PATH`, and a relative entry there is
    /// skipped: it names a different directory depending on where the command
    /// ran, which is not a path to write into a settings file.
    #[test]
    fn a_bare_name_is_found_on_path_and_only_in_absolute_entries() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let file = if cfg!(windows) {
            "anamnesis.exe"
        } else {
            "anamnesis"
        };
        std::fs::write(bin.join(file), b"").unwrap();
        let exe = dir.path().join("real").join(file);

        let search = std::env::join_paths([PathBuf::from("bin"), bin.clone()]).unwrap();
        assert_eq!(
            invoked_path(&exe, Some(OsStr::new("anamnesis")), Some(&search), always),
            Some(bin.join(file))
        );

        let relative_only = std::env::join_paths([PathBuf::from("bin")]).unwrap();
        assert_eq!(
            invoked_path(
                &exe,
                Some(OsStr::new("anamnesis")),
                Some(&relative_only),
                always
            ),
            None
        );
    }

    /// What the module is for, on a real link: the name a package manager
    /// keeps current wins over the versioned directory it points into, and
    /// a copy — which a later upgrade does not touch — is not mistaken for it.
    #[cfg(unix)]
    #[test]
    fn a_symlink_to_the_binary_is_preferred_and_a_copy_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let cellar = dir.path().join("Cellar").join("1.1.1");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&cellar).unwrap();
        std::fs::create_dir(&bin).unwrap();
        let exe = cellar.join("anamnesis");
        std::fs::write(&exe, b"binary").unwrap();

        let link = bin.join("anamnesis");
        std::os::unix::fs::symlink(&exe, &link).unwrap();
        assert_eq!(
            invoked_path(&exe, Some(link.as_os_str()), None, same_file),
            Some(link)
        );

        let copy = dir.path().join("copy");
        std::fs::write(&copy, b"binary").unwrap();
        assert_eq!(
            invoked_path(&exe, Some(copy.as_os_str()), None, same_file),
            None
        );
    }
}
