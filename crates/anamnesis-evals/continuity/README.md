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
yet generate the brief, import histories into Anamnesis, install hooks or replay
delivery. The optional client adapter below executes one explicitly released,
verified trial; it does not establish those treatment prerequisites. Installing a second memory plugin on the host is
not a valid way to construct a comparison arm.

Build 30 distinct, frozen developer-task histories before evaluating usefulness,
with 15 Claude-to-Codex and 15 Codex-to-Claude cases. Cover accepted and rejected
decisions, superseded rules, failed attempts, facts in the middle of long
sessions and independent parallel workstreams. Use six *different* development
cases for tuning. Do not tune retrieval or summarization against evaluation
answers. `--corpus` imports private, frozen tasks and task-specific artifact
checks. No captured developer-task corpus has been supplied or run yet.

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

External corpora use their frozen artifact checks instead of `policy.json`.

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
Fresh preparation does not verify API access. A separate local access check on
2026-10-03 succeeded for the exact request/response model `gemini-3.6-flash`:
5 input and 130 output tokens including thinking, $0.000492 at the pinned
standard rates, charged to the shared ledger. Each newly prepared manifest
still records access as unverified; retain and check the separate access trace
for the actual credential/model used. No corpus summary or reader task was
generated by this access probe.

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

`gemini.py` implements a text-only preparation adapter, using a credential from
`GEMINI_API_KEY` solely in the HTTP header. It first reads the pinned model's
metadata, then reserves its full advertised input/output ceilings before
generation. Reserving the full output ceiling also protects against unexpected
thinking beyond the requested output setting. It records actual total output
including thinking, retains uncertain holds, refuses redirects/model-version
changes and never retries automatically. A blocked or truncated response still
incurs its measured cost and is marked incomplete. The reservation may require
more remaining budget than the expected bill. Model metadata reads generate no
tokens; their returned limits are retained in the request trace.

```powershell
python crates/anamnesis-evals/continuity/gemini.py --root C:/private/run `
  --prompt C:/private/preparation-prompt.txt --out C:/private/durum.md `
  --purpose brief --max-output 4096
```

This is an adapter accounting boundary, not an account-wide cloud spending
limit. API access and the configured model's exact response identity remain
unverified until a budgeted access check succeeds. Sources: Google's
[REST reference](https://ai.google.dev/api/generate-content) and
[thinking token documentation](https://ai.google.dev/gemini-api/docs/generate-content/thinking).
Unmanaged live-server calls bypass the ledger;
do not route this experiment through them. The $50 cap does not guarantee
completion of 450 tasks. Preserve incomplete trials when limits are reached.

## Private corpus contract

Pass `prepare --corpus C:/private/corpus.json`. The top-level JSON fields are
`format: 1`, `kind: "captured-developer-tasks"`, and `scenarios`. Include six
development and thirty evaluation cases, balanced 3/3 and 15/15 by direction.
Each scenario needs `id`, `split`, `category`, `direction`, `writer`, `reader`,
`workstream`, timezone-qualified `task_at`, its original `archive`, a
task-specific `prompt`, and `seed` mapping relative repository filenames to
their initial UTF-8 text. Archive events retain `id`, `at`, `agent`, `speaker`,
`text`, `source_session` and `source_event`. Omit all events at or after the
task cutoff; the importer refuses leakage rather than silently truncating it.

`checks` is a nonempty list of task acceptance checks. `decision_check` and
`stale_decision_check` separately identify use of the current and obsolete
decision. Supported checks are `{"kind":"json-equals","file":"result.json",
"keys":["backend"],"value":"sqlite"}`, `{"kind":"contains","file":"next.txt",
"text":"verify restore"}`, and `{"kind":"absent","file":"obsolete.txt"}`.
Checks stay outside the reader repository; no arbitrary shell checker is run.
Produced artifact hashes and individual check outcomes are retained. These
static artifact checks do not establish functional correctness for arbitrary
coding tasks; such tasks still require independently frozen functional checks.

Duplicate IDs, identical task prompt/snapshot pairs, reused identical source
sets, missing provenance, direction mismatches and path escapes are rejected.
This validates structure, not authenticity: review actual capture provenance,
independent task selection, redaction and acceptance check adequacy separately.
Import does not certify that someone has not fabricated a history. External
corpora remain behind the acceptance-review gate even after all trials finish.
Keep corpus files, native memory snapshots and traces outside the Git checkout.

## Single-trial client adapter

`clients.py --root ... --trial ... --client codex` prints a plan without starting
a client. `--execute --readiness C:/private/readiness.json` executes one trial.
The readiness record must name the frozen `reader` and `model`, set
`access_verified`, `profile_isolation_verified`, `permissions_verified` and
`treatment_verified` to true, and cite fresh-process `evidence`. These are
external acceptance claims, not proof generated by the adapter. Authenticate
the independent profile beforehand; no host credential/config copying occurs.
Profile environment changes apply only to children. Host API variables are
excluded; local native-memory features are disabled equally, and each reader
receives the common frozen `local-memory/` snapshot. Codex bypasses the shared
daemon; controls skip user config, while Anamnesis loads its owned isolated
profile to activate prepared `CODEX_HOME/hooks.json`. Ignoring that config
layer also skips those hooks. Review and activate hook trust in that profile;
do not claim a working treatment merely because MCP initializes. Control
profiles containing hooks are refused. Claude uses restricted mode, explicit tools and strict
MCP selection. See [Codex non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode)
and [configuration](https://learn.chatgpt.com/docs/config-file/config-reference).

Brief and Anamnesis arms require a prepared `durum.md`; Anamnesis also requires
`--anamnesis` pointing to the intended binary, its independent populated store
and verified capture/delivery hooks. Claude receives only its explicitly
prepared local hook settings. MCP initialization, hook trust/delivery, native
memory isolation and filesystem read confinement still require real client
checks. A separate profile and write sandbox alone do not prove that a reader
cannot open held-out checks or other run directories. No evaluation usefulness
claim is permitted before that confinement is established.

Claude execution additionally requires `--claude-ready` and refuses starts
before 2026-10-03 19:00 Istanbul. Time alone does not establish quota availability.
Wall time and distinct streamed actions are monitored; a limit stops the owned
process tree and records budget exhaustion. This observes emitted actions,
not a provider-side hard token/tool limit; parallel dispatch can already be in
flight when the transcript reaches the limit. Retain overruns in the result.
Client stdout/stderr, launch metadata and available tokens/costs stay local.
`client.measurements.json` deliberately leaves preparation, reminders and
capture/summary/delivery/source-open stages unknown. Add their measured values
and trace references before `record`; never infer successful delivery merely
from registering MCP or seeing a tool name. Check client quota failures in the
retained transcript and record them as quota outcomes, not silent exclusions.
