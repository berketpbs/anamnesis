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
