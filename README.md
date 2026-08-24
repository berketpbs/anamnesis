# Anamnesis

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
- **anamnesis-llm** - LLM provider abstraction (Anthropic, OpenAI, etc.)
- **anamnesis-consolidate** - Session consolidation and summary generation
- **anamnesis-web** - Web UI and HTTP server for wiki browsing
- **anamnesis-cli** - Command-line interface
- **evals** - Evaluation suite and testing harness

## Features

- Zero-friction lifecycle capture from agent sessions
- Git-versioned markdown wiki for persistent knowledge
- FTS5 full-text search + entity-based recall
- Per-project isolation with optional per-user memory slots
- Optional LLM-powered consolidation and auto-improvement
- Web UI for browsing and searching the wiki
- MCP server for integration with Claude Code and other agents

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
