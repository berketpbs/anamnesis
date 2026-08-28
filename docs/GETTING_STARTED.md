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

Run this **inside** the repository you want remembered. `init` takes no
arguments and creates no directory — it registers the project you are
standing in:

```bash
cd my-project
anamnesis init
```

This creates:
- `<data_dir>/wiki/<workspace>/<project>/` - Wiki pages, in their own git repository
- `<data_dir>/db/anamnesis.db` - SQLite index, rebuildable from the wiki
- `<data_dir>/raw/` - Append-only transcripts the index can be rebuilt from

Identity comes from the git remote, so two clones of the same repository share
one memory. To pin it explicitly instead, write a `.anamnesis.toml` in the
repository root:

```toml
[scope]
workspace = "default"
project = "my-project"
```

The data directory defaults to the platform data directory and can be overridden
with `--data-dir` or `ANAMNESIS_DATA_DIR`. Run `anamnesis status --verbose` to
see exactly which paths are in use.

### 2. Configure a model (optional)

Anamnesis works with no model configured. Every session still gets a page and
a handoff — compiled by counting what happened rather than by reading it, and
the page says so in its footer. A model replaces that counted summary with one
that can say *why* a session did what it did.

Export a key in the environment the server runs in:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

That alone is enough: a key present selects the Anthropic provider. Everything
else has a default.

| Variable | Default | What it does |
| --- | --- | --- |
| `ANTHROPIC_API_KEY` | — | Credential. `ANAMNESIS_LLM_API_KEY` overrides it. |
| `ANAMNESIS_LLM_PROVIDER` | `anthropic` when a key is set | `anthropic` or `none`. Set `none` to turn the model off without unsetting the key. |
| `ANAMNESIS_LLM_MODEL` | `claude-opus-5` | Model id. |
| `ANAMNESIS_LLM_BASE_URL` | `https://api.anthropic.com` | Point at a gateway or a local server. |
| `ANAMNESIS_LLM_EFFORT` | `high` | `low`, `medium`, `high`, `xhigh`, or `max`. |
| `ANAMNESIS_LLM_MAX_INPUT_TOKENS` | `6500` | Prompt budget. Long sessions are trimmed from the middle to fit. |
| `ANAMNESIS_LLM_MAX_OUTPUT_TOKENS` | `2000` | Reply budget, floored at 1000. |
| `ANAMNESIS_LLM_TIMEOUT_SECS` | `90` | Per-request timeout. |
| `ANAMNESIS_LLM_MAX_RETRIES` | `2` | Retries, for rate limits and server faults only. |
| `ANAMNESIS_LLM_FALLBACKS` | on | Server-side fallback to another model if a request is declined. |

A typo is reported at startup rather than at the end of the first session:
`anamnesis serve` refuses to bind if the settings do not parse, and prints
which model it will consolidate with when they do. `anamnesis status
--verbose` reports the same thing.

**Nothing depends on the model being reachable.** If a request times out, is
declined, or comes back as something other than a page, the counted summary is
written instead and the reason is logged. Consolidation also runs *after* the
hook's response is sent, so a slow model delays the page, never the session.

#### Per-project style

A project can say how its summaries should be written by adding a page to its
own wiki:

```text
<data_dir>/wiki/<workspace>/<project>/_prompts/consolidation.md
```

Whatever is in it is included in the prompt — "write in Turkish", "always name
the migration numbers", "mention ticket ids". It is guidance, not structure:
the reply shape is fixed by a schema regardless.

### 3. Start the Memory Server

```bash
anamnesis serve
```

This binds `127.0.0.1:8080` and serves four endpoints — `POST /hook`,
`GET /handoff`, `GET /whoami`, `GET /health`. There is no web UI to open.

