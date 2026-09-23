# Getting Started with Anamnesis

## Prerequisites

- Rust 1.95 or later
- Git
- SQLite (bundled with project)

## Installation

### With the install script

```bash
# Linux and macOS
curl -fsSL https://raw.githubusercontent.com/berketpbs/anamnesis/main/install.sh | sh
```

```powershell
# Windows
irm https://raw.githubusercontent.com/berketpbs/anamnesis/main/install.ps1 | iex
```

The script finds the latest release, downloads the archive for this machine,
and refuses to go further unless the archive matches the release's
`SHA256SUMS`. It then starts the new binary once, from where it was unpacked:
a binary this machine cannot run fails there, in the loader's own words, and a
working install is left as it was.

Where it puts the binary follows from the hooks, the MCP registration and the
service all naming it by its path. An `anamnesis` already on `PATH` — or, on
Windows, in `%APPDATA%\anamnesis\bin` beside the data directory — is replaced
in place (on Windows the old one is renamed to `anamnesis.exe.old-<stamp>`
first, since a running server or MCP process holds it open, and Windows
allows renaming a running program but not overwriting it). A first install
goes to `~/.local/bin` or `%LOCALAPPDATA%\Programs\anamnesis`; the Windows
script adds that directory to the user `PATH`, the POSIX one prints the line to
add. A server that is already running keeps the old binary until it restarts.

Both scripts read their settings from the environment, since a script piped
into a shell takes no arguments:

| Variable | Effect |
|---|---|
| `ANAMNESIS_VERSION` | a tag such as `v1.1.0` instead of the latest release |
| `ANAMNESIS_INSTALL_DIR` | install here instead of the directory chosen above |
| `ANAMNESIS_NO_PATH` | Windows only: leave the user `PATH` alone |

From v1.1.1 the Linux binary needs glibc 2.34 or later (RHEL 9, Ubuntu 22.04,
Debian 12) and the Windows binary needs nothing Windows does not ship. v1.1.0
and v1.0.0 were built on the newest runners: their Linux binary needs glibc
2.39 (Ubuntu 24.04, Debian 13) and their Windows binary the Microsoft Visual
C++ Redistributable, so pinning `ANAMNESIS_VERSION` to either on a machine
without them stops at the check above, which says so.

### With a package manager

```bash
# Homebrew, on macOS or Linux (x86-64)
brew tap berketpbs/anamnesis https://github.com/berketpbs/anamnesis
brew install berketpbs/anamnesis/anamnesis
```

```powershell
# Scoop, on Windows
scoop bucket add anamnesis https://github.com/berketpbs/anamnesis
scoop install anamnesis/anamnesis
```

```bash
# cargo-binstall, anywhere a release is built for
cargo binstall --git https://github.com/berketpbs/anamnesis anamnesis-cli
```

The tap and the bucket are this repository: the formula is
`HomebrewFormula/anamnesis.rb` and the manifest `bucket/anamnesis.json`, both
written from a release's `SHA256SUMS` by `packaging/render.sh`, so
`brew upgrade` and `scoop update` install a release only once its manifests
have been rendered and merged. `cargo binstall` needs `--git` because the name
`anamnesis` on crates.io belongs to another project; it fetches the same
release archive the install scripts do.

Run `anamnesis setup` after an upgrade. It reports a hook or a service naming
a binary other than the one it runs as, which is what an upgrade that moved the
binary leaves behind.

### From a Release

