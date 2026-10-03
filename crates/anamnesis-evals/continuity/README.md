# Continuity comparison preparation

This prepares the matrix, independent directories, scoring, incomplete-run
reporting and Gemini accounting for the proposed three-arm comparison. It does
not start Claude, Codex or an API request. Claude runs are paused at the user's
request while the account limit is exhausted.

The generated histories are **controlled smoke fixtures**, not real agent
transcripts. The 30-case matrix uses six templates with five ID variants;
those variants are not 30 independent developer tasks. A completed smoke run
cannot satisfy the usefulness gate. Six development fixtures are kept separate.

## Local checks and preparation

Requires Python 3.11 or later, with no third-party packages:

```powershell
python -m unittest discover -s crates/anamnesis-evals/continuity -v
python crates/anamnesis-evals/continuity/comparison.py prepare --root C:/experiments/continuity-dev --budget-ledger C:/experiments/continuity-gemini-budget.sqlite --split development --claude-model claude-model-to-verify --codex-model codex-model-to-verify
python crates/anamnesis-evals/continuity/comparison.py report --root C:/experiments/continuity-dev
```

These example model strings are placeholders for smoke preparation. Before a
real run, verify access and pin the actual reader identifiers in a new run.
Preparation refuses an existing output directory. The manifest is frozen by a
SHA-256 fingerprint. Keep it and its expected answers outside the reader's
permitted workspace; the task's archive contains only pre-cutoff history.
Deleting or rewriting experiment directories is never part of preparation.

`--split evaluation` creates 30 scenarios, 15 in each direction, with three
methods and five repeats: 450 trial directories. Method order rotates. Within
a scenario, reader model, prompt, archive, optional explicit `--local-memory`
Markdown snapshot, permissions and task budget are identical in all methods.
Every trial gets its own Claude, Codex and Anamnesis data directories. The script
does not edit home-directory settings, hook registration or plugin installation.

## Treatment adapters still required

Directory isolation alone does not isolate a running client. Before any real
task, implement and verify adapters which enforce the manifest's permissions
and budget, select each trial's profiles, and disable unrelated memory plugins.
Verify with fresh processes that no host memories, other trials, expected
answers or future conversations are visible. Native client memory, when used,
must come from the same snapshot in all three methods.

The adapters must give every method the same archived history and instruction
to research it. `history` gets native history research; `brief` additionally
gets the current `durum.md`; `anamnesis` additionally gets automatic handoff,
recall and MCP. The current script creates the common inputs only. It does not
yet generate the brief, import histories into Anamnesis, install hooks, replay
delivery or launch readers. Installing a second memory plugin on the host is
not a valid way to construct a comparison arm.

Build 30 distinct, frozen developer-task histories before evaluating usefulness,
with 15 Claude-to-Codex and 15 Codex-to-Claude cases. Cover accepted and rejected
decisions, superseded rules, failed attempts, facts in the middle of long
sessions and independent parallel workstreams. Use six *different* development
cases for tuning. Do not tune retrieval or summarization against evaluation
answers. This preparation script needs an external-corpus importer and
task-specific artifact checks before those histories can replace the smoke set.

## Recording and reporting

After a task, supply a JSON evidence file to `record --root ... --trial ...
--evidence ...`. Required fields are `reader`, `model`, `status`,
`elapsed_seconds`, `preparation_seconds`, `tool_calls` and `user_reminders`.
Statuses are `complete`, `quota`, `budget`, `error` and `paused`. Record the
final outcome once; include retries and preparation in the measurements and
retain each attempt's trace. An existing result is never overwritten.

Use `tokens`, `preparation_tokens` and `agent_cost_usd` when available; null
means unknown. Stage fields `captured`, `kept`, `shown`, `source_opened` are
boolean or null. Each true claim requires a corresponding `*_evidence` path,
relative to the trial directory, to an existing trace file. Its hash is stored.
Presence of a trace file does not independently prove its semantic claim:
review the relevant event, source page, delivered block and reader action.
The final stage is checked against the produced `policy.json`, and the graded
artifact is stored in the result. A `complete` label alone never means success.

Reports retain the planned denominator, incomplete statuses and success bounds.
Time, tool calls, reminders and known costs of unsuccessful attempts remain in
the report. Missing costs and token counts stay unknown. The paired uncertainty
calculation resamples scenarios, keeping their five repeats together; it does
not treat 150 repeated tasks as independent cases. The strongest alternative
is selected by success, then total measured time. The proposed gate requires
either a lower 95% success-difference bound of at least 10 percentage points,
or a lower accuracy bound of -2 points with an upper time/token ratio bound of
0.80. Incomplete, development and controlled-fixture runs cannot establish the
usefulness gate. Full cost accounting remains a separate reporting requirement.

## Gemini accounting

The configured model snapshot is `gemini-3.6-flash`, standard text pricing
verified on 2026-10-03: $0.75 per million input tokens and $3.75 per million
output tokens including thinking. The published prices change on 2027-01-01,
so this snapshot expires on 2026-12-31. Sources: Google's
[pricing](https://ai.google.dev/gemini-api/docs/pricing) and
[model documentation](https://ai.google.dev/gemini-api/docs/models/gemini-3.6-flash).
API access is **unverified**; no request has been made by this preparation.

The CLI requires `--budget-ledger`: use the **same absolute ledger path** for
development, evaluation, retries and replacement runs, even when their run
directories differ. A new run does not receive a new $50 allowance. Preserve
this ledger alongside the archives; API costs remain attached to the overall
experiment, not only the final completed run.

`reserve` atomically reserves an upper cost bound in the shared ledger before
dispatch. `settle` records actual usage, including thinking. Access checks,
preparation, summarization, brief generation and retries share one $50 ledger.
Uncertain/failed requests retain their reservation until actual usage is known;
a retry needs a new reservation. Pricing drift, an unpriced fallback, expired
prices or insufficient remaining budget refuse reservation. An overrun records
the actual bill and stops further reservations. Concurrent reservations and
restart recovery are covered by tests.

This is an accounting boundary for forthcoming request adapters, not an
account-wide cloud spending limit. An adapter must bound input and maximum
output, reserve before calling, disable automatic model fallbacks, and settle
the complete usage response. Unmanaged live-server calls bypass the ledger;
do not route this experiment through them. The $50 cap does not guarantee
completion of 450 tasks. Preserve incomplete trials when limits are reached.
