# Anamnesis Project Memory Index

Project-wide memory for anamnesis - persistent wiki for decisions, procedures, and project context.

## Project Status

**Phase**: Core infrastructure setup (Steps 1-3 complete)
**Repository**: https://github.com/berketpbs/anamnesis
**Tech Stack**: Rust, Tokio, SQLite, Axum, MCP, Docker
**Commits**: 5 (workspace structure, Step 1+2, Docker support)

## Active Pages

This index tracks high-value pages in the project memory. Add entries as pages are created.

### Decisions
- [[workspace-structure]] - Multi-crate Rust workspace design
- [[ui-framework-choice]] - Web UI technology selection
- [[storage-engine]] - SQLite as primary storage

### Procedures
- [[local-development]] - Development environment setup
- [[adding-new-crate]] - Process for adding workspace members
- [[database-migrations]] - Schema management workflow

### Gotchas
- [[windows-compatibility]] - Platform-specific considerations
- [[unsafe-code-policy]] - Forbid unsafe across all crates
- [[hook-capture-sanitation]] - Sensitive data removal requirements

### Rules
- [[code-standards]] - Rust edition 2024, MSRV 1.95
- [[documentation-requirements]] - Required docs for public APIs
- [[testing-practices]] - Testing and CI expectations

## Architecture Overview

10 modular crates in a Rust workspace:
- **Core Layer**: types, abstractions
- **Storage**: SQLite persistence, migrations
- **Wiki**: Git-versioned markdown management
- **Integration**: MCP server, lifecycle hooks
- **LLM**: Provider abstraction, consolidation
- **Web**: HTTP server, UI
- **CLI**: Command-line interface
- **Workstream**: Cross-harness session management

## Last Updated

2026-08-19

---

For more information, see [README.md](README.md) in this directory.