Every tagged version has binaries attached to it on the
[releases page](https://github.com/berketpbs/anamnesis/releases): Linux
(x86-64), macOS (Intel and Apple silicon) and Windows. Each archive holds the
binary, the README, the licence and the changelog, and every release carries a
`SHA256SUMS` file — a release nobody can verify is a release nobody should run.

```bash
tar -xzf anamnesis-v1.2.1-x86_64-unknown-linux-gnu.tar.gz
./anamnesis-v1.2.1-x86_64-unknown-linux-gnu/anamnesis --version
```

Put the binary where it will stay — on `PATH`, or beside the data directory —
before wiring anything: hooks, the MCP registration and the service all name
the binary by its path. Then `anamnesis setup` inside a repository.

### From Source

```bash
git clone https://github.com/berketpbs/anamnesis.git
cd anamnesis
cargo build --release
```

The binary will be available at `target/release/anamnesis`.

## Quick Start

### In one command

Inside the repository you want remembered:

```bash
cd my-project
anamnesis setup            # says what is done and what is not
anamnesis setup --write    # does the rest
```

`setup` looks at five things and prints one line for each:

```
  ✓ memory  default/my-project is registered in ~/.local/share/anamnesis
  → hooks   claude-code: wire 8 event(s) to http://127.0.0.1:8080, in .claude/settings.local.json
  → mcp     claude-code: register the memory tools in .mcp.json
  → server  nothing answers on port 8080; register the service and start it
  → seed    memory is empty; seed bootstrap/ pages from git history
```

With `--write` it runs the steps marked `→` — the same `init`,
`install-hooks --write`, `install-mcp --write`, `service install --write` and
`bootstrap` the sections below describe one at a time — and then probes the
server, so the last thing it says is whether an event would be recorded. A
step is only ever marked done when the command behind it would change nothing,
so running `setup` again is how to check a project later.

With no `--agent`, setup detects the harness executables on `PATH` and harness
configuration already present in the project. On a machine with Claude Code and
Codex installed, the same invocation wires both. `--agent codex` is repeatable
and overrides detection when only named harnesses should be wired.
`--no-service` leaves the service manager alone, and `--no-seed` leaves an empty
memory empty. A model is not something `setup` configures, since it needs a key;
the `Model:` line it prints says what the server would use, and step 2 below is
how to change it.

The rest of this section is the same ground one command at a time.

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

That alone is enough: `ANTHROPIC_API_KEY` present selects the Anthropic
provider. Everything else has a default.

| Variable | Default | What it does |
| --- | --- | --- |
| `ANTHROPIC_API_KEY` | — | Anthropic's credential. The only key that selects a provider by being set. |
| `ANAMNESIS_LLM_API_KEY` | — | The credential of the provider `ANAMNESIS_LLM_PROVIDER` names, used before that provider's own variable. With no provider named it is sent nowhere, and `serve` says so. |
| `ANAMNESIS_LLM_PROVIDER` | `anthropic` when `ANTHROPIC_API_KEY` is set | `anthropic`, `openai`, `google`, `ollama`, or `none`. Set `none` to turn the model off without unsetting the key. |
| `ANAMNESIS_LLM_MODEL` | `claude-opus-5`; `llama3.2` for `ollama`; `gemini-3.6-flash` for `google` | Model id. |
| `ANAMNESIS_LLM_BASE_URL` | per provider | `https://api.anthropic.com`, `https://api.openai.com/v1`, `https://generativelanguage.googleapis.com/v1beta/openai`, or `http://127.0.0.1:11434/v1`. Point at a gateway, a second Ollama, or vLLM. |
| `ANAMNESIS_LLM_EFFORT` | `high` | `low`, `medium`, `high`, `xhigh`, or `max`. `google` has no word above `high` and is sent `high` for the two above it. |
| `ANAMNESIS_LLM_MAX_INPUT_TOKENS` | `64000` | Prompt budget. Long sessions are trimmed from the middle to fit, and the page says how much of the session it was written from. |
| `ANAMNESIS_LLM_MAX_OUTPUT_TOKENS` | `16000` | Reply ceiling, floored at 1000. Only generated tokens are billed; reasoning models spend much of it before answering. |
| `ANAMNESIS_LLM_TIMEOUT_SECS` | `90` | Per-request timeout. |
| `ANAMNESIS_LLM_MAX_RETRIES` | `2`; `8` for the server | Retries, for rate limits and server faults only. |
| `ANAMNESIS_LLM_FALLBACKS` | on | Server-side fallback to another model if a request is declined. |
| `ANAMNESIS_LLM_FALLBACK_PROVIDERS` | — | Providers to ask, in order, when the configured one fails transiently: `provider[:model]`, comma-separated. See below. |

A typo is reported at startup rather than at the end of the first session:
`anamnesis serve` refuses to bind if the settings do not parse, and prints
which model it will consolidate with when they do. `anamnesis status
--verbose` reports the same thing.

#### Settings for a server nobody starts by hand

A server started at login by the operating system gets an environment nobody
chose, so exporting variables in a terminal configures nothing it will see.
Write them to `settings.env` in the data directory instead:

```bash
# <data_dir>/settings.env
ANAMNESIS_LLM_PROVIDER=google
ANAMNESIS_LLM_MODEL=gemini-3.5-flash
ANAMNESIS_LLM_FALLBACK_PROVIDERS=google:gemini-3.6-flash
ANAMNESIS_EMBED_ENABLED=1
ANAMNESIS_EMBED_PROVIDER=openai
ANAMNESIS_EMBED_URL=http://127.0.0.1:11434/v1/embeddings
ANAMNESIS_EMBED_MODEL=nomic-embed-text
```

Every command reads it — the server, the MCP server a harness starts, `status`,
`reconsolidate` — so the CLI sees the model the server does. A variable in the
environment still wins over the same line in the file. `NAME=value` per line,
`#` for comments, an optional `export ` and one pair of quotes; nothing is
expanded.

Two kinds of line are refused: a **secret** (`*_API_KEY`, `ANAMNESIS_TOKEN`),
since the file is plain text, and `ANAMNESIS_DATA_DIR`, since the file is found
through the data directory. A refused or malformed line is named on stderr by
every command, `status --verbose` lists what the file set and how many lines it
did not, and `serve` will not start while any line is refused. The file is not
part of `anamnesis backup`: it describes one machine.

The key goes in this account's credential store — Credential Manager on
Windows, the Keychain on macOS — typed without being shown:

```bash
anamnesis key set GEMINI_API_KEY          # or ANAMNESIS_LLM_API_KEY, ANTHROPIC_API_KEY, OPENAI_API_KEY
anamnesis key list                        # which are stored or in this shell, never the values
anamnesis key check                       # ask each configured model one question with its key
anamnesis key forget GEMINI_API_KEY
```

`key check` builds the models a server started now would build — the
configured one and every fallback, each on its own — and sends each a one-line
question. It says whether the key was accepted, refused, or out of quota, which
of the variables it came from, and exits non-zero when any model could not be
shown to work. Run it after `key set` and before restarting the server, which
reads its key only when it starts. An answer to one line is not an answer to a
consolidation — a model can answer this and refuse every real session with a
`503` — so read `anamnesis status` after the next session ends: its `Why:` line
carries what the model said if it did not write the page.

Putting a new key in, in order:

1. `anamnesis key set ANAMNESIS_LLM_API_KEY` (or the provider's own variable).
2. `anamnesis key check` — every model ✅. A refused key reads `the key was
   refused (400): ...`; Google words it `Please pass a valid API key` or
   `Invalid Auth key.` depending on the key's shape.
3. `anamnesis service restart`, since the server reads its key only when it
   starts. It stops the server the service keeps running and waits until the
   one started in its place answers. On Windows that is the process listening
   on the port, stopped only if it is `anamnesis.exe` — never the `mcp` one a
   harness started, and never by `schtasks /End`, which ends the task and
   leaves the server running outside it.
4. `anamnesis status`. Sessions whose pages were counted while the key was
   refused are asked about again by the server's next pass, a few at a time;
   `Why:` disappears once a request is answered, and the `Summaries:` count
   moves as the pages are rewritten.

On a free tier with a daily quota, a backlog of counted sessions can spend the
day's requests in minutes. A refusal for a quota counted per day is asked once
and handed to the next model in `ANAMNESIS_LLM_FALLBACK_PROVIDERS`, and the
rest waits for tomorrow.

It is read under the name of the variable it stands for, after the environment
and the settings file, by every command. The store is the account's and the
settings file is one data directory's, so a key stored as
`ANAMNESIS_LLM_API_KEY` is used only where that directory's settings name the
provider; a provider's own name (`GEMINI_API_KEY`) is the one to prefer. `--stdin` takes the key from one line
of standard input, for moving it out of somewhere else without it ever being an
argument. Server tokens (`ANAMNESIS_TOKEN`) are not stored this way: the hook
reads its token from the environment it runs in, on every tool call.

Linux gets no store. Secret Service needs D-Bus and a running keyring, which
servers and containers do not have, and kernel keyutils forgets at reboot, so
`anamnesis key` says so and the environment is the answer there: an
`EnvironmentFile=` in the unit, readable only by its owner.

#### Google AI Studio

Gemini publishes an OpenAI-compatible surface, so it is the same client again —
only its address and model differ:

```bash
export ANAMNESIS_LLM_PROVIDER=google
export GEMINI_API_KEY=AQ....                 # or GOOGLE_API_KEY
export ANAMNESIS_LLM_MODEL=gemini-3.6-flash  # whatever your account lists
```

Three things worth knowing, all of which show up as something other than what
they are:

**The provider has to be named.** A `GEMINI_API_KEY` sitting in the environment
selects nothing on its own, unlike `ANTHROPIC_API_KEY`. A key that could select
a provider is a key that could redirect one — every session transcript would
start going somewhere nobody chose, on the strength of a variable exported for
something else.

**Keys now start with `AQ.`, not `AIza`.** Google is retiring the older
standard keys, and the new ones authenticate the same way here: one
`Authorization: Bearer` header. Sending a second credential alongside it — the
`x-goog-api-key` header, or `?key=` on the URL — is refused with `400 Multiple
authentication credentials received`, which reads like a bad key rather than
like two of them. Anamnesis sends exactly one, and a test holds it there.

**The models endpoint lists models your key cannot call.** `gemini-2.5-flash`
is returned by `GET /v1beta/openai/models` and answers a completion with `404 …
no longer available to new users`, naming its replacement. Pick the model by
calling it, not by reading the listing — and if a name that used to work starts
404ing, read the message: it says which one to move to.

**`xhigh` and `max` are not words here.** Google's vocabulary stops at `high`,
and it refuses the two above it with a plain 400 that names neither thinking
nor the field — so the recovery that saves a non-thinking Ollama model (below)
cannot fire, and the session would lose its page over a setting the model never
needed. Anything above `high` is therefore sent as `high`.

#### A model on this machine

`openai`, `google` and `ollama` are the same client — one wire format, several
backends: OpenAI itself, Ollama, Gemini, vLLM, LM Studio, and any gateway
presenting `/chat/completions`. The difference between the names is the default
address, and that `ollama` expects no credential, because a model running here
has none to present.

```bash
export ANAMNESIS_LLM_PROVIDER=ollama
export ANAMNESIS_LLM_MODEL=llama3.2          # whatever `ollama list` shows
```

Three things learned pointing this at a real Ollama, all worth knowing before
you conclude the setup is broken:

**Ollama's default context is smaller than the prompt anamnesis sends.** A
session goes out at up to `ANAMNESIS_LLM_MAX_INPUT_TOKENS` (64000 by default)
and the reply ceiling is on top; Ollama serves 4096 unless the model says otherwise, and
what does not fit is dropped rather than refused. The page comes back valid,
readable, and quietly missing the middle of the session. Give the model a
window that holds both — a `Modelfile` is the durable way, since it travels
with the model instead of with whoever remembers to set an environment
variable:

```
FROM qwen2.5:7b-instruct
PARAMETER num_ctx 32768
```

```bash
ollama create anamnesis-qwen -f Modelfile
export ANAMNESIS_LLM_MODEL=anamnesis-qwen
export ANAMNESIS_LLM_MAX_INPUT_TOKENS=24000   # what the window holds after the reply
```

The window is the model's to give — 32768 is qwen2.5's own — and the budget is
yours to fit inside it: a larger budget than the window is the silent failure
above, not a fuller page.

Measured rather than assumed: a 123-observation session reported 7394 input
tokens, comfortably past both 4096 and the 6500 the budget then allowed.

**A reasoning model can spend the whole reply budget thinking.** It comes back
as HTTP 200 with a full `reasoning` field and an empty answer, and anamnesis
says so by name rather than reporting a parse error. `deepseek-r1` needed the
budget raised to 4000 to answer at all; at the default 2000 it thought until
the tokens ran out.

**A model that satisfies the schema can still write a poor page.** The reply is
constrained to the fields consolidation asks for, and every one of them is
checked before a page is written — but nothing can check whether the prose
inside them is any good. A small local model produced a valid page whose body
was JSON fragments. The page is only as good as the model; the schema keeps it
*parseable*, not *worth reading*.

**Nothing depends on the model being reachable.** If a request times out, is
declined, or comes back as something other than a page, the counted summary is
written instead and the reason is logged. Consolidation also runs *after* the
hook's response is sent, so a slow model delays the page, never the session.

#### When the model does not answer

A free tier's daily quota, or an afternoon of `503 high demand`, turns every
session in that window into a counted page. A chain of fallbacks turns it into
a page written by the next model that answers:

```bash
export ANAMNESIS_LLM_PROVIDER=google
export ANAMNESIS_LLM_MODEL=gemini-3.5-flash
export ANAMNESIS_LLM_FALLBACK_PROVIDERS=google:gemini-3.6-flash,ollama:qwen2.5:7b-instruct
```

Each entry is `provider[:model]`, split at the first colon, so a local model's
own tag survives. They are asked in order, and only after the one before has
failed **transiently** and spent its own retries: a timeout, a refused
connection, a rate limit, a server fault, a reply that did not parse. A bad
key, an unknown model, a refusal, or a reply too long for its budget stops
there, because each is something to fix or decide rather than route around.

A link to the same backend keeps its address and key — another model on a
separate quota is the common case. A link to a different backend takes that
backend's own key (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`),
never `ANAMNESIS_LLM_API_KEY`, which belongs to the configured provider. Every
link shares the configured budgets, timeout and effort, and a chain that cannot
be built is refused at startup like any other setting.

**Every link is sent the prompt the configured budget built.** The transcript
is fitted to `ANAMNESIS_LLM_MAX_INPUT_TOKENS` once, before the first link is
asked, and a fallback gets that same text. A hosted model with a large window
takes it whole. A local one may not: Ollama serves 4096 tokens unless the model
says otherwise, and what does not fit is dropped rather than refused, so the
fallback's page would read fine and be written from part of the session with
nothing on it saying so. Before putting a local model in a chain, give it a
window that holds the input budget and the reply (see *A model on this
machine* for the `Modelfile`), or lower the budget to what it holds — or keep
the chain to hosted models on separate quotas.

A page a fallback wrote says so at the bottom — `Written by qwen2.5:7b-instruct,
standing in for gemini-3.5-flash, which did not answer.` — and its session is
recorded against the model that wrote it. `anamnesis serve` prints the whole
chain. `anamnesis reconsolidate` does not use it: it replaces pages that
usually already had a good one, and a stand-in's page over it is the loss a
counted one would be.

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

This binds `127.0.0.1:8080` and serves `POST /hook`, `GET /handoff`,
`GET /whoami`, `GET /health`, and the wiki browser at
<http://127.0.0.1:8080/ui> — scopes, the pages in one, one page rendered, and
a search box that runs the same fused query an agent's `memory_query` does.
Its front page also answers the question `anamnesis status` answers — what
this server is doing, and when each scope last recorded an event — from any
browser that can reach it, which need not be the machine memory lives on.
A scope whose wiki and index have drifted apart says so: pages the index has
never seen (search cannot find them yet) and rows whose file is gone. Both are
what `anamnesis reindex` repairs. Proposals waiting on a person are listed
there too, each with the `anamnesis improve --apply <id>` that carries it
out — the browser shows them and never acts on them.

Each page says whether the decay sweep can reach it at all — `pinned`,
`semantic`/`procedural`, `canonical` and `do-not-answer-from` are out of
reach — and, when it can, what it is judged on: tier, age, and how often it
has been read. The score itself belongs to `anamnesis sweep`, which reads the
`[decay]` table in the project's marker.

It is read-only, and `serve --no-ui` leaves it out. Opening a page does not
count as reading it, because the decay sweep watches those counters; being
handed one by a search does, exactly as `anamnesis search` already does. On a
server that requires a token, the browser asks for a username and password:
any username, and the token as the password.

By default no token is required, which is why the default bind is loopback:
there, the port is the boundary. To serve any other address, protect it first —
see [Requiring a token](#requiring-a-token).

The MCP server is a separate process the agent launches itself; `serve` does
not start one.

#### Keep it running

Started by hand in a terminal, the server lives exactly as long as that
terminal. This repository's own memory recorded nothing for four days for that
reason: the window was closed, every hook after it failed to connect, and the
only report was a line on stderr that no harness shows. A session that starts
while the server is down now says so in the agent's context, and `anamnesis
status` says it any time — but the fix is to not need either.

Closing the window is at least no longer abrupt. The server takes it as a
request to stop: it finishes the summaries it owes and writes down what stopped
it, so a gap in `logs/` that begins with a reason is a different thing from one
that begins with nothing. A server that stopped politely is still a server that
is not recording.

Whatever you use, four settings matter, and each one is a way it has actually
stopped or would:

- **start it at login**, since the terminal it was started from will close
- **restart it if it dies**, since a crash is otherwise indistinguishable from
  never having started
- **never time it out.** Windows Task Scheduler kills a task after three days
  by default, which reintroduces the same silent failure on a schedule
- **refuse a second copy**, so a restart attempt against a live server is
  dropped rather than fighting it for the port

One command writes all four for this machine's service manager, from the copy
of the binary the service should run:

```bash
anamnesis service install            # show what would be registered, and what the server will start with
anamnesis service install --write    # register it, read it back, start the server
anamnesis service status             # registered? what does it run? does a server answer?
anamnesis service restart            # after a new key, settings.env or binary: stop it, wait for the new one
anamnesis service uninstall --write
```

- **Windows**: a scheduled task, *Anamnesis Memory Server*, for your account —
  at logon and every minute, `IgnoreNew`, no time limit, both battery settings
  off. The action is `conhost.exe --headless`, so there is no window and no
  elevation. What Task Scheduler holds is read back after registering, and a
  task missing any of those settings is refused rather than reported as
  installed.
- **Linux**: a systemd user unit, `Restart=always`, with an optional
  `EnvironmentFile` in the data directory for keys. `loginctl enable-linger`
  keeps it past logout.
- **macOS**: a launchd agent with `RunAtLoad` and `KeepAlive`.

Before anything is registered it says what the server will start with — model,
vectors, the settings file — read the way the service will read them, from
`settings.env` and the credential store and not from your shell. A setting
exported in the shell but missing from the file is named, because that is the
server that runs perfectly and summarises every session by counting. A binary
in a cargo `target/` directory is refused: the service would run whatever the
next build leaves, and on Windows keep that build from replacing it. If a
server is already answering on the port, the service is registered but not
started, and it says so.

#### By hand

What the command writes, and why each piece is there, for a machine where it
cannot be run or a setup that needs something it does not do.

**Windows.** Register it as a logon task for your own account:

```powershell
$exe = Join-Path $env:APPDATA 'anamnesis\bin\anamnesis.exe'

# No wrapper around it: the principal below leaves the task no desktop for a
# console to appear on, so the server is launched directly.
$action = New-ScheduledTaskAction -Execute $exe -Argument 'serve'

# Two triggers. The first covers the ordinary case. The second is what makes a
# crash survivable: Task Scheduler's own "restart on failure" does **not**
# cover the launched program exiting non-zero — killing the server leaves the
# task in Ready with result 1 and nothing restarts it. A trigger that fires
# every minute restarts a dead server and, with IgnoreNew below, does nothing
# at all to a live one.
#
# No -RepetitionDuration: an absent <Duration> in the task XML means repeat
# indefinitely. [TimeSpan]::MaxValue looks like the way to say that and is not
# - it serialises to P99999999DT23H59M59S, which Task Scheduler rejects as out
# of range, refusing the whole registration.
$triggers = @(
    (New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME),
    (New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) `
        -RepetitionInterval (New-TimeSpan -Minutes 1))
)

$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries -DontStopOnIdleEnd -StartWhenAvailable `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew

# S4U — "run whether the user is logged on or not" — and this is the setting
# most people arrive at this section looking for. A task registered without a
# principal runs interactively, an interactive task has a desktop, and a task
# with a desktop shows a console window every time it really launches
# something. With the repeating trigger above that is not once: it is every
# login and every recovery, each one a window that appears, prints the startup
# banner, and goes. Hiding it does not work either — wrapping the action in
# `powershell.exe -WindowStyle Hidden` still flashes, because the console
# exists before PowerShell has started far enough to hide it. S4U gives the
# task no desktop, so there is no window to hide.
$principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" `
    -LogonType S4U -RunLevel Limited

Register-ScheduledTask -TaskName 'Anamnesis Memory Server' `
    -Action $action -Trigger $triggers -Settings $settings -Principal $principal -Force
```

S4U runs the task as you without storing your password, which needs the *log on
as a batch job* right. If the registration is refused with a message about the
logon type, that right is what is missing; granting it, or registering the task
from the Task Scheduler UI with "run whether user is logged on or not" ticked,
is the same thing by another route.

On a machine where you are not an administrator the refusal is a bare
`Access is denied`, and it is worth confirming that S4U is what was refused
rather than the registration as a whole: the same task with
`-LogonType Interactive` registers without elevation, so if that fails too the
problem is somewhere else. Granting the right needs elevation either way. From
an elevated PowerShell, registering the task above is enough — Task Scheduler
grants the right as part of accepting an S4U principal.

**Without elevation.** Interactive is the only principal left, and an
interactive task has the desktop that produces the console window. What removes
it is not hiding the window but never letting one be drawn:

```powershell
# serve-hidden.vbs, beside the binary
$vbs = @'
Dim shell, exe
Set shell = CreateObject("WScript.Shell")
exe = shell.ExpandEnvironmentStrings("%APPDATA%") & "\anamnesis\bin\anamnesis.exe"
shell.Run """" & exe & """ serve", 0, True
'@
$vbs | Set-Content -Encoding ascii (Join-Path $env:APPDATA 'anamnesis\bin\serve-hidden.vbs')

$action = New-ScheduledTaskAction -Execute "$env:SystemRoot\System32\wscript.exe" `
    -Argument "`"$(Join-Path $env:APPDATA 'anamnesis\bin\serve-hidden.vbs')`""
$principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" `
    -LogonType Interactive -RunLevel Limited
# $triggers and $settings exactly as above.
```

`wscript.exe` is a GUI-subsystem program, so no console is created for the task
itself, and `Run`'s second argument — `0`, `SW_HIDE` — means the server's own
console is created hidden rather than shown and then hidden. That is the
difference from `powershell.exe -WindowStyle Hidden`, which flashes because the
console exists before PowerShell has read its own arguments.

The third argument is the one to get right. `True` means *wait*, and without it
`wscript` returns immediately, the task drops to `Ready` while the server is
still running, and the repeating trigger starts a **new** server every minute —
`MultipleInstances IgnoreNew` only protects a task that is still running. It
costs one extra process in the tree, and the crash-recovery behaviour is
unchanged: when the server exits, `wscript` exits with it and the next
repetition starts a fresh one.

Point it at the copy under `%APPDATA%\anamnesis\bin\`, not at one in
`target/`: Windows will not let `cargo build` overwrite a running executable.

Then check what you registered rather than what you asked for. A failed
`Register-ScheduledTask` leaves whatever was there before, and the next command
in a script will happily describe *that*, which reads exactly like success:

```powershell
$task = Get-ScheduledTask -TaskName 'Anamnesis Memory Server'
$task.Triggers | Select-Object @{n='type';e={$_.CimClass.CimClassName}},
                               @{n='repeats';e={$_.Repetition.Interval}}
$task.Settings.ExecutionTimeLimit   # PT0S, or it is killed in three days
$task.Principal.LogonType          # S4U, or Interactive via the launcher above
$task.Settings.MultipleInstances   # IgnoreNew, or the repetition stacks copies
```

Two triggers, one of them repeating, `PT0S`, and a principal that leaves the
task no desktop to draw a window on. Killing the server
should then bring it back within the repetition interval - measured at 50
seconds here, and the restart is in `logs/`, where the next person can see that
it happened. The stop before it is in there too, with the reason it stopped,
whenever the server was asked to stop rather than killed outright: a process
ended with `Stop-Process -Force` gets no say and leaves no line, which is
itself worth knowing when reading a gap in the log.

**Linux**, as a user unit in `~/.config/systemd/user/anamnesis.service`:

```ini
[Unit]
Description=Anamnesis memory server

[Service]
ExecStart=%h/.local/bin/anamnesis serve
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

Then `systemctl --user enable --now anamnesis`, and
`loginctl enable-linger $USER` if it should survive logout.

**macOS**, as a launchd agent in
`~/Library/LaunchAgents/dev.anamnesis.server.plist`, with `RunAtLoad` and
`KeepAlive` both true.

Whichever it is, check it the way you would check anything else that claims to
be running: `anamnesis status` names the server, says whether it answers, and
says when it last recorded something. `<data_dir>/logs/` holds what the server
itself said, one file a day, which is the only account left once the terminal
is gone.

### 4. Connect Your Agent

Claude Code talks to anamnesis two ways, and they are independent.

**Hooks** capture the session. Print the configuration and paste it into your
`settings.json`, or `--write` to merge it in:

```bash
anamnesis install-hooks --agent claude-code          # .claude/settings.local.json
anamnesis install-hooks --agent codex --write        # .codex/hooks.json
anamnesis install-hooks --agent gemini-cli --write   # .gemini/settings.json
anamnesis install-hooks --agent cursor --write       # .cursor/hooks.json
anamnesis install-hooks --agent opencode --write     # .opencode/plugins/anamnesis.js
```

For Codex, review new or changed hooks in `/hooks`, then open a fresh session.
Codex trusts each hook definition separately; writing `.codex/hooks.json`
does not make it run. The installer includes `Stop` and `SubagentStop` so the
agent's final explanation and subagent reports reach memory, as well as its
tool calls. After a real prompt, check that `anamnesis status` shows a recent
capture event. MCP access and a successful `hook --probe` establish other
parts of the connection, not that Codex has actually emitted an event. See
[Codex's hook contract](https://developers.openai.com/codex/hooks).

All five capture the same five moments and one server captures all of them,
though each spells the events its own way, Cursor names its fields its own way,
and Gemini CLI and Cursor both want their answers as JSON. Hooks are read when a session starts, so the session you run this from
is not the one that gets captured.

**OpenCode is the odd one.** It extends through a plugin rather than a command,
so `--write` puts a module at `.opencode/plugins/anamnesis.js` instead of
merging JSON into a settings file. The plugin subscribes to `chat.message`,
`tool.execute.after` and `experimental.session.compacting`, synthesises the
session's start from the first event that carries a session id, and sends the
end when OpenCode disposes of it — a session that ends some other way is
summarised by the server's reaper instead.

The handoff arrives differently too. Every other harness injects a hook's
stdout; OpenCode has no such channel, so the plugin pushes the waiting note
into the system prompt (`experimental.chat.system.transform`), labelled as
coming from anamnesis. A file already at that path that anamnesis did not
write is left exactly where it is — the command says so rather than replacing
somebody's own plugin.

**MCP** lets the agent search memory and write pages on purpose. Without it an
agent is handed one summary at startup and can read nothing else, however much
the hooks have recorded — which is the shape this took on the machine anamnesis
is developed on, for four months, with nothing saying so.

```bash
anamnesis install-mcp                        # prints the entry, .mcp.json
anamnesis install-mcp --agent cursor --write     # .cursor/mcp.json
anamnesis install-mcp --agent gemini-cli --write # .gemini/settings.json
anamnesis install-mcp --agent codex --write      # .codex/config.toml
```

Run the copy of the binary you actually use, the same way as for hooks: the
registration names the executable that ran it. `claude mcp add anamnesis --
anamnesis mcp --repo .` does the same thing when `anamnesis` is on `PATH`, and
registers a server that cannot start when it is not.

`.mcp.json` is read from the project root and is often committed, but what
`--write` puts there is an absolute path on one machine. Ignore it, or expect a
colleague's checkout to point at your home directory.

Four harnesses, three shapes of file. Cursor and Gemini CLI keep the same
`mcpServers` object Claude Code does, in their own directories; Gemini's is the
settings file its hooks already live in, and only the `mcpServers` key is
touched. Codex is TOML, and its table is `mcp_servers` rather than
`mcpServers` — merged with `toml_edit`, so the comments and key order in an
existing `config.toml` come back as they were.

OpenCode's hooks are wired (above), but its MCP registration is not written
for it: its configuration is its own shape and `install-mcp` has not learned
it. The server itself is harness-agnostic — `anamnesis mcp --repo <dir>` over
stdio is all any of them need, including OpenCode.

Hooks need `anamnesis serve` running. MCP does not — it opens the store
directly.

### 5. Start Your Agent Through Anamnesis

Once the hooks are wired, the remaining way to lose an afternoon is to start a
session when the server is not running. `run` checks before it starts:

```bash
anamnesis run claude-code            # checks, then launches `claude`
anamnesis run codex -- --model o3    # everything after `--` goes to the harness
anamnesis continue                   # whichever harness ran the last session here
```

If the server is not answering, or this harness's hooks do not point at
anamnesis, nothing is launched and the message carries the command that fixes
it:

```
⏸  Not starting claude-code: the memory server at http://127.0.0.1:8080 is not answering.

    anamnesis serve

  Or `--anyway` to start without a memory of it.
```

Before launching, `run` also probes `/hook` with the selected token and project
marker, without recording an event or consuming a handoff. A listener answering
`/health` is not enough: it may refuse capture. Failed preflight exits nonzero;
`--anyway` explicitly skips the refusal. This checks the receiving side; the
harness still has to load and trust its hooks, and actual capture should be
confirmed with `status` after a real prompt.

`--program` names the executable when a harness is called something else on
this machine — the launcher tries `claude`, `codex`, `cursor-agent`, `gemini`
and `opencode`. The server address is passed to the harness in its environment,
so the hooks inherit it: a project wired to one server can be run against
another without touching a settings file.
The managed child receives `ANAMNESIS_RUN_SERVER` as an override even when an
installed hook still contains an older `--server` argument. Both launcher and
hook binary need this behavior; reinstall the OpenCode plugin after upgrading
so its handoff request uses the override as well.

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

A page written that way is episodic, and the decay sweep will eventually reach
it. A decision usually should not be:

```bash
anamnesis write-page \
  --path decisions/0002-storage.md \
  --title "Storage: one file, no server" \
  --body "..." \
  --tier semantic \                        # durable; the sweep does not reach it
  --canonical \                            # authoritative on its subject
  --entity SQLite --entity rusqlite \      # what the entity stream matches on
  --supersedes decisions/0001-database.md  # recall stops offering the old one
```

`--entity` also takes a comma-separated list. `--status` sets the trust level
(`active`, `historical`, `do-not-answer-from`, `superseded`); a misspelled tier
or status is refused rather than defaulted, because filing a page as episodic
when `semantic` was meant puts it where the sweep can reach it.

`write-page` creates only. To update an existing page, first read its exact
revision and then patch only the fields you mean to change:

```bash
anamnesis show-page decisions/0002-storage.md
anamnesis patch-page \
  --path decisions/0002-storage.md \
  --expected-revision <revision-from-show-page> \
  --body "Corrected decision text"
```

Omitted frontmatter is preserved and listed in the response. Nullable fields
are removed only with an explicit `--clear-field`, for example
`--clear-field supersedes`; doing that may make the predecessor current again,
so the command prints the chain effect as a warning.

### Share a Page Across Projects

Some things are true of every project you work on, not one of them. Those go
in the workspace's shared scope:

```bash
anamnesis write-page --global   --path policy/databases.md   --title "We use PostgreSQL"   --body "Every project in this workspace stores its data in PostgreSQL."   --tier semantic --canonical
```

Every project in the same workspace finds it, and search says where it came
from:

```
policy/databases.md  We use PostgreSQL [canonical] (_global)
    semantic · score 0.0164
```

It is inheritance, not merging: the page stays in `_global` and nothing is
copied into a project. One shared scope per workspace — two workspaces are two
memories. When a project's own page and a shared page score the same, the
project's wins; it is the more specific answer.

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

Before actually opening a port to other machines, read
[REMOTE.md](REMOTE.md): TLS, the proxy body limit that would otherwise refuse
ordinary events, per-operator handoff slots, the audit log, and a checklist
that ends with the two-line test for whether the thing is readable by everyone
who can reach it.

### Read What the Last Session Left

```bash
anamnesis handoff        # peek, without consuming it
anamnesis sessions       # recent sessions, newest first
anamnesis show-page bootstrap/repository.md
```

Peeking never consumes it: looking is what a person does, claiming is what a
starting session does, and conflating the two would mean checking on a note
costs the next session its context.

A note that is wrong — written from a bad model reply, or about work that was
abandoned — can be thrown away instead:

```bash
anamnesis handoff --discard
```

It prints what it dropped, because a handoff being discarded should be seen
once by somebody in case it was not the one they meant. The row is kept and
marked expired, the same state a newer handoff already puts an older one in: a
record saying a note was written and never delivered is a more honest account
than no record. Without this the only way to be rid of one is to let a session
claim it — which puts it in that session's context, which is the thing being
avoided.

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

### Measure What This Machine Can Record

```bash
anamnesis bench                 # 2000 events
anamnesis bench --events 10000
```

```
                           events/s        p50        p95        p99
  parse + redact            274 140     0.00ms     0.00ms     0.01ms
  record (index)              3 708     0.23ms     0.30ms     1.40ms
  record + transcript         1 866     0.48ms     0.66ms     1.86ms
```

The path measured is the one an event takes: parsed and redacted as a
harness's payload is, then recorded as `POST /hook` records it — the marker
file is read and the body is scanned for secrets on every event, because both
happen per event in production. It runs against a temporary data directory, so
nothing reaches this project's memory.

Two things the numbers say. The durable transcript under `raw/` costs about
2×, which is what the copy that survives losing the index is worth. And
against the hook's one-second budget there is room for roughly 1 500 events,
so on a machine like this one recording is not what a session waits on.

A number is only comparable to itself: run it before and after a change to the
capture path, on the same machine, and the difference is the finding.

### Score Retrieval

The question this answers is not "is the code correct" but "does memory find
the page that answers this":

```bash
anamnesis eval             # the suite built into the binary
anamnesis eval --verbose   # every case, with the rank its answer came back at
anamnesis eval --check     # exit non-zero when a suite is below its thresholds
anamnesis eval --streams   # what each stream contributes on its own
```

```
🎯 crowded — Ranking under competition: a corpus where more than one page could plausibly answer
   crowded · 22 pages · 15 cases · scored over the first 5

   Hit@1   0.933  (bar 0.930) ok
   MRR     0.967  (bar 0.960) ok
   NDCG@5  0.975  (bar 0.970) ok
   Recall  1.000  (bar 1.000) ok

   category      n   Hit@1    MRR   NDCG  Recall
   keyword       5   1.000  1.000  1.000   1.000
   natural       1   1.000  1.000  1.000   1.000
   paraphrase    4   0.750  0.875  0.908   1.000
   symptom       4   1.000  1.000  1.000   1.000
   temporal      1   1.000  1.000  1.000   1.000
```

Four numbers because they fail differently. **Hit@1** is what the agent is
actually handed — it reads the top result and works from it. **MRR** notices an
answer sliding from first place to third while hit@1 has already written it off.
**NDCG@5** is the one another project's published figure can be held against,
discounted over however many results the suite scores (the `@5` travels with it
for that reason). **Recall** says only whether the page came back at all.

The second table is the same four numbers per **kind of question**, and it is
where a trade stops being able to hide. The 0.933 above averages to nothing much;
the row below it says the whole shortfall is `paraphrase` — questions whose
words are not on the page that answers them — while bare keywords, symptom
descriptions and everything else come back first every time. A change that helps
one kind and costs another reads as "no change" in a total and as a trade in the
table. Read the `n` column first: over four questions a rate moves in quarters.

A suite declares its own categories, and a case in one it did not declare is
refused rather than filed under a new label; a suite that labels nothing is
scored exactly as before.

Two more lists print when they have anything in them: questions nothing relevant
came back for, and questions answered so far down the page nobody would scroll
to the answer. A suite passing on average with one question unanswered is the
result most likely to be read as fine.

The corpus is checked in at `crates/anamnesis-evals/suites/`, and a run builds it in a
throwaway directory — it never touches your own memory, because every query
would otherwise count as a read and the decay sweep believes those. Write your
own with `--suite path/to/suite.toml`; the format is the shipped file. A suite
is gated on the measures it names in `[thresholds]` and no others, so an older
suite goes on meaning what it meant.

#### Against your own memory

The shipped suites say how retrieval does on pages written for them. Whether a
setting that wins there also wins on the memory you actually have is a second
question, and it is asked of a copy:

```bash
anamnesis backup --out memory.tar.gz
anamnesis eval --suite my-questions.toml --pages-from memory.tar.gz
anamnesis eval --suite my-questions.toml --pages-from memory.tar.gz --embed --compare vectors=0
```

`--pages-from` takes an archive written by `anamnesis backup`, or a data
directory. Its wiki pages become the corpus — frontmatter and all, so a
superseded page stays out of the answers as it is in real use — and the corpus
is built in a throwaway directory like any other: an archive is unpacked
there, and a data directory is only read as files, so nothing is counted
against the memory the pages came from. A page that does not parse is left out
and named.

The questions file is a suite with no `[[page]]` in it: `name`, `description`,
optionally `categories` and `limit`, and the `[[case]]`s, each naming the paths
that answer it. A case naming a page the copy does not have is refused before
anything runs. When the copy holds more than one project, say which with
`--scope workspace/project`.

Your questions are about your notes, so keep the file wherever the notes are
kept rather than beside the shipped suites. Write them before the first run
and do not edit them after: a set tuned to the scores it produced measures
nothing.

### Turn On Semantic Search

Retrieval fuses four streams, and one of them is off unless asked: cosine
similarity against a local embedding model. It costs a download of about 90 MB
on first use, into `<data_dir>/models/`, and runs on CPU.

```bash
export ANAMNESIS_EMBED_ENABLED=1
```

Set it in the environment the **server** runs in, and in any shell you run
`anamnesis search` from. Everything that writes a page then embeds it —
consolidation, the wiki watcher, `write-page`, `bootstrap`, and the MCP tool.

#### Or from an API instead

On a small server the 90 MB download and the CPU that inference wants can be
the difference between memory being cheap to run and memory being the reason
the box is busy. Any OpenAI-compatible `/v1/embeddings` endpoint can do it
instead:

```bash
export ANAMNESIS_EMBED_ENABLED=1
export ANAMNESIS_EMBED_PROVIDER=openai
export ANAMNESIS_EMBED_API_KEY=sk-...        # or OPENAI_API_KEY, which it falls back to
# Optional, with these defaults:
# export ANAMNESIS_EMBED_MODEL=text-embedding-3-small
# export ANAMNESIS_EMBED_URL=https://api.openai.com/v1/embeddings
```

Local stays the default, and a misspelled provider name is local rather than
an error: this setting is how you opt *into* sending every page and every
query to somebody else, and a typo must not be a way to end up doing that.

#### Recommended where Ollama runs: nomic-embed-text

The built-in model reads the first 128 tokens of a page. Most pages a
consolidator writes are longer than that — on this project's own memory, 50 of
56 — so the vector stream answers from each page's opening and nothing below
it. `nomic-embed-text` reads 8192 tokens, and on a question set frozen before
it was run over that memory it scored hit@1 0.875 / MRR 0.931 against MiniLM's
0.833 / 0.903, losing nothing on keyword or natural-language questions
(`docs/DIRECTION.md` has the measurement). It runs in Ollama, on the same
machine, with no key and nothing leaving it:

```bash
ollama pull nomic-embed-text

export ANAMNESIS_EMBED_ENABLED=1
export ANAMNESIS_EMBED_PROVIDER=openai
export ANAMNESIS_EMBED_URL=http://127.0.0.1:11434/v1/embeddings
export ANAMNESIS_EMBED_MODEL=nomic-embed-text

anamnesis reindex          # every page gets a nomic vector beside its old one
```

It is not the default for one reason: it needs Ollama to be running. When it is
not, queries carry on without the vector stream and a page written meanwhile
records an embedding failure that `anamnesis doctor` reports. So start Ollama
with whatever keeps the server running — before it, in the same script — rather
than from a terminal that will be closed.

Two places need these variables, not one: the **server's** environment, and
the MCP registration, since the agent's `memory_query` embeds the question in
its own process. Run `anamnesis install-mcp --write` from a shell that has them
and it carries them into the registration — every setting, never a key. An
endpoint that does need one reads it from `ANAMNESIS_EMBED_API_KEY` in the
environment the harness starts in, and the command says so.

A registered MCP server whose endpoint does not answer when the harness starts
it — Ollama not up yet after a reboot — starts anyway, says so on stderr, and
asks the endpoint again when a query needs a vector, at most every thirty
seconds. Queries in between go without the vector stream rather than the agent
going without its memory tools. `anamnesis serve` does the same: a service
manager's refusal log is written nowhere anybody reads, and a server that would
not start without Ollama recorded nothing for as long as Ollama stayed down.
It says so in its log (`going on without vectors, asking again when one is
needed`).

A page the server writes while the endpoint is down is indexed without a
vector, and `anamnesis doctor` names it. Once a minute the server sends the
endpoint one short string, and when that is answered it embeds those pages
again, twenty at a time — so starting Ollama is the whole fix, without a
restart or a `reindex`. An endpoint that answers and refuses, a wrong key or a
model that does not exist, is treated the same way and says why in the same
log line, so read it when `doctor` keeps naming the same pages.

**Switching providers does not corrupt anything, and does not migrate
anything.** Every vector is stored beside the name of the model that produced
it, because two models put vectors in unrelated spaces and cosine similarity
between them is a number with no meaning. Pages embedded by the old model are
simply not consulted until `anamnesis reindex` writes new ones.

**Pages written before you turned it on have no vector**, and nothing
backfills them on its own. One command does:

```bash
ANAMNESIS_EMBED_ENABLED=1 anamnesis reindex
```

What it buys is the question whose words are not in the answer. From this
repository's own memory, the same query twice:

```
$ anamnesis search "who has been committing here"
bootstrap/hotspots.md      Where the work concentrates

$ ANAMNESIS_EMBED_ENABLED=1 anamnesis search "who has been committing here"
bootstrap/contributors.md  Contributors
```

Neither page says "committing here". Full text picked the one about *files*
because it says "commits" more often; the embedding stream picked the one about
*people*.

Scored rather than assumed: `anamnesis eval --embed` puts the stream's own
recall second of the four, and with it on, the questions only full-text search
can answer drop from three to one on one suite and from eight to two on the
other.

### Forget a Page on Purpose

`sweep` forgets what decayed. This forgets what was *wrong* — a page written
from a bad model reply, a note that turned out to be untrue, a duplicate:

```bash
anamnesis forget sessions/2026-08-29-3da85483.md
```

It removes the index row first and the file second, so an interruption leaves
the page briefly unfindable and wholly recoverable with `anamnesis reindex`,
rather than leaving the index pointing at markdown that is gone.

No `--apply`, unlike the sweep: a sweep proposes a judgement over pages nobody
named, and its report is where that judgement gets checked. Here you named the
page. What the command owes you instead is to say what it removed and where it
went — the wiki is a git repository, so the commit it prints will still have
the content:

```bash
git -C <data_dir>/wiki show <commit>
```

A path that names no page is refused before anything is removed, and so are
all of them if any one is wrong: forgetting two pages and then complaining
about the third leaves you working out which name was the typo.

### Forget a Session That Was Never a Session

`forget` removes a page. This removes the other half of what memory holds: a
session, its observations, and its transcript.

It exists for the sessions nobody meant to record. Firing a hook by hand to
check whether capture is alive is the ordinary way to answer that question,
and every such probe is recorded exactly like somebody's afternoon — counted
in `status`, listed by `sessions`, and eventually summarised into a page of
its own. Those transcripts are easy to spot once you know the tell: a probe
you typed carries the `cwd` you typed, while a harness sends the path in its
own shape.

```bash
anamnesis sessions
anamnesis forget-session a920ec80          # reports, removes nothing
anamnesis forget-session a920ec80 --apply  # removes it
```

Any unambiguous prefix names a session, the way `sessions` prints them. A
prefix matching more than one is refused with all of the candidates rather
than acted on, and an empty one — which abbreviates every session in the
project — is refused outright.

Unlike `forget`, this is gated behind `--apply`. The reasoning for a page
rests on the wiki being a git repository, so a page removed by mistake is
still in its history. Nothing plays that part here: the transcript under
`raw/` is the only copy of what a session observed, and it is not versioned.
Where there is no history to fall back on, the report before the fact is the
whole safety net.

The index row goes first and the transcript second, so an interruption leaves
a session that `anamnesis reindex` restores whole — that is where reindex
rebuilds sessions from. A page the session already produced is not touched;
`anamnesis forget` is what removes one of those.

### Rename a Project

Identity is derived from the marker file or the git remote, so renaming a
repository or moving it to a new remote makes the next session resolve a
different key — and find an empty project, with the old memory still there
under a name nobody types any more:

```bash
anamnesis rename new-name              # says exactly what would move
anamnesis rename new-name --apply
```

The pages move as one commit git reads as a rename, the index moves in one
transaction with every derived identifier recomputed, the transcripts follow,
and the marker file is pinned to the new name — without that last step the
next event would re-derive the old identity and the rename would read as
having quietly failed. Comments in the marker are kept.

Renaming into a project that already has memory is refused. Merging two
memories is a different operation with different answers — which page wins
where both have `decisions.md` — and a rename is no place to decide them.

### Take It Back Out

```bash
anamnesis uninstall              # says exactly what would go
anamnesis uninstall --apply
```

Removes the hooks from every harness's settings file, the MCP registration,
and the OpenCode plugin. **Only what anamnesis wrote**: a project's own hook
beside ours stays, and so does a wrapper script that happens to call
anamnesis.

Memory is untouched. Uninstalling stops the recording; it does not remove what
was recorded. `anamnesis purge --apply` removes this project's memory, and
deleting the data directory removes all of it.

### Start a Project Over

When the memory is wrong rather than incomplete — a repository re-scoped by
accident, a `bootstrap` against the wrong directory — fixing it page by page
is worse than starting again:

```bash
anamnesis purge              # says exactly what would go
anamnesis purge --apply
```

Pages leave as a git commit and stay in the wiki's history. Transcripts do
not: `raw/` is not a repository and it is the only copy of what was said in
those sessions, so take an `anamnesis backup` first if there is any doubt.

The audit line outlives the project. After a purge, `anamnesis audit` still
says who did it and what went — which is the question somebody asks next.

### Rebuild the Index

The database is disposable. If it is lost or corrupted, rebuild it from the
wiki and the transcripts:

```bash
anamnesis reindex
```

Safe to run against a live database: every identifier is derived, so a rebuild
reproduces the same rows rather than duplicating them.

### Back Up

```bash
anamnesis backup                                   # ./anamnesis-backup-<stamp>.tar.gz
anamnesis backup --out /backups/memory.tar.gz
```

One archive: the index, the transcripts, and the wiki including its `.git`.
`models/` and `logs/` are left out — a download any machine can repeat, and one
machine's afternoons.

Safe to run while the server is recording. The index is copied through SQLite's
own backup API rather than by copying the file: in WAL mode the committed
database is spread across `anamnesis.db` and a `-wal` beside it, and copying
the first without the second gives a database that opens, reports a plausible
schema version, and is missing whatever was written most recently — a backup
that is quietly stale, discovered on the day it is needed.

Putting one back says what it would do before it does anything:

```bash
anamnesis restore /backups/memory.tar.gz           # reports, writes nothing
anamnesis restore /backups/memory.tar.gz --apply
```

A data directory that already holds memory is left exactly as it was unless
`--force` says otherwise: restoring is the one operation here that running the
other one cannot undo.

The wiki is also an ordinary git repository, so it can be pushed on its own —
useful for reading memory from somewhere else, though it carries only the
compiled half:

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
anamnesis hook --probe       # what would the next event do?
```

The `hook` command always exits 0, but it writes the reason to **stderr**: a
rejected event, an unreachable server, a payload it could not parse. If your
harness hides hook stderr, `--probe` is the way to see the same answer.

A probe sends a payload the server describes instead of storing. It reports
the scope the working directory resolves to, the session the identifier would
derive, whether `[capture] ignore_paths` would drop the event, what redaction
caught, whether a handoff is waiting, and whether summaries are written by a
model or counted:

```bash
$ anamnesis hook --probe
Probing memory at http://127.0.0.1:8080

  Server:     reachable
  Scope:      default/anamnesis
  Session:    fb4d44b0 (new)
  Event:      user-prompt (read as claude-code)
  Redacted:   nothing
  Handoff:    none waiting
  Summaries:  counted - no model configured

  This event would be recorded. Nothing was.
```

Use it rather than firing a hook by hand. A hand-fired hook is a real event,
so it makes a real session that is counted, listed, and eventually summarised
into a page of its own - and if it is a `SessionStart`, it *claims the waiting
handoff* and takes part in deciding what the next session is told. Anything a probe of the older kind already left
behind comes out with `anamnesis forget-session`.

Unlike the hook, a probe exits non-zero when memory would not record, so it
works in a script. It reads a payload on stdin when one is piped, which is how
to ask about a specific event rather than a made-up one:

```bash
anamnesis hook --probe < the-payload-that-went-missing.json
```

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
