# Long-run eval

`anamnesis eval` asks whether memory finds the page that answers a question.
This asks what that is for: does a real agent, sessions after it was told
something, do the right thing because memory kept it?

## What runs

`scenario.toml` is twelve sessions on `fixture/`, a small Python bookkeeping
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
| plant | S01-S05 | the session finds out or is told something the repository does not say |
| distractor | S06-S07 | unrelated work, so the plant is not simply the last page |
| probe | S08-S12 | a task that needs a plant; its check is the measurement |

| Probe | Needs | Passes only if |
|---|---|---|
| S08 logging | S02: amounts never go to logs | the importer logs, and no amount appears in any record |
| S09 EUR rate | S03: `generated_rates.py` is generated | `rates.toml` and the generated file both say 1.09 |
| S10 repeated imports | S04: caching import results served stale rows | a file edited to the same size with its mtime restored imports as edited |
| S11 DEPLOY.md | S05: the staging host and command | the file names `ledger-stg-02` and `ENV=staging` |
| S12 mixed currencies | S01: tests run through `tools/check.py` | the suite passes and mixing currencies raises; also compared by turns and cost |

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
through `anamnesis key check`, and stops if any model cannot be shown to work:
under a refused key every page is counted, every probe is excluded, and the
run measures nothing for two hours. What the check said is in
`runs/<run>/model-check/key-check.txt` and `results.json`.
`--skip-model-check` starts anyway.

That check is one small question, and a model out of quota can still answer
it. So the run asks the same question of the work: when a **planting**
session's page comes back written by counting, or does not come back at all,
the run stops there and exits 5. `report` excludes every probe behind such a
page anyway, and a model that refused one session refuses the rest of the
hour — on 2026-09-17 a repeat started against a spent quota wrote its first
page by counting, and the eleven sessions after it would have cost two hours
and $1.59 to measure nothing. `--keep-going` runs the whole scenario anyway.

## The model the memory arm writes with

A repeat is twelve consolidation requests plus whatever the enrich pass asks
again, against a Google free tier of 20 per day per model that is shared with
any server already running on the same key. The first complete run spent it by
S10. `settings.local.env.example` points the arm at a local Ollama model
instead: no key, no quota, and measured here on 2026-09-17 a session's page
came back written by the model. A small local model writes thinner pages than
the hosted one, so the arm is weaker than a real install and a difference it
still shows is a floor — which is worth more than a nightly repeat that
measures nothing. Point a run at it with `--settings-env`, and leave it off to
measure the setup this machine actually runs.

One repeat takes one to two hours with Haiku 4.5 and costs a few dollars of
agent usage. It also asks the consolidation model about twelve sessions, which
on a free Gemini tier is most of a day's quota, so repeats are meant to run
one a night.

## Reading the result

A memory-arm probe counts only if the page of the session that planted its
knowledge was written by a model and the MCP server was connected. A counted
page is a page of tool counts; a probe after one measures the counter, not
memory, and `report` excludes it and says how many it excluded.

The agent and the scenario are both fixed, so a difference between arms is
memory's; the number of repeats is what says whether the difference is more
than one model's variance.
