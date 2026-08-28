# Anamnesis

[![CI](https://github.com/berketpbs/anamnesis/actions/workflows/ci.yml/badge.svg)](https://github.com/berketpbs/anamnesis/actions/workflows/ci.yml)

Long-term memory for AI coding agents. Preserve context across sessions and enable seamless continuity between different AI agent tools.

## Vision

AI coding agents lose context when a session ends. Anamnesis gives them a shared, persistent wiki compiled from sanitized lifecycle observations. When a session ends, relevant observations become a coherent summary; the next agent receives a bounded handoff.

## Project Structure

Anamnesis is organized as a Rust workspace with modular crates:

- **anamnesis-core** - Core types and data structures
- **anamnesis-store** - SQLite storage layer with migrations
- **anamnesis-wiki** - Git-versioned markdown wiki management
- **anamnesis-mcp** - Model Context Protocol server implementation
- **anamnesis-hooks** - Lifecycle hook capture and processing
- **anamnesis-llm** - LLM provider abstraction (Anthropic Messages API) and local embeddings
- **anamnesis-consolidate** - Session consolidation and summary generation
- **anamnesis-web** - HTTP server that receives hooks and delivers handoffs
- **anamnesis-cli** - Command-line interface
- **evals** - Retrieval evaluation: a checked-in corpus, the questions asked
  of it, and the score they get through the real query path

## What Works Today

- Lifecycle capture from Claude Code hooks, sanitized before anything is stored
- Every observation appended to an immutable transcript under `raw/`, so the
  index can be thrown away and rebuilt (`anamnesis reindex`)
- Git-versioned markdown wiki, one repository per data directory
- Session consolidation into a page plus a handoff for the next session —
  by counting when no model is configured, by reading when one is
- Retrieval over four signals fused with reciprocal rank: FTS5, entity
  matching, link neighbours, and optional local embeddings
  (`ANAMNESIS_EMBED_ENABLED=1`)
- MCP server: `memory_query`, `memory_write_page`, `memory_handoff_accept`,
  `workstream_start`, `workstream_status`
- Workstreams: parallel threads of work, each keeping its own handoff slot
- `anamnesis bootstrap` seeds a new project's memory from its git history
- `anamnesis sweep` forgets pages that have decayed — reports by default,
  deletes only with `--apply`, and never touches pinned, durable, canonical,
  or known-wrong pages
- `anamnesis improve` proposes what the memory could do better — promote a
  page several sessions kept returning to, write a page several pages link to
  — and carries out what it can, once a project says it may
- `[auto_improve.scheduler]`: the server runs that pass per project, on the
  interval that project asked for
- `anamnesis eval` scores retrieval against a checked-in corpus and its
  questions, so a change to ranking can be judged by what it does to recall
  rather than by argument. The suite runs in CI as an ordinary test

## Not Built Yet

These have configuration or a placeholder in the tree, and nothing behind it.
They are listed so nobody plans around them:

- **Web UI** — the server exposes `/health`, `/hook`, `/handoff`, and
  `/whoami`. There is no browser interface.

## Getting Started

### Build

```bash
cargo build
```

### Run CLI

```bash
cargo run -p anamnesis-cli -- status
```

### Run Tests

```bash
cargo test
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines on how to contribute to this project.

## License

MIT - See [LICENSE](LICENSE) for details.
