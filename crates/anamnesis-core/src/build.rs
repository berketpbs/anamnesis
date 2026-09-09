//! Which build this is.
//!
//! The version alone cannot answer it. `1.0.0` is the same string across every
//! commit of a release cycle, so a binary recording sessions in the background
//! and a binary just compiled are indistinguishable by version — and this
//! project has already lost weeks of page quality to exactly that: hooks
//! calling an executable that predated the code meant to be recording what
//! tools returned, with nothing anywhere saying so.

/// The commit this binary was built from, or `unknown`.
///
/// A trailing `+` means the tree had uncommitted changes at build time, which
/// is the ordinary state of a binary somebody built to try something out — and
/// worth seeing, because it is also the state where two builds share a commit
/// and are not the same build.
pub const COMMIT: &str = env!("ANAMNESIS_BUILD_COMMIT");

/// The release version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Version and commit, as a person reads it: `1.0.0 (a40902a)`.
///
/// A `const` rather than a function so that it can be handed to argument
/// parsers and headers that want a `&'static str`.
pub const IDENTITY: &str = constcat();

/// Concatenate the two at compile time.
///
/// Written out rather than pulled from a crate: it is one string, built once,
/// and a dependency for it would be the larger cost.
const fn constcat() -> &'static str {
    // `concat!` takes literals only, and `COMMIT` is one — it arrives from the
    // build script as `env!`, which expands to a literal here.
    concat!(
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("ANAMNESIS_BUILD_COMMIT"),
        ")"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stamp that is empty, or that still carries the shell quoting of the
    /// build script, is worse than none: it makes two different builds compare
    /// equal and reports the mismatch as agreement.
    #[test]
    fn the_build_is_stamped_with_something_usable() {
        assert!(!COMMIT.is_empty());
        assert!(!COMMIT.contains(char::is_whitespace), "{COMMIT}");
        assert!(IDENTITY.starts_with(VERSION));
        assert!(IDENTITY.contains(COMMIT));
    }
}
