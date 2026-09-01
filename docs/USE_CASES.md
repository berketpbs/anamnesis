# Anamnesis Use Cases

## 1. Single Developer, Single Agent

**Scenario**: Developer using Claude Code to work on a project.

**Flow**:
```
Session 1: Claude Code starts
  → Agent solves a complex problem
  → Learns about architecture, design decisions
  → Session ends
  
Session 2: Claude Code resumes
  → Agent recalls previous context
  → Knows what was attempted before
  → Continues without re-explanation
```

**Value**: No context loss between sessions. Faster problem-solving.

## 2. Developer Switching Agents

**Scenario**: Developer wants to switch from Claude Code to Codex, or use multiple agents.

**Flow**:
```
Session 1: Claude Code analyzes schema design
  → Makes decision on database approach
  → Writes decision page to wiki
  → Session ends with handoff
  
Session 2: Codex starts via ai-memory run codex
  → Receives handoff summary
  → Knows postgres was chosen and why
  → Implements with full context
```

**Value**: Cross-agent continuity. Preserves architectural decisions.

## 3. Team Knowledge Capture

**Scenario**: Team builds institutional knowledge about project decisions.

**Flow**:
```
Developer A: Discovers bug in authentication
  → Documents gotcha in memory
  → Explains workaround
  
Developer B: Encounters same issue
  → Searches memory for "auth bug"
  → Finds gotcha with solution
  → Avoids duplicate work
```

**Value**: Shared team knowledge. Prevents repeated mistakes.

## 4. Onboarding New Developers

**Scenario**: New developer joins team, needs project context.

**Flow**:
```
New Dev: Queries memory for project decisions
  → Finds "why we chose Postgres"
  → Finds "architecture decisions"
  → Understands project structure
  
Opens <data_dir>/wiki in Obsidian or any editor
  → Reads design documents
  → Follows the wikilinks between them
```

The wiki is plain markdown in a git repository, so any editor works. There is
no built-in browser UI.

**Value**: Faster onboarding. Centralized knowledge base.

## 5. LLM-Powered Auto-Improvement

**Scenario**: System learns from sessions and improves documentation.

**Flow**:
```
Session ends
  → Consolidation runs                          [implemented]
  → LLM reads the session's observations        [implemented]
  → Generates a summary page and a handoff      [implemented]
  → Sweeps and updates other wiki entries       [not implemented]

Developer reviews changes
  → Approves or rejects suggestions             [not implemented]
```

Consolidation writes one page for the session that just ended, and commits it.
It never rewrites another page, and there is no approval queue: the
`require_approval` setting is parsed and unused. A model is optional
throughout — without one the page is compiled by counting what happened, and
says so in its footer.

**Value**: Living documentation. Stays in sync with reality.

## 6. Multi-Project Context Sharing

**Scenario**: Developer works on multiple related projects.

**Flow**:
```
Global Scope: "We always use PostgreSQL for data"
  → Policy stored in _global/
  → Inherited by all projects
  
Project A: Uses PostgreSQL for storage
Project B: Also uses PostgreSQL
  
New Project: Queries memory
  → Finds global policy
  → Applies same standards
```

**Value**: Cross-project consistency. Code style reuse.

> **Implemented.** `anamnesis write-page --global` writes into the workspace's
> `_global` scope, and every query from a project in that workspace searches
> it alongside the project's own pages. A hit from the shared scope is marked
> as one, because a policy that applies everywhere and a note about this
> project are different kinds of answer. Ties go to the project: it is the
> more specific of the two.
>
> It is one shared scope per workspace, not one overall — two workspaces are
> two memories. Sharing is read-only inheritance, not merging: the pages stay
> in `_global`, and nothing is copied into a project.

## 7. Debugging Session Context

**Scenario**: Developer debugging complex issue across multiple attempts.

**Flow**:
```
Attempt 1: Claude Code analyzes logs
  → Documents failed hypothesis
  → Session ends
  
Attempt 2: Continue with full context
  → Agent reads what was tried
  → Knows why it failed
  → Tries different approach
  
Attempt 3: Finally finds root cause
  → Writes solution to wiki
  → Future devs have complete debug story
```

**Value**: Better debugging. Documented problem-solving process.

## 8. Architecture Evolution Tracking

**Scenario**: Project evolves over time. Need to track why decisions were made.

**Flow**:
```
v1.0: "Use monolith architecture"
  → Decision documented with rationale
  → Works fine for small scale
  
v2.0: "Migrate to microservices"
  → New decision supersedes old one
  → Git history shows both
  → Developers understand evolution
```

