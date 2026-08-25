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

Run this **inside** the repository you want remembered. `init` takes no
arguments and creates no directory — it registers the project you are
standing in:

```bash
cd my-project
anamnesis init
```

This creates:
- `<data_dir>/wiki/<workspace>/<project>/` - Wiki pages, in their own git repository
- `<data_dir>/db/anamnesis.db` - SQLite index, rebuildable from the wiki
- `<data_dir>/raw/` - Append-only transcripts the index can be rebuilt from

Identity comes from the git remote, so two clones of the same repository share
one memory. To pin it explicitly instead, write a `.anamnesis.toml` in the
repository root:

```toml
[scope]
workspace = "default"
project = "my-project"
```

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

This binds `127.0.0.1:8080` and serves three endpoints — `POST /hook`,
`GET /handoff`, `GET /health`. There is no web UI to open, and no
authentication, which is why the default bind is loopback.

The MCP server is a separate process the agent launches itself; `serve` does
not start one.

### 4. Connect Your Agent

Claude Code talks to anamnesis two ways, and they are independent.

**Hooks** capture the session. Print the configuration and paste it into your
`settings.json`:

```bash
anamnesis install-hooks --agent claude-code
```

**MCP** lets the agent search memory and write pages on purpose. Register the
server as a stdio subprocess:

```bash
claude mcp add anamnesis -- anamnesis mcp --repo .
```

Hooks need `anamnesis serve` running. MCP does not — it opens the store
directly.

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

### Read What the Last Session Left

```bash
anamnesis handoff        # peek, without consuming it
anamnesis sessions       # recent sessions, newest first
anamnesis show-page bootstrap/repository.md
```

### Rebuild the Index

The database is disposable. If it is lost or corrupted, rebuild it from the
wiki and the transcripts:

```bash
anamnesis reindex
```

Safe to run against a live database: every identifier is derived, so a rebuild
reproduces the same rows rather than duplicating them.

### Back Up

There is no `backup` command. The data directory is the backup — copy it, or
push `<data_dir>/wiki` somewhere, since it is an ordinary git repository:

```bash
git -C <data_dir>/wiki remote add origin git@example.com:me/memory.git
git -C <data_dir>/wiki push -u origin HEAD
```

`HEAD` rather than a branch name, because a wiki created before this was
fixed is on `master`: `libgit2` ignores `init.defaultBranch`, and older
versions left the branch it chose. New wikis start on `main`, and reopening
an existing one never renames its branch.

## Configuration

`.anamnesis.toml` is optional. Only `[scope]` changes behaviour today:

```toml
[scope]
workspace = "default"
project = "my-project"
```

Unknown keys are rejected rather than ignored, so a typo surfaces instead of
quietly sending memory to the wrong project.

The loader also accepts `[capture]`, `[slots]`, and `[auto_improve]`, but
**nothing reads them yet**:

```toml
[capture]
ignore_paths = ["target/**", "*.log"]   # parsed; never consulted

[slots]
per_user = false                        # parsed; never consulted

[auto_improve]
enabled = true                          # parsed; nothing runs
require_approval = true

# A single table. `[[auto_improve.scheduler]]` — the double-bracket array
# form — is rejected at load time.
[auto_improve.scheduler]
enabled = false
interval_minutes = 60
```

In particular, do not put `.env` in `ignore_paths` and assume it is excluded.
Redaction is what keeps secrets out of memory today.

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

Hooks fail quietly on purpose — a memory system that can break your editing
session is worse than one that misses an event — so check in this order:

```bash
anamnesis status --verbose   # is the scope what you expect?
anamnesis sessions           # did anything arrive at all?
curl http://127.0.0.1:8080/health
```

The `hook` command always exits 0, but it writes the reason to **stderr**: a
rejected event, an unreachable server, a payload it could not parse. If
Claude Code hides hook stderr, run the same command by hand with a payload on
stdin to see it.

On Windows, PowerShell prepends a UTF-8 BOM when piping text into a native
executable. Both the CLI and the server strip it; a third-party wrapper that
does not will produce a parse error on the first character.

### Database Locked

If you see "database is locked", more than one process is writing. Only one
`anamnesis serve` should be running against a data directory:

```bash
ps aux | grep anamnesis
```

If the index is genuinely damaged, delete it and rebuild — nothing is lost
that `wiki/` and `raw/` do not already hold:

```bash
rm <data_dir>/db/anamnesis.db
anamnesis reindex
```

## Next Steps

1. **Read** [ARCHITECTURE.md](./ARCHITECTURE.md) to understand the system
2. **Read** [USE_CASES.md](./USE_CASES.md) for what this is for
3. **Run** `anamnesis --help`, or `anamnesis <command> --help` — the CLI is
   the current reference for the MCP tools and every flag

## Support

- File issues on [GitHub](https://github.com/berketpbs/anamnesis/issues)
- Check [CONTRIBUTING.md](../CONTRIBUTING.md) for contribution guidelines
