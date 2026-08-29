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
  `ANAMNESIS_EMBED_ENABLED=1`. When on, every path that writes a page embeds
  it — consolidation, the wiki watcher, `write-page`, `bootstrap`, `reindex`
  and the MCP tool — because a vector stream that covered only one of them
  would answer differently depending on which command happened to write a page

**Supports:**
- Anthropic Messages API.
- The OpenAI chat-completions API, and everything that speaks it: OpenAI,
  Ollama, vLLM, LM Studio, any gateway presenting `/chat/completions`. One
  client rather than two, because they are one wire format — `ollama` differs
  from `openai` only in its default address and in expecting no credential,
  which is what lets a model on the same machine be dropped in where a hosted
  one was.
- `ANAMNESIS_LLM_BASE_URL` points any of them at somewhere else.

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
- `GET /whoami` — what the server makes of the caller's token
- `GET /health`
- `GET /ui` — the wiki browser: scopes, the pages in one, one page
  rendered, and `?q=` to search a scope. Read-only, and `serve --no-ui`
  leaves it out
- The consolidation pipeline, which runs *after* the response is sent

> There is no search page and no git visualization; the browser lists and
> renders, and nothing there writes.

**Bearer tokens.** With `ANAMNESIS_TOKEN` or `ANAMNESIS_TOKENS` set, every
route but `/health` requires `Authorization: Bearer <token>`. With neither set
the server is open, which is what loopback deployments have always been;
binding a non-loopback address that way is refused at startup unless
`--allow-anonymous` says it was meant. `/health` stays open so `anamnesis
status` can tell a server that is down from one that refuses this machine's
token — two problems with different fixes that would otherwise look identical.

**The browser's credential.** `/ui` accepts the same token as an HTTP Basic
password, because a browser will not attach a bearer token to a link somebody
clicked but will ask for a password and remember it. Any username is accepted:
the secret is the whole credential. The API keeps the header-only rule, so a
credential the browser sends on its own cannot authorise `POST /hook` — a page
on another site cannot make somebody's browser write to their memory.

**What the browser deliberately does not do.** Opening a page never records
an access. `query_pages` bumps those counters because retrieval finding a page
useful is evidence about the page, and the decay sweep reads exactly them; a
person clicking through an index is not the same claim, and browsing must not
be able to rescue a page from being forgotten. Searching *does* record one,
here as in `anamnesis search` and `memory_query` — a search hands somebody a
page it chose, which is the act the counter is about. Page bodies come from
the wiki rather than from the index's copy — the file is what a person edits
and what git holds — and raw HTML in a body is rendered as text, since bodies
are written by models and by capture.

**Search is the same call an agent makes.** `?q=` runs `query_pages_across`
with the workspace's shared scope and the opt-in embedder, at the same default
limit, so what a person is shown is what an agent would have been handed. A
browser that ranked pages its own way would be a second retrieval that
`anamnesis eval` does not measure.

### anamnesis-cli
Command-line interface.

**Provides:**
- `status`, `init`, `serve`, `mcp`, `hook`, `install-hooks`
- `search`, `write-page`, `show-page`, `sessions`, `handoff`
- `reindex` — rebuild the index from `wiki/` and `raw/`
- `bootstrap` — seed a new project's memory from its git history
- `sweep` — forget pages that have decayed; reports unless `--apply`
- `forget` — remove named pages on purpose, from the wiki and the index
- `improve` — file and act on proposals about the memory itself

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
    ├─→ anamnesis-llm (Generate summary)      ← optional; counting otherwise
    │
    ├─→ a digest: title, body, handoff, entities
    │
    ├─→ anamnesis-wiki (Create/update pages)
    │
    ├─→ the index: page row, entities, links
    │
    └─→ Git repo (Commit changes)
