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

# Run with logging
RUST_LOG=debug cargo run -p anamnesis-cli -- status
```

## Architecture Guidelines

- Keep crates focused and independent
- Use workspace dependencies in Cargo.toml for consistency
- Document module boundaries and public APIs
- Prefer composition over inheritance

## Questions?

Open an issue if you have questions or want to discuss a feature before implementing it.
