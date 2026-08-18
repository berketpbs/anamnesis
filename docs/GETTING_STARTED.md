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
- `.ai-memory.toml` - Configuration file
- `.memory/` - Wiki directory structure
- `.git/` - Git repository for versioning

### 2. Configure LLM (Optional)

Create a `.env` file with your LLM provider credentials:

```bash
# For Anthropic (Claude)
ANTHROPIC_API_KEY=sk-ant-...

# For OpenAI
OPENAI_API_KEY=sk-...
```

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

Edit `.ai-memory.toml` to customize behavior:

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
