# Contributing to Anamnesis

We welcome contributions! Please follow these guidelines when contributing to the project.

## Getting Started

1. Fork the repository
2. Clone your fork: `git clone https://github.com/YOUR_USERNAME/anamnesis.git`
3. Create a new branch: `git checkout -b feature/your-feature-name`
4. Make your changes
5. Test your changes: `cargo test`
6. Commit with clear messages
7. Push to your fork and submit a pull request

## Code Style

- Follow Rust naming conventions (snake_case for functions/variables, PascalCase for types)
- Use rustfmt for formatting: `cargo fmt`
- Run clippy for linting: `cargo clippy -- -D warnings`
- All crates require `unsafe_code = "forbid"` - no unsafe code
- Document public APIs with doc comments

## Testing

- Write tests for new functionality
- Ensure all tests pass: `cargo test`
- Add integration tests for feature interactions
- Test error cases and edge conditions

## Commit Messages

- Use clear, descriptive commit messages
- Reference issues when applicable: `Fixes #123`
- Keep the first line under 72 characters
- Add more detailed description if needed

## Pull Requests

- Provide a clear description of changes
- Link any related issues
- Ensure CI passes
- Request review from maintainers

## Development Commands

```bash
# Build all crates
cargo build

# Run tests
cargo test

# Format code
cargo fmt

# Run clippy linter
cargo clippy -- -D warnings

# Score retrieval against the checked-in corpus
cargo run -p anamnesis-cli -- eval --verbose

# Run with logging
RUST_LOG=debug cargo run -p anamnesis-cli -- status
```

## Releasing

Aim for a stable release about once a month, or soon after a significant
user-visible change, once checks and field use support it. Keep the README's
install instructions honest while the main branch is ahead of the latest
stable release.

The maintainer pushes release tags. A `-rc` tag publishes a prerelease; a
final tag publishes the stable release. Before tagging, update the version in
`Cargo.toml` under `[workspace.package]` and move the relevant entries from
`Unreleased` to a dated heading in `CHANGELOG.md`. If the final release lands
on a later day than its candidate, update that heading's date first.

The release workflow builds five archives: Linux x86-64 and arm64, macOS Intel
and Apple silicon, and Windows x86-64. It checks that each binary starts and
publishes the archives with `SHA256SUMS` and build provenance attestations.
Check an extracted binary, its SHA-256 checksum, and its attestation before
using a candidate for daily work. Pin install-script rehearsals with
`ANAMNESIS_VERSION=vX.Y.Z-rc.N`; an unpinned install should still select the
previous stable release during the candidate period.

Use the candidate for several days, including real handoffs between agents,
and check `doctor` and `reindex --check`. After final publication, the workflow
pushes a `packaging/vX.Y.Z` branch with Homebrew and Scoop manifests. Review
and merge that branch through a PR so package installs catch up with the new
stable release.

To rehearse without publishing anything, run the same workflow by hand:

```bash
gh workflow run release.yml
```

That builds and uploads the archives as workflow artifacts and stops before
the publish step, which is guarded on the ref being a tag. It is worth doing
after any change to the workflow: a release workflow that is first exercised
on release day is exercised on the worst possible day.

## Architecture Guidelines

- Keep crates focused and independent
- Use workspace dependencies in Cargo.toml for consistency
- Document module boundaries and public APIs
- Prefer composition over inheritance

## Questions?

Open an issue if you have questions or want to discuss a feature before implementing it.
