# Anamnesis Architecture

## Overview

Anamnesis is a system for capturing, storing, and retrieving context from AI coding agent sessions. It provides persistent memory across sessions through a combination of:

1. **Lifecycle Hook Capture** - Sanitized observations from agent sessions
2. **SQLite Storage** - Reliable, queryable persistence
3. **Git-Versioned Wiki** - Human-readable, version-controlled markdown
4. **LLM Consolidation** - Synthesis of observations into coherent knowledge
5. **MCP Server** - Integration with Claude Code and other agents

## Crate Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      CLI / HTTP server                      │
│               (anamnesis-cli, anamnesis-web)                │
└────────────────────────────┬────────────────────────────────┘
                             │
            ┌────────────────┼────────────────┐
            │                │                │
┌───────────▼────────┐  ┌────▼──────────┐   │
│  Consolidate       │  │  Workstreams  │   │
│  (model optional)  │  │  (core+store) │   │
└────────┬───────────┘  └────┬──────────┘   │
         │                    │               │
         └────────────────────┼───────────────┘
                              │
         ┌────────────────────┼────────────────┐
         │                    │                │
    ┌────▼────────┐  ┌───────▼──────┐  ┌────▼──────┐
    │   LLM       │  │   Hooks      │  │   MCP     │
    │ (Providers) │  │  (Capture)   │  │  (Server) │
    └────┬────────┘  └───────┬──────┘  └────┬──────┘
         │                   │              │
         └───────────────────┼──────────────┘
                             │
          ┌──────────────────┼──────────────────┐
          │                  │                  │
    ┌─────▼─────┐    ┌──────▼──────┐    ┌────▼────┐
    │   Wiki    │    │   Store     │    │  Core   │
    │ (Git-FS)  │    │  (SQLite)   │    │ (Types) │
    └───────────┘    └─────────────┘    └─────────┘
```

## Crates

### anamnesis-core
Core data types and abstractions used throughout the system.

**Provides:**
- Common types (Session, Observation, Entity, etc.)
- Error types
- Configuration structures
- Trait definitions

### anamnesis-store
SQLite storage layer with database migrations.

**Provides:**
- Database schema and migrations
- Query builders
- Transaction management
- Observation and session persistence

### anamnesis-wiki
Git-versioned markdown file management.

**Provides:**
- Wiki file operations
- Git history tracking
- Markdown parsing
- Entity extraction from markdown frontmatter

### anamnesis-hooks
Lifecycle event capture from agent sessions.

**Provides:**
- Hook payload parsing (Claude Code today; one module per harness)
- Observation creation
- Redaction, applied before an observation reaches storage
- The file paths a tool event names, which is what `[capture] ignore_paths`
  is matched against

### anamnesis-llm
LLM provider abstraction, and the local embedder.

**Provides:**
- Provider trait and one implementation
- Prompt building, with a token budget that trims long sessions from the middle
- Structured output enforced by a JSON schema
- Local embeddings (candle, CPU, all-MiniLM-L6-v2), off unless
  `ANAMNESIS_EMBED_ENABLED=1`

**Supports:**
- Anthropic Messages API. `ANAMNESIS_LLM_BASE_URL` points it at a gateway
  that speaks the same wire format; there is no OpenAI or Ollama provider.

Requests are not streamed: consolidation is one POST per finished session,
made after the hook's response has already been sent.

### anamnesis-consolidate
Session consolidation and summary generation.

**Provides:**
- Observation analysis
- Summary generation
- Wiki page synthesis
- Handoff creation

### anamnesis-mcp
Model Context Protocol server implementation.

**Provides:**
- MCP tool definitions
- Tool handlers
- Session management
- Integration with Claude Code

**Tools:**
- `memory_query` - Search wiki
- `memory_write_page` - Create wiki pages
- `memory_handoff_accept` - Accept session handoff
- `workstream_start` - Start or resume a named thread of work
- `workstream_status` - A workstream's status and event ledger

Workstreams (cross-harness threads of work, session linking, per-thread
resume, and the visible event ledger) are not a separate crate: their types
live in `anamnesis-core::workstream`, their persistence in `anamnesis-store`,
and their tool surface here — the same split `Page` and `Handoff` already
follow.

### anamnesis-web
The HTTP server hooks deliver to.

**Provides:**
- `POST /hook` — accept one lifecycle event, record it, return 202
- `GET /handoff` — hand the next session what the last one left
- `GET /health`
- The consolidation pipeline, which runs *after* the response is sent

> There is no web UI: no wiki browser, no search page, no git visualization.
> There is also no authentication — bind to loopback, which is the default.

### anamnesis-cli
Command-line interface.

**Provides:**
- `status`, `init`, `serve`, `mcp`, `hook`, `install-hooks`
- `search`, `write-page`, `show-page`, `sessions`, `handoff`
- `reindex` — rebuild the index from `wiki/` and `raw/`
- `bootstrap` — seed a new project's memory from its git history
- `sweep` — forget pages that have decayed; reports unless `--apply`

## Data Flow

### Session Capture

```
Agent Session
    │
    ├─→ Hook Event (SessionStart, UserPromptSubmit, PostToolUse, …)
    │
    ├─→ anamnesis-hooks (Redact & validate)
    │
    └─→ anamnesis-store
         │
         ├─→ SQLite index   (authority for this request)
         │
         └─→ raw/ spool     (append-only JSONL, outlives the index)