```

A page says what it is about, not only what it contains. With a model, the
entities are the names it says a later search would type — files, crates,
tools, systems, error names. Without one, they are the basenames of the files
the session worked on, which is what counting can reach. Basenames rather than
paths: an entity matches when every token of its name is in the query, so
`lib.rs` asks for the two tokens someone would type where the full path would
demand six. A name that ends up on half the wiki costs nothing, since entity
weight is inverse to how many pages carry it.

### Retrieval

```
Next Session Starts
    │
    ├─→ anamnesis-mcp (memory_query) or `anamnesis search`
    │
    ├─→ four independent streams
    │     ├─ FTS5 over pages_fts
    │     ├─ entity matching, by token, weighted by inverse frequency
    │     ├─ link neighbours, over page_links, weighted by their seed's rank
    │     └─ vector cosine (only when the local embedder is enabled)
    │
    ├─→ reciprocal-rank fusion (anamnesis_core::retrieval, a pure function)
    │
    └─→ Return results to agent
```

A query searches the project **and** the workspace's shared `_global` scope.
Each is searched on its own and the two rankings are fused, rather than the
streams being widened to select across projects: RRF combines by rank
precisely because scores from different sources are not comparable, and two
projects are two sources — and a canonical page in a five-page global scope
should not outrank one in a five-hundred-page project merely for having less
competition. Ties go to the project, which is the more specific answer.

Tier is a bounded signal applied *after* candidates are generated, never an
independent retriever: otherwise a targeted search for something said once in
one session would be buried under durable pages that merely outrank it.

The numbers fusion is built from live in `anamnesis_core::retrieval::Tuning`,
and since 2026-08-29 they are measured rather than argued (see **Evaluation**):

| | | why |
|---|---|---|
| `rrf_k` | 2 | was 60, the value from the paper — written for fusing runs a thousand deep from engines of comparable quality. Neither holds here: streams are thirty deep and one is far better than the rest. At 60 a stream's whole spread was 1.47×, so a page sitting anywhere in two streams outscored the page one stream was sure of |
| `fts` / `entity` | 1.0 | level with each other. Weighting declared names above the words on the page scored no better and claims more |
| `links` | 0.25 | enough to break a tie between pages full text likes equally, not enough to outvote it. Neighbours of a hit are evidence about the hit |
| `vectors` | 1.0 | measured once the stream was populated on every write path and `eval --embed` could run it. On its own it has the second-best recall of the four (0.700 and 0.533) and takes full text's unique answers from 3 to 1 and from 8 to 2 — it independently reaches most of what only full text reached. Its weight does not separate: 0.5 and 1.0 both appear among the best rows |
| `entity_coverage` | 1.0 | every token of a name has to be in the query, as the stream has always required. Measured now rather than argued: admitting partial matches was never better in two thousand comparisons, and costs the crowded suite 0.967 → 0.889 at this tuning |
| `candidates` | 30 | unchanged, and the one knob whose measurement came back empty: at the tuning above, 10, 30 and 120 score identically on both corpora. Depth only mattered where the rest of the fusion was wrong — a shallower pool left fewer also-rans to outvote the stream that had the answer. The suites are 10 and 22 pages, so nothing here can tell 30 from 120; measuring that needs a corpus larger than the depth |
| `authority_exponent` | 0.25 | the multiplier reached 2.34×, larger than the entire spread of relevance it adjusts, so a canonical page in an authoritative namespace outranked whatever any stream put first. Now about 1.24× — a preference between comparable answers, which is what it was always described as |

Where the sweep was decisive it was followed; where it was indifferent the
design was kept. Silencing the link stream, dropping authority to nothing, and
weighting entities above full text each scored exactly the same as the values
above, and each would have thrown away a signal on the evidence of
twenty-five questions.

Pages that something else has replaced are excluded outright (`is_latest`),
because an agent that recorded "this decision replaces that one" should not be
answered with the decision it replaced. Links to them still resolve, though:
being replaced changes how a page ranks, not whether it exists.

### Evaluation

```
anamnesis eval [--suite FILE] [--streams] [--sweep] [--check]
    │
    ├─→ build a throwaway corpus from the suite's pages
    │     (wiki write → index upsert → entities → links: the live path)
    │
    ├─→ ask each case through Store::query_pages, the call memory_query makes
    │
    └─→ mean reciprocal rank + recall, against the suite's own thresholds
