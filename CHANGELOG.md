# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed
- A model that escapes its own newlines gets them back. Seen in a real reply
  from a local model: every paragraph break in the handoff was the two
  characters `\` and `n`, so the JSON was valid, the fields were non-empty
  strings, every check passed, and the next session would have been handed one
  unbroken wall of text with the escapes printed in it. Unescaped only when the
  text contains no real newline at all — a page that has line breaks *and*
  writes the sequence is explaining it, most likely in code, and rewriting that
  would corrupt the one thing it was trying to say
- The hook says, where it will be read, that capture is not working. This
  repository's own memory recorded nothing for four days: the server was not
  running, every hook failed to connect, and the only report was a line on
  stderr from a process that exits zero, which no harness surfaces. A session
  that starts while the server is unreachable now says so through stdout — the
  channel the handoff already uses — naming itself, so it cannot be read as
  memory. A handoff that fails when capture is working gets a different
  sentence, because sending someone to restart a running server is its own
  false alarm. Every path out of the hook now goes through one function:
  a failed POST used to return early and print nothing, so Gemini CLI, which
  parses stdout as one JSON object on every event, got silence exactly when
  the server was down
- `serve` writes to `logs/`, which the data-directory layout has documented as
  "rolling trace output" since the first commit with nothing ever written to
  it. The server is the one command nobody watches — it runs for days in a
  terminal that gets closed — so when it stopped there was no way to say when
  or why. One file a day, fourteen kept, written straight through rather than
  buffered, because what a buffer loses is the last few lines before a crash
- Link-neighbour expansion no longer throws away the rank of the page it
  expanded from. Neighbours were ordered by `COUNT(*)` of the edges reaching
  them, so a neighbour of the best full-text hit and a neighbour of the
  thirtieth ranked identically, and two neighbours of the thirtieth outranked
  one neighbour of the first. Each edge now counts for `1 / (k + rank)` of its
  seed — the same reciprocal-rank form fusion uses, and the same constant,
  because both are answering how much a ranking's order should matter. The
  stream's own MRR on the crowded suite rises from 0.178 to 0.256; what it is
  worth in the fusion was re-measured and left at 0.25, which is the weight
  a stream that answers no question alone should carry. Ties within the stream
  are broken by page id rather than by whatever order SQLite returned

### Changed
- The vector stream is measured, and stays where it is. `anamnesis eval
  --embed` embeds the corpus page by page and every question with the same
  model, through the same call the server makes. On its own the stream has the
  second-best recall of the four (0.700 on the retrieval suite, 0.533 on the
  crowded one) and takes full text's *unique* answers from 3 to 1 and from 8 to
  2 — it independently reaches most of what only full text reached. Its weight
  does not separate: the best rows use 0.5 and 1.0 alike, so 1.0 stays. Nothing
  else changes either: the twelve settings that beat what ships all need
  embeddings on **and** a shallower candidate pool, and a corpus of ten and
  twenty-two pages cannot say whether a shallow pool is safe on a real wiki
- The sweep's acceptance rule was wrong at the ceiling. It required a rise on
  *every* suite, which was written before either suite could reach a perfect
  score; once `retrieval` sat at 1.000 nothing could raise it, so a setting
  that took `crowded` from 0.967 to 1.000 was reported as no improvement, and
  six thousand rows produced none. It now asks that nothing falls anywhere and
  something rises
- There is one way to index a page, and every path that writes one embeds it.
  `Store::index_page` writes the row, the entities, the links and — when an
  embedder is enabled — the vector; five hand-written copies of that sequence
  became calls to it, and the two rebuilds that resolve links in a second pass
  keep their own sequence and say so. The copies had drifted twice: once when
  the live path wrote pages without their links, so the link-neighbour stream
  was blind to everything this system wrote for itself, and again in a way that
  was still true — exactly one of the seven embedded anything, so switching the
  embedder on bought a vector stream over the pages an agent had written
  through `memory_write_page` and nothing else. No session summary, no
  bootstrap page, no hand edit. `serve` now builds the embedder the MCP server
  already built, and says on startup whether it has one; a rebuild embeds too,
  or a wiki rebuilt from disk would answer differently from the same wiki
  written page by page
- The entity stream's rule that *every* token of a name must appear in the
  query is a setting rather than a fact of the SQL, and was swept like the
  rest. It stays: admitting partial matches was never better in two thousand
  comparisons across both corpora, and at the tuning that ships it costs the
  crowded suite 0.967 → 0.889. The rule was written on an argument — a
  two-word name answering one word would drown the streams it is fused with —
  and that argument now has a number behind it. Partial matches, when enabled,
  rank by how complete they are rather than beside a name said in full
- Candidate depth — how deep each stream reaches before fusion — is part of the
  tuning rather than a constant, and was swept along with everything else. The
  answer came back empty: at the tuning that ships, 10, 30 and 120 score
  identically on both corpora, so it stays at 30. Depth only mattered where the
  rest of the fusion was wrong, a shallower pool leaving fewer also-rans to
  outvote the stream that had the answer — which is worth knowing, because it
  is the shape of a fix somebody would otherwise reach for. The suites are 10
  and 22 pages, so nothing in them can tell 30 from 120
- **Retrieval is tuned against measurement rather than argument.** The RRF
  constant is 2 rather than 60, the link stream is weighted a quarter, and the
  authority multiplier is applied at a quarter power (about 1.24× rather than
  2.34×). Under the old constants a page sitting anywhere in two streams
  outscored the page one stream had ranked first — at `k = 60` a stream's whole
  thirty-deep spread is 1.47×, and the authority multiplier alone was larger
  than that — so on a corpus with enough linked, entity-bearing pages, fusion
  buried the answers full-text search had found. The shipped suites go from
  0.708 / 1.000 to **1.000 / 1.000**, and from 0.436 / 0.533 to **0.967 /
  1.000**: the second now scores above the 0.900 full text reaches on its own,
  which is the only thing that makes fusing four streams worth doing. Where the
  sweep was indifferent the design was kept — the link stream is quietened
  rather than silenced, entities stay level with full text, and canonical pages
  are still preferred

### Added
- Search in the wiki browser: `?q=` on a scope, which is where the page list
  already was. It runs `query_pages_across` — the workspace's shared scope
  included, the opt-in embedder with it, at the same default limit — so what a
  person is shown is what an agent asking the same question would have been
  handed, rather than a second retrieval nothing measures. Hits say which
  scope they came from, because a policy that applies to every project and a
  note about this one are different kinds of answer and the path does not say
  which is which. Unlike opening a page, a search *does* record an access for
  what it returns: it hands somebody a page it chose, which is the act those
  counters are about, and `anamnesis search` has always recorded it
- The workspace's shared scope is derived in one place, `Wiki::global_scope`.
  Where its pages sit and what project identifier its rows carry have to
  agree between every reader, or a page written through one is invisible to
  the other; the MCP server now asks the wiki instead of rebuilding the path
- A wiki browser at `/ui`, served by `anamnesis serve`. Until now memory could
  only be read by asking it something — `search`, `show-page`, or an agent's
  MCP query — which meant a stale page, a summary the model wrote badly, and a
  page that never got indexed all looked identical from outside. Three routes:
  the scopes this server holds, the pages in one, and one page rendered.
  It is read-only, and `serve --no-ui` leaves it out: it is the only part of
  this server that can read the whole of a memory, where the API accepts events
  and delivers a single handoff. Three deliberate limits. It never records a
  page access, because the decay sweep reads exactly those counters and
  browsing an index is not the claim that retrieval found a page useful.
  Bodies come from the wiki rather than the index's copy of them, since the
  file is what a person edits and what git holds. And raw HTML in a body is
  shown as text with non-`http(s)`/`mailto` link destinations defused, because
  a page body is written by models and by capture. `[[wiki links]]` become
  links, and ones with no page behind them are marked rather than hidden —
  the same signal `improve` turns into a proposal
- The browser's credential: `/ui` also accepts the server's token as an HTTP
  Basic password, so a token-protected server is still openable. A browser
  cannot be asked to attach a bearer header to a link somebody clicked, but it
  will ask for a password. Any username is accepted — the secret is the whole
  credential. The API is unchanged and stays header-only, so a credential the
  browser attaches by itself cannot authorise `POST /hook`
- `anamnesis handoff --discard` throws away the note waiting for the next
  session. A handoff written from a bad model reply had exactly one way out:
  let a session claim it, which puts it in that session's context — the thing
  being avoided. It prints what it dropped, and the row is kept and marked
  expired, the same state a newer handoff already puts an older one in, because
  a record saying a note was written and never delivered is more honest than no
  record. Slots are separate here as everywhere: discarding one operator's note
  leaves everyone else's pending
- `anamnesis forget <path>...` removes named pages from the wiki and the index.
  `sweep` forgets what decayed; nothing forgot what was *wrong*. A page written
  from a bad model reply, a note that turned out to be untrue, a duplicate —
  the only ways out were to wait for a decay that never comes for a pinned or
  durable page, or to delete the file by hand and hope the watcher was running
  to notice. Index row first and file second, the recoverable order; every path
  resolved before anything is removed, so one typo does not leave a half-done
  job; and the commit it prints still holds the content, because the wiki is a
  git repository
- An OpenAI-compatible provider, which is also the Ollama one: `openai` and
  `ollama` are one client over one wire format, differing in their default
  address and in whether a credential is expected. A model running on this
  machine has none to present, and requiring one would have made the only
  configuration that costs nothing — and sends nobody's transcript anywhere —
  impossible to express. Verified against Ollama before it was written: a
  `response_format` carrying a JSON schema is honoured, and unknown request
  fields are ignored, which is what lets the same body carry `reasoning_effort`
  to a backend that has never heard of it. Two failure modes are named rather
  than guessed at: a reasoning model that spends its whole budget thinking
  returns HTTP 200 with an empty answer, and a structured-output refusal
  arrives as a field on a success rather than as an error status
- `anamnesis install-mcp` registers with Cursor, Gemini CLI and Codex as well
  as Claude Code, each format checked against its own documentation first, as
  the hooks were. Three of them keep the same `mcpServers` object in different
  files — Gemini's being the settings file its hooks already live in, where
  only that one key is touched — and Codex keeps TOML under `mcp_servers`,
  merged with `toml_edit` so an existing `config.toml` comes back with its
  comments and key order intact. OpenCode is refused with the reason its hooks
  are refused
- `anamnesis install-mcp` registers the MCP server with a harness, the half of
  connecting an agent that had no command. Hooks had one because setup steps
  nobody writes down fail silently; MCP had a line of documentation that
  assumed the binary was on `PATH`. On the machine this project is developed on
  it is not — it is copied out of `target/` so `cargo build` can overwrite it —
  so following that line would have registered a server that cannot start. It
  was never run at all: four months of captured sessions the agent could not
  search, and nothing said so. The registration names the executable that ran
  the command, merges rather than replaces, is idempotent, refuses to touch a
  file it cannot parse, and replaces a stale entry of its own only while saying
  what it replaced
- A second eval corpus, `crowded`: twenty-two pages, a plausible competitor for
  most questions, half the answers on pages with no authority, and a link
  cluster dense enough to offer noise as readily as signal. It exists to be the
  set no knob is tuned on, and it found the fusion defect above on its first
  run — which the ten-page suite could not show, because there fusion gained
  recall and looked like it was working
- `anamnesis eval --sweep` scores the same questions once per candidate
  setting, through the same call the server makes. The rule for accepting one
  is in the code rather than in whoever reads the table: rank up **and** recall
  held, on every suite. An eval fixture can now say that one page replaces
  another, so a corpus can ask what a wiki asks whenever somebody revises a
  decision
- Initial project structure as Rust workspace
- Core data types and abstractions
- SQLite storage layer with migrations `V01`–`V10`
- Git-versioned wiki system
- MCP server: `memory_query`, `memory_write_page`, `memory_handoff_accept`
- Lifecycle hooks capture system, with redaction before storage
- LLM provider abstraction (Anthropic Messages API), optional throughout
- Session consolidation, deterministic when no model is configured
- HTTP server for hook delivery and handoff pickup (`/hook`, `/handoff`,
  `/whoami`, `/health`) — no UI
- The workspace-wide `_global` scope is read. The data directory has reserved
  it since the beginning and the layout was designed around it, but retrieval
  answered from the current project only, so anything written there was a file
  nobody read. A query now searches the project and the shared scope as two
  rankings fused into one — ties going to the project, which is the more
  specific answer — and a hit says which scope it came from. `write-page
  --global` writes there, `reindex` rebuilds it alongside the project, and the
  wiki watcher indexes a page edited there by hand. One shared scope per
  workspace, and inheritance rather than merging: nothing is copied into a
  project
- `anamnesis write-page` reaches the rest of a page: `--tier`, `--status`,
  `--canonical`, `--entity`, and `--supersedes`, which the MCP tool has always
  accepted and the CLI could not. A page written from the command line can now
  be durable, authoritative, or a replacement for another, and it says which it
  was written as. Its entities reach the index too — the command wrote none
  before, so a page written this way was reachable through its words alone
- `install-hooks` wires Cursor, the first harness whose payload differs rather
  than only its event names. It identifies a session by `conversation_id` and
  sends `session_id` on only some events, with different values — keyed on the
  latter, one Cursor session would have been recorded as two, its boundaries in
  one and its work in another. It gives the working directory as
  `workspace_roots` except on tool events, and serialises tool results as a
  JSON string rather than an object. Its `hooks.json` declares a schema
  version, which `install-hooks` writes when the file is silent and never
  overwrites, and it takes injected context back as a top-level
  `additional_context`
- `install-hooks` wires Gemini CLI. The same five moments under four different
  names — `BeforeAgent` when a prompt is submitted, `AfterTool` when one
  finishes, `PreCompress` before the context goes — which the parser now reads
  as the boundaries they are. The way back differs too: Gemini parses a hook's
  stdout as one JSON object and rejects anything else, so the handoff travels
  as `hookSpecificOutput.additionalContext`, and every other event prints an
  empty object rather than nothing
- `install-hooks` wires Codex CLI as well as Claude Code. Codex reads
  `.codex/hooks.json`, names the same five lifecycle events, and delivers a
  payload with the same field names, so nothing downstream had to learn a
  second shape — and what a `SessionStart` hook prints on stdout becomes
  developer context there too, which is how the handoff arrives. An agent that
  cannot be wired this way is now told why rather than "not yet": OpenCode
  extends through a TypeScript plugin API, not a command hook
- `anamnesis eval --streams` scores each retrieval stream on its own and names
  the cases only it can answer — the measure that decides whether a stream
  earns its place, which a fused ranking cannot show. `Store::query_streams`
  is the diagnostic behind it, and deliberately records no access: asking
  which stream *would have* found a page is not somebody reading it, and the
  decay sweep reads those counters. First run over the shipped suite: full
  text alone scores a higher MRR than the fused ranking (0.800 against 0.708)
  while missing a fifth of the questions — fusion is buying recall with rank,
  and the link stream answers nothing on its own that the others miss
- `anamnesis eval` — retrieval scored against a checked-in corpus and the
  questions asked of it, through the same `query_pages` call `memory_query`
  makes. Reports mean reciprocal rank and recall against thresholds the suite
  file declares, so a change that costs recall has to edit a number in the
  diff. The corpus is built in a throwaway directory, never real memory: every
  query would otherwise count as a read, and the decay sweep believes those.
  The shipped suite runs as an ordinary unit test, so CI fails on a regression
  without a job of its own
- `[slots] per_user` is enforced rather than merely parsed. A project that
  sets it keeps one pending handoff per operator, so two people sharing a
  server are each handed what their own last session left instead of whichever
  note was written last. The operator comes from the bearer token a request
  presents; callers the server cannot name share the one slot they always
  shared, and a project that has not set it is unchanged. Sessions record whose
  they were either way, `anamnesis sessions` shows it, `anamnesis handoff
  --operator` peeks one slot, and `memory_handoff_accept` takes an `operator`
- Bearer-token authentication. `ANAMNESIS_TOKEN` is the secret a machine
  presents; `ANAMNESIS_TOKENS` is the `name=secret` set a server accepts, so a
  shared server can tell whose session it is recording. With neither set the
  server is open, as it always was — except on a non-loopback bind, which is
  refused unless `--allow-anonymous` says it was meant. `/health` stays open so
  `anamnesis status` can tell a server that is down from one that is refusing
  this machine, and says which on its `Auth:` line. `anamnesis token` mints a
  secret and stores nothing
- CLI entry point
- Cross-harness workstreams: named threads of work with per-thread handoff
  slots, plus the `workstream_start` and `workstream_status` MCP tools
- Retrieval over four fused signals: FTS5, entities, link neighbours, and an
  opt-in local embedder (`ANAMNESIS_EMBED_ENABLED=1`)
- Raw spool: every observation appended to `raw/` as immutable JSONL
- `anamnesis reindex` — rebuild the index from `wiki/` and `raw/`
- `anamnesis bootstrap` — seed a new project's memory from its git history
- CI: fmt, clippy, and tests on Linux and Windows for every push and PR
- `[capture] ignore_paths` is enforced: events naming an excluded path are
  dropped before an observation exists, so nothing about them reaches the
  index, the spool, or a summary
- `anamnesis sweep` — forget pages that have decayed below a retention
  threshold, or whose `expires_at` has passed. Reports and changes nothing
  without `--apply`; pinned, durable, canonical, and `do-not-answer-from`
  pages are never swept; deleted pages remain in the wiki's git history, in
  one commit that names each page and why it went
- `[decay]` in `.anamnesis.toml` — retention tuning as half-lives, read by
  the sweep and refused at load time when a value would make it nonsense
- Session pages name their entities. Consolidation produces them in both
  modes — the names a model says a later search would type, or the basenames
  of the files the session touched when no model is configured — so the entity
  retrieval stream finally sees the pages the system writes for itself
- `anamnesis improve` — file proposals from what the index already records: a
  page several sessions kept coming back to should be durable, and a page
  several pages link to should exist. Proposals are identified by what they
  are about, so a dismissal sticks and a condition someone fixed themselves
  resolves
- `[auto_improve]` is enforced rather than merely parsed: `require_approval`
  decides whether a pass may carry out its own applicable proposals, and
  `[auto_improve.scheduler]` makes the server run that pass per project, on
  that project's interval, measured from its last pass rather than from
  server start

### Changed
- Configuration marker is `.anamnesis.toml`; `.ai-memory.toml` is read as a
  fallback for projects migrating from upstream `ai-memory`
- `ANAMNESIS_DB` became `ANAMNESIS_DATA_DIR`; memory lives outside the
  repository it describes

### Fixed
- Entity matching finds names that are not a single word. Names were stored
  whole and compared against tokenized queries, so `Windows BOM` or
  `anamnesis-llm` could never match anything, and the pages they named were
  reachable through full text alone. Names are now split at write time, and an
  entity matches when every one of its tokens is in the query; names stored
  before this still match whole, and are split the next time their page is
  written or reindexed
- Supersession reaches the index. `supersedes` was accepted by the MCP tool,
  written into frontmatter, and then dropped: no column was written and
  `is_latest` never changed, so an agent recording that one page replaced
  another kept being answered with the page it replaced. The claim is now
  stored as authored and resolved in both directions, so it survives the two
  pages being written in either order, and `show-page` says when a page has
  been replaced
- Session pages written by the server have their wikilinks indexed, instead of
  only after a rebuild
- Backlinks now resolve when the target page is written after the page that
  links to it
- Rebuilt sessions come back closed when the transcript records their end
- The first run is readable again. Refinery logs the entire SQL text of every
  migration at info, so `anamnesis init` saying where memory now lives scrolled
  away under several screens of schema that is checked into this repository.
  Quieted to `warn` unless `--debug` or an explicit `RUST_LOG` asks — a
  migration that fails halfway is exactly when the statement is worth seeing
- A handoff request that fails is no longer printed as a handoff. The hook read
  the body without looking at the status, so an error page — a 401 among them —
  went to stdout, where the harness injects it into the model's context as
  though the last session had written it

### Removed
- The empty `anamnesis-workstream` crate; workstreams live in core, store,
  and mcp instead
- The `new-session` CLI command; sessions are created by hooks and MCP

## [0.1.0] - 2026-08-19

### Added
- Project initialization
- Workspace structure with 10 modular crates
- Configuration templates
- Documentation and contribution guidelines
