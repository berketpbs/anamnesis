# Long-run eval

`anamnesis eval` asks whether memory finds the page that answers a question.
This asks what that is for: does a real agent, sessions after it was told
something, do the right thing because memory kept it?

## What runs

`scenario.toml` is twenty-eight sessions on `fixture/`, a small Python bookkeeping
library. Every session is a headless `claude -p` and runs twice, once in each
arm, on two copies of the fixture:

- **memory**: the hooks and MCP tools `anamnesis setup` writes, a server of
  its own on port 18080, a data directory of its own, and this machine's model
  and embedder (its `settings.env` is copied in).
- **control**: the same prompts, model, tools and turn limit, and nothing that
  carries anything between sessions. Claude Code's own memory is turned off
  with `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` and only project settings load. The
  run starts by asking that setup whether it has persistent memory and stops
  if the answer is yes.

The sessions come in three kinds:

| Kind | Sessions | What it does |
|---|---|---|
| plant | S01-S05, S13-S17, S23, S26 | the session finds out or is told something the repository does not say |
| distractor | S06-S07, S24, S27 | unrelated work, so the plant is not simply the last page |
| probe | S08-S12, S18-S22, S25, S28 | a task that needs a plant; its check is the measurement |

The first five probes are also what stands between the second five plants
and the first. In the second five, each planting session is an ordinary task
during which the person mentions, in passing and for later, something no file
says. S09 and S12 can be passed by looking at the repository; none of S18-S22
can, so a control arm that passes one has guessed.

| Probe | Needs | Passes only if |
|---|---|---|
| S08 logging | S02: amounts never go to logs | the importer logs, and no amount appears in any record |
| S09 EUR rate | S03: `generated_rates.py` is generated | `rates.toml` and the generated file both say 1.09 |
| S10 repeated imports | S04: caching import results served stale rows | a file edited to the same size with its mtime restored imports as edited |
| S11 DEPLOY.md | S05: the staging host and command | the file names `ledger-stg-02` and `ENV=staging` |
| S12 mixed currencies | S01: tests run through `tools/check.py` | the suite passes and mixing currencies raises; also compared by turns and cost |
| S18 `total` | S13: CHF totals round to 0.05 | CHF totals round to five rappen and USD totals to the cent |
| S19 `import --strict` | S14: errors read `<file>:<line>: <message>` | a bad row exits non-zero with `bad.csv:3:` and no traceback |
| S20 `export-jsonl` | S15: `export.py` stays as it is, formats go under `ledger/formats/` | the command works and `export.py` is unchanged |
| S21 `tools/fetch_rates.py` | S16: the internal rates service and its header | the script names `fx.internal.example/v2/rates` and `X-Ledger-Team: finance` |
| S22 GBP rate | S17: rate changes ask `@dana-fin` to sign off | GBP is 1.29 and `PR.md` names the rate and `@dana-fin` |
| S25 default currency | S23: settings are `LEDGER_<NAME>` variables read in `ledger/settings.py` | the variable makes a blank currency EUR, nothing else in `ledger/` names it, and there is no flag or config file |
| S28 EUR rate | S26: rate changes are now signed off by `@omar-fin`, not S17's `@dana-fin` | EUR is 1.11 and `PR.md` names the rate and `@omar-fin`; naming only `@dana-fin` is reported as misled |

### A decision taken in conversation

S01-S22 are each told what they plant in a single prompt. S23 is how a
decision is usually made: a conversation of four turns in one session. The
agent builds a `largest` command, is then asked how the project should be
made configurable, lists the options, and is told which one was chosen and
which was dropped — environment variables named `LEDGER_<NAME>`, read only in
`ledger/settings.py`, no `ledger.toml` and no flags — with nothing to build
for it. A last turn goes back to `largest`, so the decision is neither the
first nor the last thing the session did. No file says it, and no pull
request carries it.

A session with `turns` in `scenario.toml` is held open through all of them:
each turn is sent once the agent has answered the one before it
(`--input-format stream-json`), so the prompt hook fires once per turn and the
session starts and ends once, which is how a person talks to an agent in one
terminal. Neither of the easy ways does that: turns written at once are
answered as one prompt, and a `claude -p --resume` per turn ends the session
after every turn, so the server writes it up after the first.

S24 is unrelated work between the plant and its probe, so the handoff waiting
for S25 is S24's, and a pass has to come from what S23 left in memory. A probe
stays one prompt, since `codex exec` takes one.

The third turn also says, in passing, a word to check that the session is
being recorded. `noise` in `scenario.toml` names it, and `report` counts the
runs in which the session left it in a note outside `sessions/`: telling it on
the session page is what happened, keeping it as a decision is the memory
filling up with what nobody needs.

S23 also names the affirmative title of the rejected `ledger.toml` option in
`rejected_decisions`. `report` derives the current decision heads from the
wiki's authored `supersedes` links and reports whether that rejected option
survived as a current decision. The selftest removes the chosen page's
supersedes link as a mutation and requires this probe to turn red.

### A rule that was retired