**Value**: Architectural accountability. Context for future changes.

## Feature Roadmap

Based on these use cases, implement in order. What is checked here is checked
because it works, not because it was attempted — an unchecked box is worth more
than a hopeful one.

### Phase 1: Core Capture & Retrieval
- [x] Workspace structure
- [x] Database schema & migrations (`V01`–`V11`)
- [x] Basic CLI commands — `status`, `search`, `write-page`, `show-page`,
      `sessions`, `handoff`
- [x] Memory wiki file operations
- [x] Git integration for wiki versioning
- [x] Durable transcripts under `raw/`, and `anamnesis reindex` to rebuild
      the index from them
- [x] `anamnesis backup` / `restore` — the transcripts are the half nothing
      else can rebuild, and the index is copied through SQLite's backup API so
      it can be taken while the server is recording

### Phase 2: Agent Integration
- [x] MCP server implementation
- [x] Lifecycle hook capture
- [x] Claude Code integration
- [x] Session handoff system
- [x] Five harnesses: Claude Code, Codex CLI, Gemini CLI and Cursor through
      hooks; OpenCode through a plugin module, since it extends by module and
      has no stdout channel for a handoff to answer on
- [x] `anamnesis run` / `continue` — start a harness with memory wired, and
      refuse to start one whose session would not be recorded

### Phase 3: LLM & Consolidation
- [x] LLM provider abstraction (Anthropic Messages API)
- [x] Session consolidation logic, deterministic when no model is configured
- [x] Summary generation
- [x] Retrieval over four fused signals, including an opt-in embedder — a
      local model by default, or any OpenAI-compatible endpoint
- [x] Auto-improvement: proposals from recorded signals, applied on approval,
      on a per-project schedule the server runs

### Phase 4: Web UI & Admin
- [x] Web server (Axum) — `/hook`, `/handoff`, `/whoami`, `/health`
- [x] Wiki browser UI — `/ui`, read-only, off with `serve --no-ui`
- [x] Search interface — `?q=` on a scope, the same fused query an agent runs
- [x] JSON API — `/api/v1` serves scopes, pages, one page, search, sessions
      and the audit log to a program, behind the same tokens
- [x] Audit log — who changed memory and what they changed, for the changes
      people make deliberately
- [ ] Admin endpoints — the browser *shows* what wants attention (drift
      between wiki and index, open proposals); carrying anything out is still
      a CLI command, deliberately
- [x] Authentication, without which none of the above should be exposed —
      bearer tokens on the API, the same token as a Basic password in the
      browser

### Phase 5: Multi-Agent & DevOps
- [x] Cross-harness workstreams
- [x] CI on every push and pull request
- [x] Managed session resume — `anamnesis continue` starts whichever harness
      this project last used
- [x] Docker containerization — CI builds the image and checks that it
      answers `/health` and leaves on SIGTERM; a tag publishes it for
      `linux/amd64` and `linux/arm64`
- [x] Remote server setup — [REMOTE.md](REMOTE.md): tokens, TLS, the proxy
      body limit that would otherwise refuse ordinary events, per-operator
      handoffs, and a checklist
- [x] Release binaries — a tag builds for Linux, macOS (both architectures)
      and Windows, checks each one starts, and publishes them with checksums
- [x] A measured write ceiling — `anamnesis bench`, rather than an argument
      about whether the capture path holds

### Not planned
- **Batch embedding.** One page or one query at a time is what the callers
  here do; a batch API would be a second shape to keep working for a saving
  nobody has measured.
- **Writes through the HTTP API.** Every write is either capture, which has
  its own endpoint, or a decision somebody made — and those stay CLI commands
  so that changing memory takes a machine somebody has rather than a token
  somebody has.

## Success Metrics

Reached:

- ✅ Developer can capture decisions in memory
- ✅ Developer can search and find relevant context
- ✅ Switching agents preserves handoff context — five harnesses read the
  same handoff, and `anamnesis continue` picks up the one that ran last
- ✅ Project architecture decisions are documented
- ✅ A session that would not be recorded does not start quietly — the
  failure that cost this project two afternoons now stops the launch

Not yet demonstrated:

- ⬜ Team shares knowledge without duplication — everything it needs exists
  now (tokens, per-operator handoffs, an audit log, a JSON API, a guide for
  running a server other machines reach). Nobody has run it that way yet, so
  the box stays empty
- ⬜ New developers onboard faster — `anamnesis bootstrap` is the first step
  toward this; nobody has measured it