```

The order matters: the index is written first because the request depends on
it, then the transcript. A spool failure is logged and stepped over — losing
the durable copy is bad, losing the event because a disk filled up is worse.
The spool refuses an observation that has not been redacted, since it is the
longest-lived copy in the system.

### Consolidation

```
Session Ends
    │
    ├─→ anamnesis-consolidate (Collect observations)
    │
    ├─→ anamnesis-llm (Generate summary)
    │
    ├─→ anamnesis-wiki (Create/update pages)
    │
    └─→ Git repo (Commit changes)
```

### Retrieval

```
Next Session Starts
    │
    ├─→ anamnesis-mcp (memory_query) or `anamnesis search`
    │
    ├─→ four independent streams
    │     ├─ FTS5 over pages_fts
    │     ├─ entity matching, weighted by inverse frequency
    │     ├─ link neighbours, over page_links
    │     └─ vector cosine (only when the local embedder is enabled)
    │
    ├─→ reciprocal-rank fusion (anamnesis_core::retrieval, a pure function)
    │
    └─→ Return results to agent
```

Tier is a bounded signal applied *after* candidates are generated, never an
independent retriever: otherwise a targeted search for something said once in
one session would be buried under durable pages that merely outrank it.

### Forgetting

```
anamnesis sweep [--apply]
    │
    ├─→ every page of one project, with its retention facts
    │
    ├─→ anamnesis_core::sweep::judge (pure)
    │     ├─ exempt: pinned, durable tier, canonical, do-not-answer-from
    │     ├─ expired: past the author's own `expires_at`
    │     └─ scored: salience·e^(-λ·age) + σ·ln(1+reads)·e^(-μ·disuse)
    │
    └─→ --apply: drop the index rows, then remove the pages in one commit
```

Forgetting is a feature: without it the wiki accumulates every session summary
ever written, and retrieval quality falls as the corpus grows. Four properties
make it safe to leave in a system that is supposed to remember things.

**Reporting is the default.** Nothing is deleted without `--apply`. A
retention threshold is a guess until it has been run against a real wiki.

**Being read is what keeps a page.** Retrieval records an access, and the
access term outweighs age — a page someone searched for last week does not
decay out from under them, however old it is.

**Four kinds of page are never swept.** Pinned pages, durable tiers
(`semantic`, `procedural`), canonical pages, and `do-not-answer-from` pages.
The last is the least obvious: a page recording a known-wrong belief is almost
never *retrieved*, so scoring alone would sweep it first — precisely once it
has been quiet long enough for someone to make the mistake again.

**The index goes before the wiki.** Interrupted between the two steps, a sweep
leaves pages that are briefly unfindable and that `anamnesis reindex` restores
in full; the opposite order would leave the index pointing at markdown that no
longer exists. And because the wiki is a git repository, every page a sweep
deletes stays in its history — the commit names each page and why it went.

An `expires_at` that has passed forgets a page whatever its score, but does
not override an exemption: a page that is both pinned and expired is a
contradiction between two things the same author wrote, and the sweep reports
it rather than picking a winner in silence.

## Storage Schema

Twelve tables across six migrations (`V01`–`V06`), the authoritative copy of
which is `crates/anamnesis-store/migrations/`. Every timestamp is RFC 3339
with an explicit `Z`, always written from Rust — no column carries a SQL
default, because SQLite's `CURRENT_TIMESTAMP` has a different shape and
mixing the two would break both parsing and lexicographic ordering.

Identifiers are UUIDv5 derived from what they name — a page from
`(project, path)`, a session from `(project, agent session id)` — which is
what makes the whole index disposable and rebuildable.

### sessions
`id`, `project_id`, `agent`, `checkout_path`, `state` (`open` / `ending` /
`closed`), `started_at`, `ended_at`, `workstream_id`.

Sessions reference the project only; the workspace is reachable through it,
so there is no way to record a session whose workspace and project disagree.

### observations
`id`, `session_id`, `kind`, `tool_name`, `tool_ok`, `at`, `body`,
`truncated`, `sanitized`.

### pages
`id`, `project_id`, `path`, `title`, `body`, `tier`, `status`, `pinned`,
`canonical`, `supersedes`, `is_latest`, `salience`, `access_count`,
`last_accessed_at`, `expires_at`, `git_commit`, `created_at`, `updated_at`.

Entities are *not* a column: they live in `entities` and `page_entities`, so
the entity retrieval stream can weight them by inverse frequency. Links live
in `page_links`, which is what makes link-neighbour retrieval possible.

### The rest
`projects`, `pages_fts` (FTS5, unicode61), `entities`, `page_entities`,
`page_links`, `handoffs`, `page_feedback`, `page_embeddings`, `workstreams`.

## Configuration

Projects use `.anamnesis.toml` for configuration. The file is optional: without
it, the project identity is derived from the git remote.

```toml
[scope]
workspace = "default"
project = "anamnesis"

