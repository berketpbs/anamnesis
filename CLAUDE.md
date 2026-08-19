# Anamnesis - Claude Code Context

## Project Overview

Anamnesis is a Rust-based long-term memory system for AI coding agents. It provides persistent context across sessions through a git-versioned markdown wiki.

## Tech Stack

- **Language**: Rust (Edition 2024)
- **Async Runtime**: Tokio
- **Database**: SQLite with migrations via refinery
- **Web Server**: Axum
- **MCP Server**: rmcp
- **Git Integration**: git2-rs
- **Serialization**: serde_json, serde_yaml, jsonc-parser

## Architecture

The project is organized as a multi-crate Rust workspace:

1. **Core Layer** (anamnesis-core) - Types and abstractions
2. **Storage Layer** (anamnesis-store) - SQLite persistence
3. **Wiki Layer** (anamnesis-wiki) - Git-versioned markdown
4. **Agent Integration** (anamnesis-hooks, anamnesis-mcp)
5. **LLM Integration** (anamnesis-llm, anamnesis-consolidate)
6. **Web Layer** (anamnesis-web)
7. **CLI** (anamnesis-cli)

## Key Concepts

- **Wiki**: Git-versioned markdown under `<data_dir>/wiki/<workspace>/<project>/`, outside the project repository
- **Session**: Bounded work within an agent session
- **Observation**: Capture of lifecycle events (prompts, tool calls, outputs)
- **Consolidation**: LLM-powered synthesis of session observations into wiki pages
- **Handoff**: Summary delivered to the next agent session

## Development Notes

- Run `cargo build` to compile all crates
- Run `cargo test` to execute tests
- Lints require `unsafe_code = "forbid"` and `missing_docs = "warn"`
- Per-project scope is pinned by `.anamnesis.toml` in the repository root
- The data directory (`ANAMNESIS_DATA_DIR`, or the platform data dir) holds `wiki/`, `raw/`, `db/`, `models/`, `logs/`
