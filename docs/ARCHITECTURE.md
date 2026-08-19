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
│                        CLI / Web UI                         │
│               (anamnesis-cli, anamnesis-web)               │
└────────────────────────────┬────────────────────────────────┘
                             │
            ┌────────────────┼────────────────┐
            │                │                │
┌───────────▼────────┐  ┌────▼──────────┐   │
│  Consolidate       │  │  Workstream   │   │
│  (LLM-powered)     │  │  Management   │   │
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
- Hook payload parsing
- Observation creation
- Event aggregation
- Capture exclusion filters

### anamnesis-llm
LLM provider abstraction.

**Provides:**
- Provider trait and implementations
- Prompt building
- Token counting
- Streaming response handling

**Supports:**
- Anthropic (Claude API)
- OpenAI
- Local endpoints (Ollama, LM Studio)

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
- `memory_feedback` - Rate page usefulness
- `memory_handoff_accept` - Accept session handoff

### anamnesis-web
Web UI and HTTP server.

**Provides:**
- Wiki browser interface
- Search UI
- Git history visualization
- REST API endpoints

### anamnesis-workstream
Cross-harness workstream management.

**Provides:**
- Workstream tracking
- Session linking
- Cross-harness resume logic
- Visible event ledger

### anamnesis-cli
Command-line interface.

**Provides:**
- CLI subcommands
- Configuration management
- Interactive pickers
- Status reporting

## Data Flow

### Session Capture

```
Agent Session
    │
    ├─→ Hook Event (SessionStart, ToolCall, etc.)
    │
    ├─→ anamnesis-hooks (Sanitize & validate)
    │
    └─→ anamnesis-store (Persist observation)
         │
         └─→ SQLite DB
```

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
    ├─→ anamnesis-mcp (memory_query tool)
    │
    ├─→ anamnesis-store (FTS5 search)
    │
    ├─→ anamnesis-wiki (Resolve entities)
    │
    └─→ Return results to agent
```

## Storage Schema

### Sessions Table
- id: UUID
- agent: String (claude-code, codex, etc.)
- start_time: DateTime
- end_time: DateTime (optional)
- checkout_path: String
- project_id: UUID
- workspace_id: UUID

### Observations Table
- id: UUID
- session_id: UUID
- event_type: String (prompt, tool_call, tool_result, etc.)
- timestamp: DateTime
- payload: JSON
- sanitized: Boolean

### Pages Table
- id: UUID
- project_id: UUID
- path: String
- title: String
- body: Text (Markdown)
- entities: JSON array
- created_at: DateTime
- updated_at: DateTime
- git_commit: String

## Configuration

Projects use `.anamnesis.toml` for configuration. The file is optional: without
it, the project identity is derived from the git remote.

```toml
[scope]
workspace = "default"
project = "anamnesis"

[capture]
ignore_paths = ["target/**", "node_modules/**"]

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

- **Sanitization**: Sensitive data removed from observations (API keys, passwords, PII)
- **Capture Exclusions**: Patterns exclude files/paths from capture
- **Bearer Token Auth**: Optional authentication for shared servers
- **Per-User Slots**: Optional isolation of context per operator

## Future Enhancements

1. **Vector Embeddings** - Optional semantic search via embedding providers
2. **Graph Database** - Entity relationship tracking
3. **Multi-Agent Coordination** - Shared context between agents
4. **Policy Engine** - Fine-grained access control
5. **Audit Trail** - Complete action logging
