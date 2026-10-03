# Continuity evidence before team use

Execution agreement: 2026-10-03. The promise is: changing an agent or session
should not require the person to manage the history. A remembered decision
must change what the next agent does, with fewer reminders from the person.

## Current gates

1. **Finish O1.** The branch `perf/tool-input-stored-once` and PR #371 add
   migration/reindex/live-write parity, preserve distinct and unmatched tool
   attempts, and verify an upgrade on a backup copy. See the
   [measurement](https://github.com/berketpbs/anamnesis/blob/5590cd4/docs/measurements/2026-10-03-tool-input-stored-once.md)
   on that branch.
2. **Verify a genuine handoff.** Claude limit is exhausted; actual Claude tasks
   are paused by the person until today's 19:00 Istanbul renewal and an actual
   availability check. A different model is not a substitute for Claude.
3. **Prepare and run the comparison.** The
   [preparation harness](../crates/anamnesis-evals/continuity/README.md) supplies
   a smoke matrix, private-corpus importer, preparation accounting and optional
   single-trial adapters. Distinct held-out developer scenarios, verified client
   isolation, prepared treatments and real runs are still required. There is no usefulness
   result yet.
4. **Improve the personal product from evidence.** Source-message reading via
   `memory_read_session` / `show-session` (#376) show stored events only, with
   project scope, current redaction, pagination (20 events by default), event
   filters and explicit missing/truncated-content reporting. Portable
   `brief --out durum.md` (#377) preserves the handoff slot, retaining decisions,
   rejected approaches and source references, and only write a local file.
5. **Prepare enterprise local use.** Data controls, upgrade backup/downgrade,
   `config check`, data-free support bundles, uninstall/admin guides, CA/proxy,
   offline/pinned installations and platform validation follow documented
   negative tests and a recovery rehearsal.
6. **Pilot team use.** Personal raw/session/handoff data remain local. Only
   explicitly shared, redacted durable knowledge goes to the company's Git
   server through a review branch. Use `layer: personal|team`, preserving `tier`
   and treating existing pages as personal. Local team copies are read-only;
   index IDs and provenance include the layer. Conflicts retain both sources and
   validity. `team sync`, `share` and `memory_share` follow an end-to-end test
   with two data directories and a local bare remote. Default sync is five
   minutes; sharing always requires an explicit call. Central services, SSO and
   live teammate handoff are outside the first pilot.

Early privacy fixes do not wait for the comparison: MCP/CLI page-write
redaction (#372), framing a received handoff as historical evidence (#373),
and making implicit provider selection visible (#374) have independent branches.
Old model leftovers in `doctor` and preview-first `vectors prune` protecting
configured and live models are implemented in #378. Git packing O3b waits behind correctness
and demonstrated usefulness; size savings alone do not move it forward.

## Genuine handoff acceptance trace

Use a new project and independent workstream; record binary commits, live server
identity, client/model identifiers and capture health for each agent. Start an
actual Claude session. The person states an accepted decision, rejects a
plausible alternative and identifies the next step. Claude performs part of the
work and exits normally, allowing finalization. Retain source event IDs, the
written source page, workstream/operator scope and the pending handoff record.

Start a fresh Codex process in the same scoped work. Prompt it only to continue
the task: do not tell it to search history or repeat the three facts. Retain the
actual startup/claim/recall delivery and inspect the resulting work for use of
the accepted decision and next step without the rejected approach. Codex leaves
a new checkpoint. Start a fresh Claude process and repeat the continuation
check. The acceptance condition is correct continuation by both readers without
extra reminders, with a complete source-to-delivery-to-action chain. If any
stage fails, report the failing stage rather than assuming an embedding defect.

Separate negative tests must cover another project, missing/truncated source,
redaction, a superseded decision, parallel workstreams and a server restart.
Portable brief generation's tests verify that it does not consume the pending
handoff. Existing `supersedes` is retained; fix demonstrated gaps
rather than replacing it. Synthetic hooks or manufactured traces do not close
this gate.

## Comparison and release boundaries

The real experiment is 30 scenarios × three methods × five repeats, with 15
cases per direction. Equalize history, local memory, reader model, tool
permissions and budget; isolate profiles without changing the person's normal
settings. Brief and Anamnesis preparation costs count, including retries. Keep
six development scenarios separate, cut archives before the task timestamp and
report uncertainty by scenario. Measure task success, correct/stale decisions,
reminders, time, calls, available tokens and total available API costs. Report
captured → kept → shown → source opened → correctly used separately; infer the
last stage only from task checks. Compare against the strongest alternative.

The gate is +10 percentage points of success, or at most 2 points lower accuracy
with at least 20% time/token savings; uncertain results are not proof. Simplify
scope or collect more evidence if the gate is not met. Gemini is for summaries
and brief preparation with a shared $50 ceiling, a verified pinned model and
explicit incomplete runs. It does not replace Claude/Codex task readers.

v1.2.1 final remains a separate release gate: freeze the exact commit, create a
new candidate after any code change, pass upgrades from old published releases
and 3–5 days of field use before calling it final. Feature scheduling does not
depend on publishing final. Sales and licensing are outside this plan.
