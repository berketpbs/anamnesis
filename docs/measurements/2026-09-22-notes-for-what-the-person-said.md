# Notes for what the person said to keep

The long-run eval plants knowledge by having the person say it in passing,
during a session about something else ("for your information: we deploy with
`make deploy ENV=staging` from the ops repository"). A later probe needs that
knowledge. On 2026-09-22 the first full run with Codex probes lost two of five
probes before any agent read anything, at the writer:

- S05's deploy command and host went into the page about adding a `version`
  command, and nowhere else. Recall asked about writing `DEPLOY.md` found that
  page at 0.508, under the 0.55 gate, and showed nothing.
- S02's rule that amounts never reach a log survived only because the agent in
  that session wrote it to a page itself.

The consolidation prompt did not name this case. It said a note is for "the
thing a later session would have to be told and could not work out from the
code", and in the same breath "most sessions leave nothing behind". A rule the
person stated fits the first sentence exactly. But the second sentence is the
one a model follows when the session's own work was a `version` command.

## Method

- The run `20260921T231518Z` left its memory arm's index. A copy of it was
  given to `anamnesis reconsolidate`, once with main (bc385e9) and once with the
  changed prompt, with the model's address pointed at a local server that
  recorded each request and refused it. That gave the exact requests either
  prompt sends for S01–S07: the five planting sessions and the two
  distractors, with the same observations and the same list of existing pages.
- Each of the 14 requests was sent to `gemini-3.5-flash-lite`, the eval's
  writer, twice. 28 requests; 88,646 prompt tokens and 65,434 output tokens
  including thinking, $0.19.
- "Kept" is the scenario's own `knowledge` patterns, which `report` uses for
  its funnel, matched against the page and its notes, or against the notes
  alone.
- "Closeness" is the cosine, under `nomic-embed-text`, between the probe's
  prompt and the best note. That is the number recall's gate reads.

## Result

| Session | Notes, main | Notes, changed | In a note, main | In a note, changed | Probe's closeness to the note |
|---|---|---|---|---|---|
| S01 (fixtures variable) | 0, 0 | 0, 0 | — | — | — |
| S02 (no amounts in logs) | 0, 0 | 0, 0 | 0/2 | 0/2 | — (the rule already had its own page, which both linked) |
| S03 (rates file is generated) | 0, 0 | **1, 1** | 0/2 | **2/2** | 0.586, 0.645 |
| S04 (the cache that served stale rows) | 0, 0 | 0, **1** | 0/2 | **1/2** | 0.690 |
| S05 (deploy command and host) | 1, 1 | 1, 1 | 2/2 | 2/2 | 0.718, 0.724 |
| S06 (distractor) | 0, 0 | 0, 0 | — | — | — |
| S07 (distractor) | 0, 0 | 0, 0 | — | — | — |

Of the three sessions whose knowledge had no page yet (S03, S04, S05), the
changed prompt gave it a note of its own in 5 tries of 6: S03 and S05 in both,
S04 in one. Under main it did so in 2 of 6, both of them S05. Every such note
sits above the recall gate for the probe that needs it. The two distractors, which plant nothing, wrote no notes under
either prompt. S04's page also started keeping the fact at all, in one try of
two, where under main it kept it in neither.

## What this does not show

- Two tries per session is enough to see a direction, not to put a rate on it.
- The model is not deterministic. S05 wrote its note under main in both tries
  here, and in the run itself it wrote none. That miss is the one that cost S11
  its recall.
- This measures the writer. Whether a probe then passes also needs recall to
  show the note (see the recall ordering change of the same day) and the agent
  to act on it. The long-run eval is where all three are measured together.
