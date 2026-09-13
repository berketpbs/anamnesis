# A validation set over this project's own memory, frozen before it was run

The candidates from `docs/DIRECTION.md` — nomic-embed-text as the embedder, and
a quarter weight on vectors under MiniLM — were both chosen by reading the two
suites whose rule is that nothing is tuned against them. This is the check that
rule asks for: questions nobody has scored, over pages nobody wrote for a suite.

## What was frozen, and when

This file was committed and pushed **before the set was run once**, with or
without vectors, under any model. The commit carrying it is the timestamp.

- **Pages:** the 56 pages of this machine's live wiki
  (`wiki/default/anamnesis`) as of 2026-09-13, exported as they are — session
  pages the consolidator wrote, gotchas, decisions, procedures and bootstrap
  pages, in English and Turkish.
- **Questions:** 24, six each of `keyword`, `natural`, `paraphrase` and
  `symptom`, the counts decided before the pages were read. Four are in Turkish,
  as questions here are. Each names the page or pages that answer it.
- **Written by:** the same agent session that produced the candidates. It had
  read the frozen suites' results, so it knew which kinds of question each
  candidate did well on. That is a bias this freeze does not remove; what it
  removes is editing the questions after seeing the scores.
- **SHA-256 of the suite file:**
  `2F0043BD4F2A51B59109DB5D754A2951FC14CBF036F6F7077C840C4A87B1B998`

## Why the suite itself is not here

The repository is public and the pages are this project's working memory:
session summaries, local paths, the operator's own notes. The file stays on the
machine that holds that memory. The hash above is what makes a result quoted
from it checkable by whoever holds the file, and the result will be reported
beside this record with the hash it was run against.

## What will be run

At the shipped tuning, and each against it, under `--embed`:

1. all-MiniLM-L6-v2 — what ships
2. all-MiniLM-L6-v2 with `vectors=0.25`
3. nomic-embed-text through Ollama
4. `vectors=0` — no embedder in play

A candidate that loses to what ships here does not ship, whatever the frozen
suites said.

## Result (run after the commit above; the file's hash matched)

hit@1 / MRR / NDCG@5 over the 24 questions, recall 1.000 in every run:

| configuration                    | hit@1 | MRR   | NDCG@5 | paraphrase hit@1 / MRR |
|----------------------------------|-------|-------|--------|------------------------|
| all-MiniLM-L6-v2, ships          | 0.833 | 0.903 | 0.928  | 0.500 / 0.722          |
| all-MiniLM-L6-v2, `vectors=0.25` | 0.833 | 0.910 | 0.933  | (2↑ 1↓ against ships)  |
| no vectors                       | 0.833 | 0.910 | 0.933  | (2↑ 1↓ against ships)  |
| nomic-embed-text                 | 0.875 | 0.931 | 0.948  | 0.667 / 0.833          |

- `keyword` and `natural` answered all twelve first under every configuration;
  `symptom` was identical under both models. Every difference is in
  `paraphrase`.
- **nomic-embed-text against MiniLM:** two questions better (`a quick health
  check of a new model can pass…` 3 → 1, `Claude Code sends nothing when a shell
  command exits with an error` 2 → 1), one worse (`the embedding only sees the
  first hundred or so words of a page` 1 → 2). Against no vectors, 2↑ 2↓ with a
  net gain.
- **A quarter weight scored exactly as no vectors did** here, including the one
  question it cost. On this set it is not a middle ground; it is no stream.
- MiniLM truncated 50 of the 56 pages; nomic read all 56 whole.

What this does and does not support. The direction agrees with the frozen
suites: nomic-embed-text is the better embedder, and it did not pay for it on
keyword or natural questions, nor with the `sqlite`-style keyword loss the
fixture suite showed. But the margin is one question in twenty-four, the set is
easy — half of it saturated under everything — and its author knew the
candidates. It rules out the candidate being a regression on real memory; it
does not measure how much better it is. The quarter weight is not confirmed.