[capture]
ignore_paths = ["target/**", "node_modules/**"]

# Retention. Half-lives rather than rates, because a half-life is something
# someone can hold an opinion about.
[decay]
threshold = 0.05                        # forget below this retention score
age_half_life_days = 30.0
access_half_life_days = 14.0
access_weight = 0.5

[slots]
per_user = false

[auto_improve]
enabled = true
require_approval = true

# A single table. `[[auto_improve.scheduler]]` declares an array of tables and
# is rejected at load time rather than being silently ignored.
[auto_improve.scheduler]
enabled = false
interval_minutes = 60
```

Unknown keys are an error, so a typo surfaces instead of quietly sending memory
to the wrong project. `.ai-memory.toml` is still read as a fallback filename for
projects migrating from upstream `ai-memory`.

Of the tables above, `[scope]`, `[capture]`, and `[decay]` change what the
system does. `[slots]` and `[auto_improve]` are parsed and validated, and then
nothing reads them; they are accepted so that a file written for a later
version does not fail to load, not because they take effect.

### Data directory

Memory lives outside the repository it describes, so the wiki can carry its own
git history:

```text
<data_dir>/
  wiki/     git-versioned markdown, the source of truth
  raw/      immutable sanitized transcripts
  db/       SQLite indexes, rebuildable from wiki/
  models/   local embedding models
  logs/     rolling trace output
```

Resolution order: `--data-dir`, then `ANAMNESIS_DATA_DIR`, then the platform
data directory.

## Security Considerations

**Implemented:**

- **Redaction** — observations are scrubbed of secret-shaped text before they
  reach either the index or the spool, and the spool rejects anything that
  arrives unredacted.
- **Capture exclusions** — `[capture] ignore_paths` drops events naming the
  paths a project has excluded, before an observation is built. Nothing about
  them reaches the index, the spool, or a later summary. Patterns are
  gitignore-shaped and matched case-insensitively; an event naming several
  files is dropped if any one of them is excluded. Only paths a tool input
  names outright are matched — a shell command that mentions a path is not
  parsed, so exclusion is not a substitute for redaction.
- **Loopback by default** — `anamnesis serve` binds `127.0.0.1`.
- **Path containment** — `PagePath` rejects absolute paths, `..`, drive
  letters, and backslashes, so no page written through it can escape its
  project directory.

**Not implemented — do not rely on these:**

- **Bearer-token auth** — there is no authentication on the HTTP server.
- **`[slots] per_user`** — every session shares one scope.

## Future Enhancements

Shipped since this list was written: vector embeddings (local candle
embedder, opt-in), the raw spool, `reindex`, `bootstrap`, and the decay sweep.

Still ahead:

1. **Graph Database** - Entity relationship tracking
2. **Multi-Agent Coordination** - Shared context between agents
3. **Policy Engine** - Fine-grained access control
4. **Audit Trail** - Complete action logging
5. **Auto-improve** - The scheduler the configuration already describes
6. **Evals** - The empty crate at `evals/`