By default no token is required, which is why the default bind is loopback:
there, the port is the boundary. To serve any other address, protect it first —
see [Requiring a token](#requiring-a-token).

The MCP server is a separate process the agent launches itself; `serve` does
not start one.

### 4. Connect Your Agent

Claude Code talks to anamnesis two ways, and they are independent.

**Hooks** capture the session. Print the configuration and paste it into your
`settings.json`:

```bash
anamnesis install-hooks --agent claude-code
```

**MCP** lets the agent search memory and write pages on purpose. Register the
server as a stdio subprocess:

```bash
claude mcp add anamnesis -- anamnesis mcp --repo .
```

Hooks need `anamnesis serve` running. MCP does not — it opens the store
directly.

## Common Commands

### Seed From Git History

A new project starts with an empty wiki. `bootstrap` fills it with what the
repository already records — who works here, where the churn is, what just
landed — so the first session has something to read:

```bash
anamnesis bootstrap --repo .          # write bootstrap/ pages
anamnesis bootstrap --dry-run         # show what it would write
anamnesis bootstrap --force           # refresh a stale snapshot
```

Existing pages are never overwritten without `--force`: bootstrap seeds a
memory, it does not maintain one. The pages it writes are derived from commits
rather than decided by anyone, and rank below what a session actually learned.

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

### Requiring a token

The server holds every prompt you typed, every path you opened, and every
summary written from them. On `127.0.0.1` the port is the boundary and no token
is required. Anywhere else, require one:

```bash
anamnesis token                 # prints a fresh secret, stores nothing
```

Set it for the server and for whatever runs the hooks — the same variable on
both sides:

```bash
export ANAMNESIS_TOKEN=anam_...   # server: accept this; client: present this
anamnesis serve --bind 0.0.0.0
```

`anamnesis serve` refuses to bind a non-loopback address with no token
configured. Pass `--allow-anonymous` if something in front of it already
authenticates.

For a server several people use, give each of them their own secret:

```bash
anamnesis token --operator alice        # prints the pair to add
export ANAMNESIS_TOKENS='alice=anam_...,bob=anam_...'   # server accepts
export ANAMNESIS_TOKEN=anam_...                          # alice's machine presents
```

`ANAMNESIS_TOKEN` is the secret a machine **presents**; `ANAMNESIS_TOKENS` is
the set a server **accepts**. On one machine they hold the same value.

The token never goes into a settings file — `install-hooks` writes a command
with no secret in it, and the hook reads `ANAMNESIS_TOKEN` from the environment
the harness started in. Hooks are read at session start, so set the variable
before launching the agent.

`anamnesis status` says whether this machine gets in:

```
  Server:    running at http://127.0.0.1:8080
  Auth:      required — this client is alice
```

`/health` stays answerable without a token, on purpose: it is what tells a
server that is down apart from one that is refusing this machine.

### Read What the Last Session Left

```bash
anamnesis handoff        # peek, without consuming it
anamnesis sessions       # recent sessions, newest first
anamnesis show-page bootstrap/repository.md
```

### Forget What Has Decayed

A wiki that only grows gets worse at answering, so pages that nobody writes to
and nobody reads eventually go:

```bash
anamnesis sweep                       # report what would go; change nothing
anamnesis sweep --verbose             # every page judged, with its score
anamnesis sweep --threshold 0.2       # try a stricter cutoff
anamnesis sweep --apply               # actually forget them
```

Without `--apply` nothing is deleted. Read the report first: the threshold is
a guess until you have seen it applied to a real wiki.

Four kinds of page are never swept — pinned, `semantic` and `procedural`
tiers, canonical pages, and pages marked `do-not-answer-from`. A page whose
`expires_at` has passed goes whatever its score, unless it is exempt, in which
case the sweep says so instead of choosing between two instructions you wrote.

Being read is what keeps a page: retrieval records the access, and a page
found last week does not decay out from under you however old it is.

Nothing is truly lost. The wiki is a git repository, so every page a sweep
deletes remains in its history, in a commit that names each page and why it
went:

```bash
git -C <data_dir>/wiki show HEAD
```

### Let the Memory Improve Itself

A sweep forgets what nobody needs. The other half is noticing what the memory
has earned, or is missing:

```bash
anamnesis improve                     # look, and report what is waiting
anamnesis improve --apply a1b2c3d4    # carry one out
anamnesis improve --dismiss a1b2c3d4  # never propose it again
anamnesis improve --history           # including proposals already decided
```

Two things get proposed, both from signals the system already records:

| Proposal | When | Applied by |
| --- | --- | --- |
| promote to the semantic tier | an episodic page three or more later searches came back to | the system |
| write the page | two or more pages link to a page that does not exist | you |

Promotion is worth understanding before you approve one. Retrieval records
every hit, so a page later sessions kept returning to is knowledge filed as a
session note — and the semantic tier is **exempt from the decay sweep**. It is
how a page becomes durable by proving itself rather than by someone
remembering to pin it, which is also why nothing is promoted without approval
unless a project says otherwise.

Proposals are identified by what they are about, not by when they were filed.
Dismiss one and later passes leave it alone; write the missing page yourself
and the next pass marks it resolved.

### On a Schedule

`anamnesis improve` is the same pass the server can run for you. It is off
until a project asks:

```toml
[auto_improve]
enabled = true
require_approval = false   # let the pass carry out what it can

[auto_improve.scheduler]
enabled = true
interval_minutes = 60
```

With `anamnesis serve` running, every project whose marker asks for a schedule
is improved on its own interval — measured from its own last pass, so
restarting the server does not restart the clock. Leave `require_approval` at
`true` and the schedule still runs: it files proposals for you to review, and
changes nothing.

The server logs every pass it runs and every project it skipped, to stderr:

```
INFO anamnesis_web::improve: auto-improve pass project=default/my-project
     filed=1 refreshed=0 resolved=0 carried=1 open=0
```

### Rebuild the Index

The database is disposable. If it is lost or corrupted, rebuild it from the
wiki and the transcripts:

```bash
anamnesis reindex
```

Safe to run against a live database: every identifier is derived, so a rebuild
reproduces the same rows rather than duplicating them.

### Back Up

There is no `backup` command. The data directory is the backup — copy it, or
push `<data_dir>/wiki` somewhere, since it is an ordinary git repository:

```bash
git -C <data_dir>/wiki remote add origin git@example.com:me/memory.git
git -C <data_dir>/wiki push -u origin HEAD
```

`HEAD` rather than a branch name, because a wiki created before this was
fixed is on `master`: `libgit2` ignores `init.defaultBranch`, and older
versions left the branch it chose. New wikis start on `main`, and reopening
an existing one never renames its branch.

## Configuration

`.anamnesis.toml` is optional. Three tables change what happens:

```toml
[scope]
workspace = "default"
project = "my-project"

[capture]
# Events naming these paths are dropped before anything is recorded.
ignore_paths = ["target/**", "*.log", ".env"]

[decay]
# What `anamnesis sweep` forgets. Every value is optional.
threshold = 0.05                        # forget below this retention score
age_half_life_days = 30.0               # an unwritten page halves every 30 days
access_half_life_days = 14.0            # an unread one halves every 14
access_weight = 0.5                     # how much being read counts for
```

Unknown keys are rejected rather than ignored, so a typo surfaces instead of
quietly sending memory to the wrong project.

### Excluding paths from capture

Patterns are shaped like `.gitignore` entries:

| Pattern | Matches |
| --- | --- |
| `target/**` | everything under this project's `target/` |
| `target/` | the same — a trailing slash means the directory |
| `*.log` | any `.log` file, at any depth |
| `.env` | any file called `.env`, at any depth |
| `config/*.yml` | `.yml` files directly in `config/`, not below it |

Matching is case-insensitive, and works on both the absolute path an agent
reports and the path relative to the project. An event naming several files is
dropped if any one of them is excluded, because the record of it would carry
the excluded file's contents too.

What this does **not** cover: a shell command that merely mentions a path.
Only the file a tool input names outright is matched, since guessing at
command lines would either drop events nobody asked to lose or miss the ones
that mattered. Redaction still runs on everything, and remains the first line
of defence — a secret pasted into a prompt has no path to exclude.

### Tuning what gets forgotten

The defaults forget an unread page after roughly four months, and never forget
one that is pinned, durable, canonical, or marked `do-not-answer-from`. Two
knobs move that:

| Setting | Raise it to |
| --- | --- |
| `threshold` | forget sooner — more pages fall below the cutoff |
| `age_half_life_days` | forget later — pages keep their weight longer |

`anamnesis sweep --threshold 0.2` tries a value without committing to it;
`--verbose` prints the score of every page so a cutoff can be picked from real
numbers rather than guessed. A half-life of zero or a negative threshold is
refused at load time rather than clamped — a typo in a file that governs
deletion should stop the command, not change what it deletes.

`[auto_improve]` governs what a pass may do, and when (see *Let the Memory
Improve Itself* above):

```toml
[auto_improve]
enabled = true                          # look at all
require_approval = true                 # file proposals, change nothing

# A single table. `[[auto_improve.scheduler]]` — the double-bracket array
# form — is rejected at load time.
[auto_improve.scheduler]
enabled = false                         # the server runs no pass for this project
interval_minutes = 60
```

And `[slots]`, for a server more than one person uses:

```toml
[slots]
per_user = false                        # true: a handoff slot per operator
```

Left false, a project keeps one pending handoff and whoever starts next is
handed it. Set it true and the note a session leaves waits for the operator
whose token that session arrived under — see
[Requiring a token](#requiring-a-token) for where operators come from. It
separates handoff slots, not pages: everyone on the project still reads and
writes the same wiki.

Everyone the server cannot name shares the one slot they always shared, so
turning this on without named tokens changes nothing.

```bash
anamnesis handoff --operator alice   # peek one operator's slot
anamnesis sessions                   # each session names whose it was
```

`anamnesis status` peeks the slot belonging to whoever the server says this
machine is, and says so:

```
  Memory:    12 sessions · 8 pages · no handoff waiting for alice
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

Hooks fail quietly on purpose — a memory system that can break your editing
session is worse than one that misses an event — so check in this order:

```bash
anamnesis status --verbose   # is the scope what you expect?
anamnesis sessions           # did anything arrive at all?
curl http://127.0.0.1:8080/health
```

The `hook` command always exits 0, but it writes the reason to **stderr**: a
rejected event, an unreachable server, a payload it could not parse. If
Claude Code hides hook stderr, run the same command by hand with a payload on
stdin to see it.

If the server requires a token, `anamnesis status` says so on its `Auth:` line
— including when this machine'''s token is the thing being refused. Hooks
inherit the environment the harness started in, so a variable exported after
that is not one the hooks have.

On Windows, PowerShell prepends a UTF-8 BOM when piping text into a native
executable. Both the CLI and the server strip it; a third-party wrapper that
does not will produce a parse error on the first character.

### Database Locked

If you see "database is locked", more than one process is writing. Only one
`anamnesis serve` should be running against a data directory:

```bash
ps aux | grep anamnesis
```

If the index is genuinely damaged, delete it and rebuild — nothing is lost
that `wiki/` and `raw/` do not already hold:

```bash
rm <data_dir>/db/anamnesis.db
anamnesis reindex
```

## Next Steps

1. **Read** [ARCHITECTURE.md](./ARCHITECTURE.md) to understand the system
2. **Read** [USE_CASES.md](./USE_CASES.md) for what this is for
3. **Run** `anamnesis --help`, or `anamnesis <command> --help` — the CLI is
   the current reference for the MCP tools and every flag

## Support

- File issues on [GitHub](https://github.com/berketpbs/anamnesis/issues)
- Check [CONTRIBUTING.md](../CONTRIBUTING.md) for contribution guidelines
