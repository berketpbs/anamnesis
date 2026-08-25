# Getting Started with Anamnesis

## Prerequisites

- Rust 1.95 or later
- Git
- SQLite (bundled with project)

## Installation

### From Source

```bash
git clone https://github.com/berketpbs/anamnesis.git
cd anamnesis
cargo build --release
```

The binary will be available at `target/release/anamnesis`.

## Quick Start

### 1. Initialize a Project

```bash
anamnesis init my-project
cd my-project
```

This creates:
- `.anamnesis.toml` - Marker file pinning this project's memory scope
- `<data_dir>/wiki/<workspace>/<project>/` - Wiki pages, in their own git repository
- `<data_dir>/db/anamnesis.db` - SQLite index, rebuildable from the wiki

The data directory defaults to the platform data directory and can be overridden
with `--data-dir` or `ANAMNESIS_DATA_DIR`. Run `anamnesis status --verbose` to
see exactly which paths are in use.

### 2. Configure a model (optional)

Anamnesis works with no model configured. Every session still gets a page and
a handoff — compiled by counting what happened rather than by reading it, and
the page says so in its footer. A model replaces that counted summary with one
that can say *why* a session did what it did.

Export a key in the environment the server runs in:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

That alone is enough: a key present selects the Anthropic provider. Everything
else has a default.

| Variable | Default | What it does |
| --- | --- | --- |
| `ANTHROPIC_API_KEY` | — | Credential. `ANAMNESIS_LLM_API_KEY` overrides it. |
| `ANAMNESIS_LLM_PROVIDER` | `anthropic` when a key is set | `anthropic` or `none`. Set `none` to turn the model off without unsetting the key. |
| `ANAMNESIS_LLM_MODEL` | `claude-opus-5` | Model id. |
| `ANAMNESIS_LLM_BASE_URL` | `https://api.anthropic.com` | Point at a gateway or a local server. |
| `ANAMNESIS_LLM_EFFORT` | `high` | `low`, `medium`, `high`, `xhigh`, or `max`. |
| `ANAMNESIS_LLM_MAX_INPUT_TOKENS` | `6500` | Prompt budget. Long sessions are trimmed from the middle to fit. |
| `ANAMNESIS_LLM_MAX_OUTPUT_TOKENS` | `2000` | Reply budget, floored at 1000. |
| `ANAMNESIS_LLM_TIMEOUT_SECS` | `90` | Per-request timeout. |
| `ANAMNESIS_LLM_MAX_RETRIES` | `2` | Retries, for rate limits and server faults only. |
| `ANAMNESIS_LLM_FALLBACKS` | on | Server-side fallback to another model if a request is declined. |

A typo is reported at startup rather than at the end of the first session:
`anamnesis serve` refuses to bind if the settings do not parse, and prints
which model it will consolidate with when they do. `anamnesis status
--verbose` reports the same thing.

**Nothing depends on the model being reachable.** If a request times out, is
declined, or comes back as something other than a page, the counted summary is
written instead and the reason is logged. Consolidation also runs *after* the
hook's response is sent, so a slow model delays the page, never the session.

#### Per-project style

A project can say how its summaries should be written by adding a page to its
own wiki:

```text
<data_dir>/wiki/<workspace>/<project>/_prompts/consolidation.md
```

Whatever is in it is included in the prompt — "write in Turkish", "always name
the migration numbers", "mention ticket ids". It is guidance, not structure:
the reply shape is fixed by a schema regardless.

### 3. Start the Memory Server

```bash
anamnesis serve
```

The server will:
- Start MCP server on loopback (for local agents)
- Start web UI on http://localhost:8080
- Begin listening for lifecycle hooks

### 4. Connect Your Agent

For Claude Code:

```bash
anamnesis install-mcp --client claude
```

For other agents, see [AGENTS.md](../AGENTS.md).

## Common Commands

### Seed From Git History

A new project starts with an empty wiki. `bootstrap` fills it with what the
repository already records — who works here, where the churn is, what just
landed — so the first session has something to read:

```bash
anamnesis bootstrap --repo .          # write bootstrap/ pages
anamnesis bootstrap --dry-run         # show what it would write
anamnesis bootstrap --force           # refresh a stale snapshot
```

Existing pages are never overwritten without `--force`: bootstrap seeds a
memory, it does not maintain one. The pages it writes are derived from commits
rather than decided by anyone, and rank below what a session actually learned.

### Search Memory

```bash
anamnesis search "postgres migration"
```

### Write a Page

```bash
anamnesis write-page \
  --path decisions/0001-database.md \
  --title "Chosen PostgreSQL" \
  --body "# Database Choice\n\nWe chose PostgreSQL because..."
```

### View Status

```bash
anamnesis status
```

### Export/Backup

```bash
anamnesis backup export --output backup.tar.gz
```

## Configuration

Edit `.anamnesis.toml` to customize behavior:

```toml
[scope]
workspace = "default"
project = "my-project"

[capture]
# Exclude patterns from capture
ignore_paths = [
    "target/",
    "node_modules/",
    ".env",
    "*.log"
]

[auto_improve]
enabled = true
require_approval = false  # Auto-approve consolidation

[[auto_improve.scheduler]]
interval_minutes = 60
enabled = true
```

## Troubleshooting

### Server Won't Start

Check that port 8080 is not in use:

```bash
# Check what's using port 8080
lsof -i :8080

# Use a different port
anamnesis serve --port 9000
```

### No Hook Events Captured

Verify hook installation:

```bash
# For Claude Code
anamnesis install-mcp --client claude --status
```

### Database Locked

If you see "database is locked":

```bash
# Ensure only one server instance is running
ps aux | grep anamnesis

# Restart the server
anamnesis serve --fresh
```

## Next Steps

1. **Read** [ARCHITECTURE.md](./ARCHITECTURE.md) to understand the system
2. **Explore** [AGENTS.md](../AGENTS.md) for agent-specific setup
3. **Review** [API.md](./API.md) for MCP tool documentation
4. **Join** discussions and contribute improvements

## Support

- File issues on [GitHub](https://github.com/berketpbs/anamnesis/issues)
- Read the [FAQ](./FAQ.md)
- Check [CONTRIBUTING.md](../CONTRIBUTING.md) for contribution guidelines
