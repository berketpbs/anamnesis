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

Based on these use cases, implement in order:

### Phase 1: Core Capture & Retrieval
- [x] Workspace structure
- [x] Database schema & migrations (`V01`–`V06`)
- [x] Basic CLI commands — `status`, `search`, `write-page`, `show-page`,
      `sessions`, `handoff`
- [x] Memory wiki file operations
- [x] Git integration for wiki versioning
- [x] Durable transcripts under `raw/`, and `anamnesis reindex` to rebuild
      the index from them

### Phase 2: Agent Integration
- [x] MCP server implementation
- [x] Lifecycle hook capture
- [x] Claude Code integration
- [x] Session handoff system
- [ ] A second harness (Codex, Cursor, …) — the hook parser is per-harness
      and only `claude-code` is written

### Phase 3: LLM & Consolidation
- [x] LLM provider abstraction (Anthropic Messages API)
- [x] Session consolidation logic, deterministic when no model is configured
- [x] Summary generation
- [x] Retrieval over four fused signals, including an opt-in local embedder
- [x] Auto-improvement: proposals from recorded signals, applied on approval,
      on a per-project schedule the server runs

### Phase 4: Web UI & Admin
- [x] Web server (Axum) — `/hook`, `/handoff`, `/whoami`, `/health`
- [x] Wiki browser UI — `/ui`, read-only, off with `serve --no-ui`
- [ ] Search interface
- [ ] Admin endpoints
- [x] Authentication, without which none of the above should be exposed —
      bearer tokens on the API, the same token as a Basic password in the
      browser

### Phase 5: Multi-Agent & DevOps
- [x] Cross-harness workstreams
- [x] CI on every push and pull request
- [ ] Managed session resume
- [ ] Docker containerization — `Dockerfile`, `Dockerfile.dev`, and compose
      profiles exist; nothing in CI builds them, so treat them as untested
- [ ] Remote server setup

## Success Metrics

Reached:

- ✅ Developer can capture decisions in memory
- ✅ Developer can search and find relevant context
- ✅ Switching agents preserves handoff context — for one harness so far
- ✅ Project architecture decisions are documented

Not yet demonstrated:

- ⬜ Team shares knowledge without duplication — needs a shared server, which
  needs authentication
- ⬜ New developers onboard faster — `anamnesis bootstrap` is the first step
  toward this; nobody has measured it