```

Everything else in the workspace is tested for being *correct*. This is the
only thing that asks whether memory is any *good*: whether the page that
answers a question comes back, and comes back near the top.

Two suites ship. `retrieval` asks whether an answer is *reachable*: ten pages,
sparse links, mostly one right answer. `crowded` asks whether it *wins*:
twenty-two pages, a plausible competitor for most questions, half the answers
on pages with no authority at all, and a link cluster dense enough to offer
noise as readily as signal. The second exists to be the corpus no knob is
tuned on — a sweep run against ten questions finds whatever those ten
questions reward.

It earned that role immediately. Under the constants retrieval shipped with,
`crowded` scored MRR 0.436 / recall 0.533 while plain full-text search alone
scored 0.900 / 0.933 over the same questions: fusion was burying the answers
one stream had ranked first. Nothing on the smaller suite showed it, where
fusion gained recall and looked like it was working.

Three constraints, all of them deliberate:

**The corpus is checked in, never real memory.** A score is only readable if
the thing it scored is identical on every machine. There is a second reason:
`query_pages` records an access for every page it returns, and the decay sweep
reads exactly that number to decide what to keep — a hundred eval queries
against a real index would look like a hundred afternoons of finding those
pages useful.

**No model is required.** The embedding stream is opt-in in production and
absent here, so a score never depends on a download having succeeded.

**The thresholds live in the suite file.** A change that costs recall has to
edit a number in the diff rather than a number nobody looks at. The built-in
suite is run as an ordinary unit test, so CI fails on a regression without a
job of its own.

`--streams` scores each stream separately, and reports the measure that
actually decides whether one stays: how many questions **only** it answers. A
stream with a respectable average and nothing unique behind it is one the
others already cover.

`--sweep` scores the same questions once per candidate setting, building each
corpus once and querying it through the same call the server makes. The rule
for accepting a setting is in the code rather than in whoever reads the table
(`SweepPoint::improves_on`): rank up **and** recall held, on *every* suite. The
mean across corpora is a sort key and nothing else — deciding by it is how a
gain on one corpus pays for a loss on another. Every knob spans values on both
sides of the shipped one, because a grid whose best row sits on its own edge
has found a direction, not an optimum.

The tuning it produced took `retrieval` from 0.708 to **1.000 / 1.000** and
`crowded` from 0.436 / 0.533 to **0.967 / 1.000** — the second now above the
0.900 that full text reaches alone, which is the only thing that makes fusing
four streams worth doing.

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

### Improving

```
anamnesis improve            server tick, every 60s
    │                            │
    │                            ├─→ each project: is its interval elapsed?
    │                            │     (read from its own marker file)
    ▼                            ▼
  anamnesis_core::improve::propose (pure)
    ├─ promote-tier        an episodic page several sessions came back to
    └─ write-missing-page  a link target no page answers to
    │
    ├─→ file, refresh, or resolve — identity is (project, kind, subject)
    │
    └─→ require_approval = false: carry out what can be carried out
