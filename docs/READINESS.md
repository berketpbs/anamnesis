# Readiness and next work

Assessment: 2026-09-20. Code baseline: `34873ea` (including #287 and #288).
This separates implemented mechanisms from evidence that they help an actual
developer. It supersedes the old feature checklist as the execution roadmap;
the measurements and decisions in [DIRECTION.md](DIRECTION.md) remain history.

## What is usable

Anamnesis is usable for supervised, single-developer memory: capture, durable
raw transcripts, a versioned Markdown wiki, deterministic fallback, model-based
session and durable pages, MCP query/read/write, handoffs, recovery and a browser
are implemented. Multi-page consolidation and `memory_read_page` already exist;
adding them again is not the next milestone. Auto-improve proposals are
rule-based, with approval by default, rather than autonomous model editing.

It is not yet demonstrated as unattended, lossless Claude Code ↔ Codex
continuity. In the local audit, MCP read existing Claude decisions successfully,
but the last captured event was about 21 hours old during an active Codex
conversation. The server held 101 sessions and 102 pages. These are a private
installation's dated observations, not a published corpus or a reproducible
benchmark. Installed binary `6db72f9` was older than the audited source.

A fresh Codex process listed the installed hooks as enabled and trusted, and a
non-writing probe succeeded. Neither proves that the already-running client
executes those hooks. The cause of that live capture gap remains unconfirmed.
New hook definitions require the client's trust flow and a fresh session.

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
failed. See the [long-run protocol](../crates/anamnesis-evals/longrun/README.md)
and [recall measurement](measurements/2026-09-18-recall-on-real-prompts.md).
Showing a page is distinct from preserving the needed fact and using it.
Repeated paired runs are needed before claiming a productivity benefit.

## Comparison with ai-memory

Source reviewed: [ai-memory at 23e427e](https://github.com/akitaonrails/ai-memory/tree/23e427e19c93d8998b31a8a52e1ee96bec30fb8f),
dated 2026-09-19. This is a code/documentation comparison, not a head-to-head
benchmark with the same corpus, model, prompts and machine.

| Area | Assessment and implication |
| --- | --- |
| Core storage | Both use a wiki and derived search infrastructure. Anamnesis's existing storage boundary is a sound basis for further work. |
| Agent coverage | ai-memory's [support matrix](https://github.com/akitaonrails/ai-memory/blob/23e427e19c93d8998b31a8a52e1ee96bec30fb8f/docs/support-matrix.md) is much broader. Some entries are MCP-only or hooks-only; they do not all promise complete lifecycle continuity. Native Windows is marked experimental there. First prove our two daily agents. |
| Retrieval/history | ai-memory has typed relations and temporal querying. Anamnesis has supersession and experimental abstract/section retrieval, but not that full graph/history surface. Add complexity only after measuring a failing user task. |
| Sustained writes | ai-memory has a [single-writer actor](https://github.com/akitaonrails/ai-memory/blob/23e427e19c93d8998b31a8a52e1ee96bec30fb8f/crates/ai-memory-store/src/writer.rs). Anamnesis has write benchmarks but no equivalent serialized writer boundary; concurrency and overload evidence should drive adoption. |
| Mid-session continuity | ai-memory checkpoints on compaction. Anamnesis records compaction events but does not yet build an equivalent automatic checkpoint; waiting for SessionEnd or stale-session recovery leaves a gap. |
| Managed sessions and teams | ai-memory has more extensive native resume and identity flows. Anamnesis's launch/continue and workstream primitives do not prove full native-session resume or automatic workstream attribution for every hook. Shared-server use remains unproven in the field. |

There is no defensible percentage for how close the project is to being
"finished," or claim that either project's published retrieval numbers beat
the other's on different data. Anamnesis has the core product; reliable everyday
continuity and measured usefulness are the next acceptance criteria.

## Execution order

1. **Close capture and handoff gaps.** #287 preserves the explicit project/shared
   scope when reading a query hit. #288 captures Codex closing reports and tests
   their delivery to the next Claude session. #289 adds authenticated launch
   preflight and propagates the selected server to installed hooks. Upgrade the
   local installation after CI; complete any required client trust review.
   Acceptance: fresh real Claude → Codex → Claude sessions produce current raw
   events, preserve a decision and a rejected approach, and deliver the expected
   scoped page/handoff. Synthetic integration tests and probes are necessary
   checks, not substitutes for this trace. Test restart and queue replay too.
2. **Protect work before the session ends.** Design a bounded, idempotent
   compaction checkpoint. Preserve the nonblocking capture budget, raw replay
   and deterministic fallback. Define when a newer checkpoint supersedes a
   prior one and how a final summary avoids duplicates before implementing it.
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
