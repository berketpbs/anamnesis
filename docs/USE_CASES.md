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
  
Via Web UI: Browses wiki like Obsidian
  → Reads design documents
  → Follows decision tree
```

**Value**: Faster onboarding. Centralized knowledge base.

## 5. LLM-Powered Auto-Improvement

**Scenario**: System learns from sessions and improves documentation.

**Flow**:
```
Session ends
  → Consolidation step runs
  → LLM reads all observations
  → Generates summary page
  → Updates relevant wiki entries
  
Developer reviews changes
  → Approves or rejects suggestions
  → Wiki stays accurate and current
```

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
- [ ] **Database schema & migrations** (Step 1)
- [ ] **Basic CLI commands** (Step 2)
  - `anamnesis status`
  - `anamnesis search`
  - `anamnesis write-page`
- [ ] Memory wiki file operations
- [ ] Git integration for wiki versioning

### Phase 2: Agent Integration
- [ ] MCP server implementation
- [ ] Lifecycle hook capture
- [ ] Claude Code integration
- [ ] Session handoff system

### Phase 3: LLM & Consolidation
- [ ] LLM provider abstraction
- [ ] Session consolidation logic
- [ ] Auto-improvement scheduler
- [ ] Summary generation

### Phase 4: Web UI & Admin
- [ ] Web server (Axum)
- [ ] Wiki browser UI
- [ ] Search interface
- [ ] Admin endpoints

### Phase 5: Multi-Agent & DevOps
- [x] Cross-harness workstreams
- [ ] Managed session resume
- [ ] Docker containerization
- [ ] Remote server setup

## Success Metrics

- ✅ Developer can capture decisions in memory
- ✅ Developer can search and find relevant context
- ✅ Switching agents preserves handoff context
- ✅ Team shares knowledge without duplication
- ✅ New developers onboard faster
- ✅ Project architecture decisions are documented
