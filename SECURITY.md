# Security

Anamnesis records what coding agents do: prompts, tool calls, their output. It
therefore holds exactly what an attacker would want from a developer's machine,
and this page says what it keeps, what leaves the machine, and how to report a
problem.

## Reporting a vulnerability

Report it privately, from this repository's **Security** tab ("Report a
vulnerability"). Please don't open a public issue for anything that exposes
stored memory, a secret that got past redaction, or a way into a server.

Include `anamnesis --version`, the operating system, which agent harness was
involved, and the steps that show the problem. If a secret got through, name
the kind of secret and never the value.

Fixes go into the next release. The [changelog](CHANGELOG.md) records them
under **Security**.

## What is stored, and where

Everything lives in the data directory: `ANAMNESIS_DATA_DIR`, or the platform
data directory (`~/.local/share/anamnesis`, `~/Library/Application
Support/anamnesis`, `%APPDATA%\anamnesis`). The project repository holds only
`.anamnesis.toml` and the hook and MCP entries `anamnesis setup` writes for each
harness.

| Directory | What it holds | How long |
| --- | --- | --- |
| `raw/` | Every captured observation, redacted, as append-only JSONL. A transcript that has been quiet for a week is compressed and keeps every line | Until `forget-session` or `purge` |
| `db/` | The SQLite index: sessions, observations, pages, handoffs, audit log. It can be rebuilt from `raw/` and `wiki/` | Until removed |
| `wiki/` | Markdown pages in a git repository | Removed pages stay in its history |
| `logs/` | The server's rolling log | Until removed |
| `models/` | A local embedding model, when one is used | Until removed |

`backup` writes all of the above into one archive. Treat that archive the way
you would treat the data directory itself.

## What leaves the machine

- **By default, nothing.** The server binds `127.0.0.1`. With no model
  configured, sessions are summarised by counting, and embeddings are off
  unless enabled.
- **With a model configured**, the redacted observations of a finished session
  are sent to that provider (Anthropic, Google, or the OpenAI-compatible
  endpoint you named) to write its page and handoff. Ollama keeps this on
  the machine.
- **With a remote embedder**, page text goes to that endpoint.
- **Keys** are read from the operating system's credential store (`anamnesis
  key set`; Credential Manager on Windows, the Keychain on macOS), from
  `settings.env` in the data directory, or from the environment. `anamnesis
  key list` names the stored keys and never shows their values.

A server reachable from other machines needs tokens and TLS. See
[docs/REMOTE.md](docs/REMOTE.md).

## Redaction

Every observation is redacted before it is written anywhere or sent to a model.
The built-in rules recognise:

- private keys
- Anthropic, OpenAI, Google, Stripe, npm, GitHub, Slack and AWS keys and tokens
- anamnesis's own tokens and JWTs
- `Authorization` and `Cookie` headers
- credentials in URLs, on command lines and in `.netrc`
- `password=`-style assignments and stated passwords

Redaction is a safety net, not a guarantee: it catches recognisable shapes. A
file that should never be read belongs in `[capture] ignore_paths` in
`.anamnesis.toml`. An event that names such a file is dropped whole.

When a rule is added after something already got through, `anamnesis redact`
runs today's rules over `raw/` and the index (`--apply` rewrites them). It lists
wiki pages, their git history, backups and copies of the index that still hold
a match, and never rewrites those.

## Removing what was recorded

- `anamnesis forget <page>` removes a page from the wiki and the index. The
  wiki's history keeps it.
- `anamnesis forget-session <id> --apply` removes a session, its observations
  and its transcript. A transcript is in no git history, so this is final.
- `anamnesis purge` removes a project's memory in the order it can be got
  back. Pages leave as a commit; transcripts do not come back.

These three zero the database pages they free, merge the full-text index and
empty SQLite's write-ahead log, so what they remove is gone from the disk and
not only from queries. If a reader keeps the log from emptying, the command
says so and how to finish.
