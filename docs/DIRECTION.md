# Direction

Assessment: 2026-10-01. The current execution plan and release gates are in
[Readiness and next work](READINESS.md). This page explains why that order
matters; dated experiments remain in [measurements](measurements/).

## The product promise

Anamnesis should help an agent use a decision or result from an earlier session
without making a developer manage transcripts. That promise needs four things
to work together: the event must be captured, the useful fact must survive
consolidation, the next agent must see it at the right moment, and the agent
must apply it correctly. A green hook configuration or a good retrieval score
establishes only one part of that chain.

The raw transcript and versioned wiki are durable sources. The SQLite index is
rebuildable. Hooks remain nonblocking, redact sensitive input, and queue events
when the server cannot accept them. A model improves summaries but is optional.
These properties make it safe to investigate usefulness without putting a
developer's work at risk.

## What the evidence says

- The checked-in retrieval fixtures catch ranking regressions, but they are
  small. The long-page suite had hit@1 of 0.500 in the 2026-09-20 audit. A
  correct answer in a fixture does not establish a better real task outcome.
- Prompt-time recall kept quiet on 170 of 171 unrelated cross-corpus questions
  in the 2026-10-01 check. It produced a block for 22 of 57 questions asked of
  their own corpus. Coverage and the quality of what an agent does with a
  shown page still need field measurement.
- The first completed paired agent run tied at two successful tasks out of
  five in each arm. The [recall measurement](measurements/2026-09-18-recall-on-real-prompts.md)
  separates pages kept, shown, opened, and used.
- On 2026-10-01 two Codex session-start events reached raw storage while
  Codex also displayed a SessionStart failure. The notification and delivered
  event must be traced together before assigning a cause. A real
  Claude Code to Codex to Claude Code handoff remains an acceptance test.

## Order of work

1. **Make daily use trustworthy.** Resolve the Codex notification, prove
   current events from both clients, and complete a real two-way handoff.
   Compare each client event with raw storage and queue state; do not infer
   one client's health from the aggregate capture timestamp.
2. **Measure useful memory.** Freeze user-written questions before paired
   runs. Report task success, correct fact use, the capture-to-use funnel,
   latency, cost, and failures. Diagnose which stage failed before changing
   retrieval or consolidation. Never tune on held-out answers.
3. **Protect the boundaries.** Exercise malformed hook payloads, wiki input,
   concurrent sessions, pending replay, restore, and index rebuild. Use those
   results to choose specific fixes.
4. **Expand when the evidence calls for it.** Shared-server load, additional
   retrieval structures, and large mechanical refactors follow a demonstrated
   bottleneck. They are not prerequisites for supervised single-developer use.

Release packages and documentation must describe the same capability. A
release candidate is exercised on the platforms it claims to support, then
used in daily work before the final tag. Wider promotion waits for a published
account of real-task results, including neutral or negative results.
