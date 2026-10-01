# Anamnesis

[![CI](https://github.com/berketpbs/anamnesis/actions/workflows/ci.yml/badge.svg)](https://github.com/berketpbs/anamnesis/actions/workflows/ci.yml)

**Shared memory for AI coding agents.**

> **Release status (2026-10-01):** This page describes the `v1.2.1-rc.1`
> candidate. The unpinned install scripts and package managers still install
> the latest stable release, `v1.1.1`. To try the candidate with an install
> script, set `ANAMNESIS_VERSION=v1.2.1-rc.1` before running it.

Every agent session starts from nothing. The decision you settled yesterday,
the approach that already failed, the reason a file looks the way it does: all
of it is gone when the terminal closes, and gone again when you switch from
Claude Code to Codex. Anamnesis keeps that memory in one place that every agent
on the project reads from and writes to. It is a plain markdown wiki under git,
and it lives on your machine.

- **One memory, every agent.** Claude Code, Codex CLI, Gemini CLI, Cursor and
  OpenCode record into the same project memory, and whichever starts next is
  handed what the last one did.
- **It comes to the agent.** A new session starts with a handoff and the
  project's recorded decisions. Each prompt is answered with the pages memory
  already has on it, and with nothing when memory has nothing to say.
- **You can read it.** Memory is markdown in a git repository: browse it, edit
  it, diff it, revert it. Nothing is hidden in an embedding store.
- **Local and private by default.** Everything is redacted before it is
  stored, the server listens on `127.0.0.1`, and no model or account is
  required.

## How it works

```
  agent hooks ──▶ capture ──▶ raw/ transcripts  (redacted, append-only)
                                  │
                   session ends   ▼
                            consolidation ──▶ wiki/  (markdown + git)
                            (count, or a model)   │
                                                  ▼
  next session ◀── handoff at start · recall at each prompt · MCP tools on demand
```

1. **Capture.** Hooks in each agent send lifecycle events (prompts, tool calls,
   their results, session start and end) to a small local server. Each event
   is redacted and appended to a transcript that is never rewritten.
2. **Consolidate.** When a session ends, its events become a page and a
   handoff note. Without a model this is done by counting: files touched,
   commands run, what failed. With a model the page can also say *why*, and
   decisions, gotchas and procedures become pages of their own.
3. **Deliver.** The next session is handed the note and the decisions at
   start. Its prompts are answered from memory as they arrive. The agent can
   search, read and write pages through MCP whenever it wants more.

The SQLite index is only a projection. It can be deleted and rebuilt from the
wiki and the transcripts at any time, and `anamnesis reindex --check` confirms
that a rebuild would give back the same index.

## Quick start

Five minutes. No model and no account needed.

**1. Install.**

```bash
curl -fsSL https://raw.githubusercontent.com/berketpbs/anamnesis/main/install.sh | sh
```

```powershell
irm https://raw.githubusercontent.com/berketpbs/anamnesis/main/install.ps1 | iex
```

**2. Wire a project.** Inside the repository you want remembered:

```bash
anamnesis setup            # what is already done and what is not, one line each
anamnesis setup --write    # do the rest
```

`setup` finds the agents installed on this machine. It writes their hooks and
MCP registration, registers a server that starts at login and restarts if it
dies, and seeds memory from the project's git history. It ends by checking that
an event would actually be recorded.

**3. Work as usual.** Start your agent the way you always do, or with
`anamnesis run claude-code`, which refuses to start a session that would not be
recorded. When the session ends, it becomes a page and a handoff note.

**4. Open the next session**, in the same agent or a different one. It starts
with the note. `anamnesis status` shows whether the server is recording, and
the wiki can be browsed at <http://127.0.0.1:8080/ui>.

A model is optional: Anthropic, Google, any OpenAI-compatible endpoint, or
Ollama on the same machine. See
[Configure a model](docs/GETTING_STARTED.md#2-configure-a-model-optional).

## Supported agents

| Agent | Records sessions | Handoff at start | Recall at each prompt | MCP tools |
| --- | :---: | :---: | :---: | :---: |
| Claude Code | ✓ | ✓ | ✓ | ✓ |
| Codex CLI | ✓ | ✓ | ✓ | ✓ |
| Gemini CLI | ✓ | ✓ | ✓ | ✓ |
| Cursor | ✓ | ✓ | — | ✓ |
| OpenCode | ✓ (plugin) | ✓ (system prompt) | — | ✓ |

Each agent is wired in its own file, event names and reply format. Cursor and
OpenCode give a hook no way to add text to a prompt, so recall cannot reach
them. Their agents still get the handoff and can query memory through MCP. A
handoff crosses agents in CI on every change: Claude Code → Codex → Gemini →
Cursor → Claude Code, through the real hook commands.

## What you get

**Continuity**
- Handoffs between sessions and between agents. A terminal closed without
  ending its session is still handed over.
- Workstreams: parallel threads of work on one project, each with its own
  resume point.
- `anamnesis run <agent>` and `anamnesis continue`, which start an agent with
  memory wired, or refuse to.

**Retrieval**
- Four signals fused by reciprocal rank: full text, entities, links between
  pages, and optional embeddings (a local model, or any OpenAI-compatible
  endpoint).
- Recall at prompt time. With embeddings, a page has to be close enough to the
  prompt; without them, the prompt has to name it.
- MCP tools: `memory_query`, `memory_read_page`, `memory_write_page`,
  `memory_patch_page`, `memory_handoff_accept`, `workstream_start`,
  `workstream_status`.
- A workspace-wide `_global` scope for policies every project should find.

**Memory that stays useful**
- Pages record which session wrote them and where their content came from.
  A decision that replaces another says so, and the old one steps aside.
- `anamnesis sweep` lets unread pages decay, reporting by default and never
  touching pinned, durable, canonical or known-wrong pages.
- `anamnesis improve` proposes promotions and missing pages, and applies them
  once a project allows it, on a schedule the project sets.
- `anamnesis lint` names pages that are not worth what they cost to keep, and
  `anamnesis doctor` says why memory is thinner than the work that went into it.

**Durability and control**
- Append-only transcripts, `reindex` and `reindex --check`, and
  `backup` / `restore`, which is safe to run while the server is recording.
- `forget`, `forget-session` and `purge` remove what they remove from the disk,
  not only from queries. `redact` applies redaction rules added later to what
  is already stored.
- `anamnesis uninstall` takes the hooks and registrations back out without
  touching anybody else's settings.
- `anamnesis audit`: who changed memory, and what they changed.
- A read-only JSON API under `/api/v1`, and
  [a server other machines can reach](docs/REMOTE.md), with tokens, TLS and
  per-operator handoffs.

**Measured, not argued**
- `anamnesis eval` scores retrieval against a checked-in corpus and runs in CI,
  so a ranking change is judged by what it does to recall.
- `anamnesis bench` measures capture: on the author's machine, 1 866 events/s
  with the durable transcript and 3 708 without, p95 0.66 ms.
- A long-run experiment runs real agents across many sessions, with memory and
  without, and scores what they get right.

## With no model running

No LLM and no embedder is a supported setup, not a degraded one. Without a
model, anamnesis still:

- captures, redacts and transcribes every lifecycle event
- writes a page and a handoff for each session by counting what happened
- retrieves over full text, entities and links
- answers prompts that name what memory holds. Over 201 real prompts on this
  project's own pages, that gave a block 8 times. Asked of a project they were
  not about, 213 prompts got none
- seeds a project's memory from its git history (`anamnesis bootstrap`)

What a model adds is reading. Consolidation can say *why* something was done,
and recall can find a page the prompt describes in other words. Without one,
decisions reach memory when the agent writes them with `memory_write_page`.
[Where recall by name stops](docs/measurements/2026-09-19-recall-by-name.md).

## Status

Anamnesis is used every day on its own development, with Claude Code and Codex
sharing one memory. Capture, handoff, recall and consolidation work, and CI
checks them on Linux, Windows and macOS. What is not yet shown is how much
memory improves an agent's work: paired runs with and without memory have not
separated clearly so far. [docs/READINESS.md](docs/READINESS.md) keeps that
assessment and the measurements behind it.

**Not built yet:** admin pages. `/ui` browses and searches the wiki and cannot
change it. Proposals are applied with `anamnesis improve`, pages removed with
`anamnesis forget`.

## Install

The scripts above check each archive against the release's `SHA256SUMS`, start
the binary once before replacing anything, and upgrade an existing install in
place. `ANAMNESIS_VERSION=v1.2.1` pins a version. Homebrew, Scoop,
cargo-binstall, Docker and building from source are covered in
[Getting Started](docs/GETTING_STARTED.md#installation).

| Platform | Binaries |
| --- | --- |
| Linux | x86-64, and arm64 from v1.2.1. glibc 2.34 or later: RHEL 9, Debian 12, Ubuntu 22.04 and newer |
| macOS | Apple silicon and Intel |
| Windows | x86-64, with nothing extra to install |
| Docker | `linux/amd64` and `linux/arm64` |

From v1.2.1, each archive carries a build provenance attestation, which shows
that the release workflow built it from the tagged commit:

```bash
gh attestation verify anamnesis-v1.2.1-x86_64-unknown-linux-gnu.tar.gz --repo berketpbs/anamnesis
```

The binaries are not code-signed, so a download from a browser meets
SmartScreen or Gatekeeper. [Getting Started](docs/GETTING_STARTED.md#from-a-release)
explains how to clear it. The scripts and package managers are not affected.

## Security and privacy

Anamnesis records prompts, tool calls and their output, so it is careful with
them. Everything is redacted before it is stored, nothing leaves the machine
unless you configure a model or a remote embedder, and what is removed is
removed from the disk. [SECURITY.md](SECURITY.md) lists what is kept where, what
is sent where, what redaction does and does not promise, and how to report a
vulnerability privately.

## Documentation

- [Getting Started](docs/GETTING_STARTED.md): installation, each agent,
  models, keeping the server running, every command
- [Architecture](docs/ARCHITECTURE.md): how capture, storage, consolidation
  and retrieval fit together
- [Use cases](docs/USE_CASES.md): what this is for
- [Remote server](docs/REMOTE.md) and [Docker](docs/DOCKER.md)
- [Readiness](docs/READINESS.md) and [Direction](docs/DIRECTION.md): what is
  proven, what is not, and the measurements behind each decision

## Development

A Rust workspace (edition 2024), one crate per layer:

| Crate | Role |
| --- | --- |
| `anamnesis-core` | Types, redaction, and the rules the other crates apply |
| `anamnesis-store` | The SQLite index with its migrations, and the raw transcripts |
| `anamnesis-wiki` | The git-versioned markdown wiki |
| `anamnesis-hooks` | Lifecycle capture for each agent |
| `anamnesis-llm` | Model providers and embeddings, local or remote |
| `anamnesis-consolidate` | A session's page and handoff, by counting or by reading |
| `anamnesis-mcp` | The MCP server agents query memory through |
| `anamnesis-web` | The HTTP server: hooks, handoffs, recall, the wiki browser |
| `anamnesis-cli` | The `anamnesis` command |
| `anamnesis-evals` | Retrieval evaluation and the long-run agent experiment |

```bash
cargo build
cargo test
cargo run -p anamnesis-cli -- status
```

CI runs formatting, clippy with warnings as errors, the tests on Linux, Windows
and macOS, the docs, dependency advisories and licenses, the Docker image, the
install scripts on every system they claim, and an upgrade from every published
release. See [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.

## License

MIT. See [LICENSE](LICENSE).
