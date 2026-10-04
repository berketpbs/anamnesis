# Reading captured session sources

`memory_read_page` returns `source_session` when a page's origin is known. Use
that identifier with the MCP tool `memory_read_session` or with the CLI:

```text
anamnesis show-session SESSION_ID --kind assistant-message
anamnesis show-session SESSION_ID --offset 20 --limit 20 --json
```

The MCP request has `session_id`, optional `kind`, optional `offset` (default 0)
and optional `limit` (default 20, maximum 100). The CLI also accepts an
unambiguous prefix printed by `sessions`. A filter names one captured event
type, such as `user-prompt`, `assistant-message`, `tool-use` or `tool-attempt`.
Offsets refer to the filtered, chronological sequence. `next_offset` is null
when that sequence ends. A `tool-attempt` whose call completed has empty text:
its input is stored once, in the `tool-use` that follows it.

Every returned event contains its stable observation `id`, timestamp `at`,
`kind`, stored `text`, `truncated` flag and `redacted_on_read` flag. The response
also reports whether any source events, user messages and assistant messages
remain available. A stored session with no events returns an explicit empty
source response. A missing or ambiguous source is an error. A filtered empty
page is distinct from an absent source.

Reads are limited to the current project. Another project's session cannot be
opened by supplying its id, including sessions in the workspace's shared
scope. Today's redaction rules are applied to returned text without rewriting
the stored observations. Reads do not claim or modify a handoff.

This is captured source access, not a complete transcript guarantee. Some
clients do not provide assistant messages; capture may truncate a message or
record only lifecycle/tool events. Missing text is never generated from the
wiki summary or reconstructed from a client's private archive. Followup work
must respect `truncated` and the availability flags before quoting a source.
