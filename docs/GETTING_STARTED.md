# Getting Started with Anamnesis

## Prerequisites

- Rust 1.95 or later
- Git
- SQLite (bundled with project)

## Installation

### From a Release

Every tagged version has binaries attached to it on the
[releases page](https://github.com/berketpbs/anamnesis/releases): Linux
(x86-64), macOS (Intel and Apple silicon) and Windows. Each archive holds the
binary, the README, the licence and the changelog, and every release carries a
`SHA256SUMS` file — a release nobody can verify is a release nobody should run.

```bash
tar -xzf anamnesis-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
./anamnesis-v0.1.0-x86_64-unknown-linux-gnu/anamnesis --version
```

Put the binary somewhere on `PATH`, then `anamnesis init`.

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
| `ANAMNESIS_LLM_PROVIDER` | `anthropic` when a key is set | `anthropic`, `openai`, `google`, `ollama`, or `none`. Set `none` to turn the model off without unsetting the key. |
| `ANAMNESIS_LLM_MODEL` | `claude-opus-5`; `llama3.2` for `ollama`; `gemini-2.5-flash` for `google` | Model id. |
| `ANAMNESIS_LLM_BASE_URL` | per provider | `https://api.anthropic.com`, `https://api.openai.com/v1`, `https://generativelanguage.googleapis.com/v1beta/openai`, or `http://127.0.0.1:11434/v1`. Point at a gateway, a second Ollama, or vLLM. |
| `ANAMNESIS_LLM_EFFORT` | `high` | `low`, `medium`, `high`, `xhigh`, or `max`. `google` has no word above `high` and is sent `high` for the two above it. |
| `ANAMNESIS_LLM_MAX_INPUT_TOKENS` | `6500` | Prompt budget. Long sessions are trimmed from the middle to fit. |
| `ANAMNESIS_LLM_MAX_OUTPUT_TOKENS` | `2000` | Reply budget, floored at 1000. |
| `ANAMNESIS_LLM_TIMEOUT_SECS` | `90` | Per-request timeout. |
| `ANAMNESIS_LLM_MAX_RETRIES` | `2` | Retries, for rate limits and server faults only. |
| `ANAMNESIS_LLM_FALLBACKS` | on | Server-side fallback to another model if a request is declined. |

A typo is reported at startup rather than at the end of the first session:
`anamnesis serve` refuses to bind if the settings do not parse, and prints
which model it will consolidate with when they do. `anamnesis status
--verbose` reports the same thing.

#### Google AI Studio

Gemini publishes an OpenAI-compatible surface, so it is the same client again —
only its address and model differ:

```bash
export ANAMNESIS_LLM_PROVIDER=google
export GEMINI_API_KEY=AQ....                 # or GOOGLE_API_KEY
export ANAMNESIS_LLM_MODEL=gemini-2.5-flash  # whatever your account lists
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
session goes out at up to `ANAMNESIS_LLM_MAX_INPUT_TOKENS` (6500) and the reply
budget is 2000 on top; Ollama serves 4096 unless the model says otherwise, and
what does not fit is dropped rather than refused. The page comes back valid,
readable, and quietly missing the middle of the session. Give the model a
window that holds both — a `Modelfile` is the durable way, since it travels
with the model instead of with whoever remembers to set an environment
variable:

```
FROM qwen2.5:7b-instruct
PARAMETER num_ctx 12288
```

```bash
ollama create anamnesis-qwen -f Modelfile
export ANAMNESIS_LLM_MODEL=anamnesis-qwen
```

Measured rather than assumed: a 123-observation session reported 7394 input
tokens, comfortably past both 4096 and the 6500 the budget estimated.

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

`--program` names the executable when a harness is called something else on
this machine — the launcher tries `claude`, `codex`, `cursor-agent`, `gemini`
and `opencode`. The server address is passed to the harness in its environment,
so the hooks inherit it: a project wired to one server can be run against
another without touching a settings file.

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
🎯 retrieval — Recall and ranking over a small project memory, through the real query path
   retrieval · 10 pages · 10 cases · scored over the first 5

   MRR     0.708  (bar 0.700) ok
   Recall  1.000  (bar 1.000) ok

   Answered, but not near the top:
     [4] "why is vector search off by default"
         The page says opt-in, not off by default. Different words, same question.
```

The corpus is checked in at `evals/suites/`, and a run builds it in a
throwaway directory — it never touches your own memory, because every query
would otherwise count as a read and the decay sweep believes those. Write your
own with `--suite path/to/suite.toml`; the format is the shipped file.

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

The server checks the endpoint while it starts, by embedding one short string
— so a wrong key or a model that does not exist is an error you see at startup
rather than a log line hours later, after sessions have been summarised
without a vector each.

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
handoff*, which is single-use. Anything a probe of the older kind already left
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