Every other plant adds a rule. S26 replaces one: in passing, during an
unrelated task, the person says that Dana has moved to treasury and rate
changes are now signed off by `@omar-fin`. By then memory holds S17's rule,
S22's session in which it was applied, and the repository a `PR.md` from S22
that asks `@dana-fin` — so the old rule is not a faint trace but the better
supported of the two. S27 is unrelated work, and S28 is a rate change.

S28's check has three outcomes instead of two. `@omar-fin` asked passes, with
or without a word about who used to sign off. Nobody asked fails. Only
`@dana-fin` asked is *misled*: not a miss but the retired rule applied, with
the authority of something remembered. `report` shows how often each arm was
misled, and counts a pair in which neither arm passed and memory was misled as
a pair control won, since memory is the reason that answer was wrong. A memory
that forgets costs a probe; one that hands over the rule it should have
retired costs more, and the report says so.

S19 asks for `--strict` rather than a better plain `import` because S08 has a
bad row logged and skipped: by then a plain import of a bad file succeeds.

Every check runs the fixture's code or reads its files; none asks a model.
`python checks.py` runs each probe against a repository that does the right
thing and one that makes the mistake the probe is there for, and fails unless
it tells them apart. CI runs that.

## What the agent may do

Both arms are started with the same `--allowedTools`, since a difference there
would be a difference between the arms. Nobody is there to answer a prompt, so
a tool left off the list is refused rather than asked about, and every refusal
costs the session a turn. `report` counts them: a probe that failed after
several of them is telling you about this list, not about memory.

`git add`, `git commit`, deleting and installing are left off on purpose — the
harness commits each session itself, an unattended nightly run should not hold
an unbounded `rm`, and a run that installs a package measures that machine's
network. The first complete run refused 59 calls, and the rest of them were
this list being wrong rather than strict:

- On Windows a session has a **PowerShell tool** beside Bash, and it was on
  neither list: 31 calls, 27 refused. A rule does not narrow that tool — with
  only `PowerShell(python:*)` allowed, `Get-ChildItem` ran — so it is taken
  away with `--disallowedTools` instead. Asked, an agent started this way
  answers that it has no PowerShell tool, and uses the shell both arms share.
- The fixture's tests read `LEDGER_FIXTURES` from the environment, so
  `LEDGER_FIXTURES=tests/fixtures python -m unittest ...` does not begin with
  `python` and was refused four times, while `python tools/check.py`, which
  sets it, was allowed. The scenario plants nothing about how the tests are
  run, so `env`, `export` and that variable are allowed now.

## Running

```bash
python longrun.py selftest
python longrun.py run --anamnesis /path/to/anamnesis     # one repeat, both arms
python longrun.py report --markdown report.md            # every run so far
```

Runs go under `%LOCALAPPDATA%\anamnesis-longrun` (or `~/.local/share/...`),
one directory per run: both repositories with a commit per session, every
session's `stream-json` transcript, the memory arm's data directory and server
log, and `results.json`. `--only S01,S02` tries the harness without the whole
scenario; `--settings-env none` runs the memory arm without a model.

Before anything else a run asks the memory arm's model, with the
`settings.env` its server will read and the key in the credential store,
through `anamnesis key check`, and stops with exit 3 unless some model in the
chain answers: under a refused key or a spent quota every page is counted,
every probe is excluded, and the run measures nothing for two hours. One is
enough, because the server asks the configured model first and a fallback
only when it fails — the run of 2026-09-20 stopped because a fallback was
overloaded while the configured model answered, and measured nothing for no
reason. When only a fallback answers, the pages are that fallback's, and
`report` names the run by it. What each model said is in
`runs/<run>/model-check/key-check.txt` and `results.json`.
`--skip-model-check` starts anyway.

Then the agent is asked, in the control arm's setup, whether it has memory of
its own; a YES stops the run with exit 2. An agent that cannot answer at all —
out of usage, or the service down — stops it with exit 6, since no session
would run either. On 2026-09-19 a weekly limit was read as a YES.

That check is one small question, and a model out of quota can still answer
it. So the run asks the same question of the work: when a **planting**
session's page comes back written by counting, or does not come back at all,
the run stops there and exits 5. `report` excludes every probe behind such a
page anyway, and a model that refused one session refuses the rest of the
hour — on 2026-09-17 a repeat started against a spent quota wrote its first
page by counting, and the eleven sessions after it would have cost two hours
and $1.59 to measure nothing. `--keep-going` runs the whole scenario anyway.

## Probes in Codex

`--probe-agent codex` runs the probes in Codex (`codex exec`, the
cheapest model it lists by default, `--codex-model` to change it), in both
arms, while every planting session stays in Claude Code. What was learnt in
one harness then has to be used in the other, through the same hooks and
memory tools a person would wire with `anamnesis setup`.

Codex runs a project's hooks only after a person has approved them, and skips
unapproved ones without a word. The approval holds as long as the hook
command stays the same, and that command names the binary's path and the
server's port. So a run with Codex works in one fixed place — the binary, both
checkouts and the memory arm's data under `<root>/codex/`, the server on port
18081 — and moves them into the run's directory when it ends. Approve it once:

