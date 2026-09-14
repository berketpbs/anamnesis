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
