# Anamnesis AI Memory Wiki

This directory contains the persistent memory system for the anamnesis project. It captures session observations, decisions, procedures, and project context across agent sessions.

## Structure

- **decisions/** - Project decisions and architectural choices with rationale
- **procedures/** - Recurring workflows and how-tos
- **gotchas/** - Known issues, constraints, and workarounds
- **rules/** - Project standards, conventions, and coding patterns
- **_global/** - Cross-project shared context and preferences
- **_slots/** - Optional per-user memory (when multi-user mode is enabled)

## How It Works

- Markdown files are git-versioned and can be browsed in Obsidian or any editor
- Frontmatter (YAML) stores metadata: `name`, `description`, `type`, `entities`
- Auto-consolidation at session-end creates coherent summaries from observations
- FTS5 search + entity matching enables intelligent recall
- Pages are trusted only if verified against current code state

## Integration with Claude Code

Memory is automatically captured through:
- Session lifecycle hooks (start, end, tool calls)
- User prompts and system observations
- Tool execution outcomes (bounded, sanitized)

## Getting Started

1. Create decision/procedure/gotcha pages as needed using `memory_write_page`
2. Tag important pages with `[[cross-reference]]` for linking
3. Use `memory_query X` to search across the wiki
4. Review `MEMORY.md` for the current index of active pages

See `.ai-memory.toml` for configuration.