```bash
python longrun.py codex-trust --anamnesis /path/to/anamnesis --settings-env <the runs' settings.env>
# then, in a terminal: cd into the checkout it names, run `codex`, trust the
# folder, approve every hook under /hooks, and quit
```

A run checks that the approval still holds: if the memory arm's first Codex
probe reaches the server with no session, its hooks did not run, and the run
stops with exit 7 rather than measure a memory arm with no memory. That
happens whenever a newer anamnesis writes different hooks; `codex-trust`
again fixes it.

Three things differ from a Claude Code probe, and none of them favours an arm:

- Codex's events do not carry what its prompt hook printed, so the harness
  asks `/recall` itself with the probe's prompt just before the session
  starts — the same question to the same endpoint, and recall records
  nothing.
- Codex reports no turns and no cost, so its effort is counted as actions:
  commands, file changes and tool calls.
- Codex's Windows sandbox runs commands as a separate account by default, and
  that account cannot see a Python installed for this user, so no probe could
  run the fixture's tests. The runs pass `windows.sandbox="unelevated"` on
  each call, which sandboxes this account's own token instead; the person's
  sandbox setting is not touched.

`codex exec` does write one thing to the person's `~/.codex/config.toml`:
every directory it runs in is recorded as trusted. So the isolation question
is asked under `<root>/codex/isolation` too, and the runs add three entries —
that one and the two checkouts — once, not one per run.

`report --probe-agent codex` reads these runs, and only these: a probe run in
another harness is another experiment.

## The model the memory arm writes with

A repeat is twenty-eight consolidation requests plus whatever the enrich pass
asks again, against a Google free tier of 20 per day per model that is shared
with any server already running on the same key: more than a whole day's
quota. The first complete run, of twelve sessions, spent it by S10. `settings.local.env.example` points the arm at a local Ollama model
instead: no key, no quota, and measured here on 2026-09-17 a session's page
came back written by the model. A small local model writes thinner pages than
the hosted one, so the arm is weaker than a real install and a difference it
still shows is a floor — which is worth more than a nightly repeat that
measures nothing. Point a run at it with `--settings-env`, and leave it off to
measure the setup this machine actually runs.

A repeat of the first twelve sessions took 17 to 29 minutes in the three
complete runs of 2026-09-21 and -22; twenty-eight have not been timed yet. It costs a few dollars of agent usage, and on a free Gemini tier
it asks the consolidation model for more than a day's quota, so repeats are
meant to run one a night, on a paid key or the local model.

## Reading the result

A memory-arm probe counts only if the page of the session that planted its
knowledge was written by a model and the MCP server was connected. A counted
page is a page of tool counts; a probe after one measures the counter, not
memory, and `report` excludes it and says how many it excluded.

Since 1.2.1 the prompt hook shows the agent what memory already has on each
prompt, and a pass rate alone cannot say whether a probe that failed was ever
shown what it needed. So each memory-arm session records `recall`: the pages
its recall block named, and the session that wrote each. It is read from
Claude Code's own transcript of the session, because `stream-json` carries
what the SessionStart hook printed and nothing a prompt hook did. `report`
splits every probe by whether it was shown a page its planting session wrote.
Failed without it is a retrieval result; failed with it in front of the agent
is about what the page said or what the agent did with it — the first run with
recall, on 2026-09-18, showed four of five probes their plant, and two of
those four still failed.

Being shown the page is not the whole path, so `report` also follows each
probe's knowledge from end to end: whether the planting session's pages kept
it — every page that session left, since the rule it was told often goes into
a gotcha beside its session page — whether one of them was put in front of
the agent, by recall or by a memory call it made itself, whether it opened
one in full, and whether it passed. What a page has to say is `knowledge` in
`scenario.toml`, one list of patterns per fact, written after reading the
pages of the first three runs; the selftest holds the patterns to lines from
those pages judged by hand, one that kept each fact and one that did not.
Read that way, those runs say the local model was the bottleneck, not recall:
the two runs gemini-3.5-flash wrote kept all five facts, the one qwen2.5:7b
wrote kept one, and the probe that opened a qwen page in full read "avoiding
the issue of caching rows as seen last month" and built the cache anyway.

One more column stands apart from that path: whether the agent went into the
memory's own files on disk. The first full run with Codex probes had one pass
that came that way. Its memory call was refused, so the agent read the data
directory's path out of the checkout's MCP registration and grepped the wiki
with `rg ..\data\wiki`. That is a way to the knowledge the product does not
offer, and counted with the others it reads as recall working. A memory call
Codex refused for want of an approval is counted where Claude Code's
permission denials are, so the console no longer says "memory calls 1" about
a call that never ran.

All of it is read off disk when `report` runs, so older runs are followed
too, and a pattern corrected later applies to every run at once. Recall is
read from Claude Code's transcript for a run that did not record it, while
Claude Code still keeps that transcript.

The agent and the scenario are both fixed, so a difference between arms is
memory's; the number of repeats is what says whether the difference is more
than one model's variance.
