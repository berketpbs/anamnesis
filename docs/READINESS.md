# Readiness and next work

Assessment: 2026-10-01. Code baseline: `74f1557` (v1.2.1-rc.1).
This separates implemented mechanisms from evidence that they help an actual
developer. It supersedes the old feature checklist as the execution roadmap;
[DIRECTION.md](DIRECTION.md) explains the product rationale.

Execution update, 2026-10-03: the [continuity roadmap](CONTINUITY-ROADMAP.md)
puts O1 migration/reindex parity first, then the genuine two-way handoff and a
three-arm usefulness comparison. Claude runs are paused while its account limit
is exhausted. The comparison preparation is a controlled smoke harness, not
held-out developer-task evidence. Early privacy fixes proceed independently;
the final release and team pilot keep their separate acceptance gates. The
assessment and measurements below retain their original baseline and dates.

## What is usable

Anamnesis is usable for supervised, single-developer memory: capture, durable
raw transcripts, a versioned Markdown wiki, deterministic fallback, model-based
session and durable pages, MCP query/read/write, handoffs, recovery and a browser
are implemented. Multi-page consolidation and `memory_read_page` already exist;
adding them again is not the next milestone. Auto-improve proposals are
rule-based, with approval by default, rather than autonomous model editing.

It is not yet demonstrated as unattended, lossless Claude Code ↔ Codex
continuity. The 2026-09-20 audit found an active Codex conversation without
current Codex capture, even though aggregate capture looked fresh because
Claude Code was recording. A non-writing probe and working MCP reads did not
establish live hook delivery. This is why status now reports each agent's
capture separately.

On 2026-10-01 the installed server and CLI were upgraded to the release
candidate. The Windows hook command was rewritten from that binary and its
changed hooks reviewed through `/hooks`. A fresh headless Codex session and a
fresh interactive one each recorded `session-start` and `user-prompt` in raw
storage; the interactive session also recorded a tool attempt. This closes
the fresh-session capture check after the earlier `SessionStart Failed`
notification. A real Claude Code → Codex → Claude Code handoff remains open.

## What the measurements say

`anamnesis eval` and `anamnesis eval --gate` were rerun locally on 2026-09-20,
without an embedding model, on the shipped fixtures. No thresholds or ranking
weights were changed.

| Suite | Cases | Hit@1 | MRR | Recall@5 |
| --- | ---: | ---: | ---: | ---: |
| retrieval | 10 | 1.000 | 1.000 | 1.000 |
| crowded | 15 | 0.933 | 0.967 | 1.000 |
| adversarial | 16 | 0.938 | 0.969 | 1.000 |
| long | 16 | 0.500 | 0.562 | 0.688 |

Recall by name produced one false block across 171 cross-corpus questions.
These small fixtures are regression checks, not evidence of superior everyday
task performance. In particular, long-page paraphrases remain weak.

The first completed agent run with recall tied control at **2/5 versus 2/5**.
Four probes were shown a page from their planting session; two of those still
failed. The later Codex-probe ledger report, through 2026-10-01, contains ten
attempted repeats, seven complete. Of 67 reported probe pairs, four are left
out because a planting task failed. Among the remaining 63, memory alone
passed 28, control alone passed one, both passed 14 and neither passed 20.
These runs used binaries before the release candidate and repeated one
synthetic scenario; they do not establish the candidate's effect on held-out
developer tasks. See the [long-run protocol](../crates/anamnesis-evals/longrun/README.md)
and [recall measurement](measurements/2026-09-18-recall-on-real-prompts.md).
Showing a page is distinct from preserving the needed fact and using it.

The 2026-10-01 nightly attempt stopped before its first paired task because
Codex's own local memories were enabled on the host. The harness now disables
that feature for both evaluation arms, and its binary has been updated to
`v1.2.1-rc.1`. The next complete runs must report Kept, Shown, Opened and
Passed, writer cost and Codex actions before the result supports a wider
announcement. Codex does not currently report per-run cost to this harness.

## Remaining limits

The durable wiki, rebuildable index, hook queue and model-optional fallback
support supervised single-developer use. Current tests do not establish
unattended continuity, correct use of a shown page in a real task, or safe
shared-server throughput. The product should make those claims only after
each has a live acceptance trace or a measured workload behind it.

## Execution order

1. **Close the cross-agent handoff gap.** Fresh Codex capture now passes.
   Real Claude → Codex → Claude sessions must still preserve a decision and a
   rejected approach and deliver the expected scoped page and handoff.
   Synthetic tests and probes do not replace this trace.
2. **Protect work before the session ends.** `PreCompact` now schedules a
   deterministic checkpoint after capture: it keeps the session open, leaves
   no handoff, and rewrites the one session path that finalization later
   replaces. Validate it against real client compaction and a server restart;
   decide from that evidence whether model-written checkpoints add enough over
   the deterministic page to justify another provider call.
3. **Prove useful memory.** Repeat paired long-run experiments, recording capture,
   consolidation, recall exposure and fact use separately. Diagnose failed
   paraphrases and missing fix/outcome pairs. Change one retrieval or extraction
   mechanism per measured PR; preserve held-out suites and report costs and
   false positives as well as wins.
4. **Scale only against observed limits.** Exercise concurrent sessions, pending
   replay, rebuild and shared-server isolation. Consider a bounded writer actor
   when those results justify it. Broader agents, typed relations and temporal
   queries follow concrete use cases, not a feature-count target.

Keep small, reviewable PRs and merge green changes. Preserve wiki/raw durability,
rebuildability, redaction, nonblocking hooks, model-optional operation and data
compatibility. Existing decisions requiring paired retrieval evaluation remain
in force; this plan does not authorize tuning against held-out answers.