```

The other half of forgetting. A sweep removes what nobody needs; a pass
notices what the memory has earned or is missing, from signals it already
records rather than from a model's opinion.

**Promotion is the interesting one.** Retrieval records every hit, so an
`episodic` page that later sessions kept coming back to is knowledge filed as
a session note. Promoting it to `semantic` says so — and makes it exempt from
the sweep, which is how a page becomes durable by proving itself instead of by
someone remembering to pin it. That is also why approval is the default: it is
a retention decision, not a tidy-up.

**A proposal is an observation, not a task.** Its identifier is derived from
`(project, kind, subject)`, so a later pass lands on the row it already filed.
Dismiss one and it stays dismissed; do the thing yourself and the next pass
resolves it, because the condition it described has stopped holding.

**Not everything can be carried out.** Writing a missing page is not
mechanical — nothing can invent what it should say — so that kind waits for a
person however the project has set `require_approval`.

**The schedule belongs to the project, not the server.** One server serves
every project that talks to it, and each keeps its own `[auto_improve]` table.
The loop ticks every 60 seconds and asks each project whether *its* interval
has elapsed since *its* last pass, which is recorded in the index — so
restarting the server does not restart everyone's clock, and a project whose
marker file cannot be found or read is skipped with a reason rather than
improved on defaults it never chose.

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
`canonical`, `supersedes`, `supersedes_target`, `is_latest`, `salience`,
`access_count`, `last_accessed_at`, `expires_at`, `git_commit`, `created_at`,
`updated_at`.

Supersession is stored twice on purpose, the same way a wikilink is:
`supersedes_target` is the path a page *authored* and `supersedes` is the row
it resolved to. Either page can be written first — a rebuild visits paths in
an order nobody chose — so the claim has to survive naming a page the index
has not seen yet and resolve when it arrives. `is_latest` is derived from
those claims rather than asserted by whoever wrote last, which is what
retrieval filters on: a replaced page stops being offered, without anything
having to edit the markdown someone else wrote.

Entities are *not* a column: they live in `entities` and `page_entities`, so
the entity retrieval stream can weight them by inverse frequency. Links live
in `page_links`, which is what makes link-neighbour retrieval possible.

### proposals
`id`, `project_id`, `kind`, `subject`, `page_id`, `rationale`, `state`
(`open` / `applied` / `dismissed` / `resolved`), `created_at`, `decided_at`.

The identifier is derived from `(project, kind, subject)`, which is what makes
a decision stick: a later pass that notices the same condition arrives at the
same row rather than filing a second copy of it.

### The rest
`projects` — which also records each project's working copy and when it was
last improved, so a scheduler can find its settings and honour its interval —
plus `pages_fts` (FTS5, unicode61), `entities`, `entity_tokens`,
`page_entities`, `page_links`, `handoffs`, `page_feedback`, `page_embeddings`,
`workstreams`.

An entity is stored twice, like a supersession and like a link: `entities.name`
is what someone wrote, and `entity_tokens` is that name split the same way a
query is. A query is tokenized before it is matched, so a name kept whole could
only be found by a query that tokenized to exactly it — never, for anything
containing a space, a dash, or a dot. An entity matches when every one of its
tokens is present in the query.

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

# Whether a pass may look at all, and whether it may act without being asked.
[auto_improve]
enabled = true
require_approval = true

# Off unless a project asks. The server checks every 60 seconds whether this
# interval has elapsed since this project's last pass.
#
# A single table. `[[auto_improve.scheduler]]` declares an array of tables and
# is rejected at load time rather than being silently ignored.
[auto_improve.scheduler]
enabled = false
interval_minutes = 60
```

Unknown keys are an error, so a typo surfaces instead of quietly sending memory
to the wrong project. `.ai-memory.toml` is still read as a fallback filename for
projects migrating from upstream `ai-memory`.

Every table above changes what the system does; none is inert. `[slots]
per_user` keys the pending-handoff slot by the operator a request's bearer
token names, so two people sharing a server stop being handed each other's
"where I left off". A caller the server cannot name has no operator and uses
the shared slot, which is what every unauthenticated install already had.

### Data directory

Memory lives outside the repository it describes, so the wiki can carry its own
git history:

```text
<data_dir>/
  wiki/     git-versioned markdown, the source of truth
  raw/      immutable sanitized transcripts
  db/       SQLite indexes, rebuildable from wiki/
  models/   local embedding models
  logs/     rolling trace output, one file a day, written by `serve`
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

- **Per-operator wiki scopes** — `[slots] per_user` separates handoff slots,
  not pages. Two operators on one project read and write the same wiki.

## Future Enhancements

Shipped since this list was written: vector embeddings (local candle
embedder, opt-in), the raw spool, `reindex`, `bootstrap`, the decay sweep, and
auto-improve with its scheduler.

Still ahead:

1. **Graph Database** - Entity relationship tracking
2. **Multi-Agent Coordination** - Shared context between agents
3. **Policy Engine** - Fine-grained access control
4. **Audit Trail** - Complete action logging
