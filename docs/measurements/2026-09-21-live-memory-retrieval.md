# Retrieval scored against this project's own memory

2026-09-21. 112 pages of `default/anamnesis`, 36 questions, scored over the
first five results, through the real query path with no embedder contributing.

Run with:

```
anamnesis eval --suite crates/anamnesis-evals/questions/live-memory.toml \
               --pages-from <data dir or backup> --scope default/anamnesis
```

The four built-in suites score 10 to 22 invented pages. They answer whether
the ranker works. They cannot answer whether *this* memory answers *these*
questions, which is the only number that decides whether somebody running
without a model is served.

## What it scored

| | Hit@1 | MRR | NDCG@5 | Recall |
|---|---|---|---|---|
| all 36 | 0.611 | 0.714 | 0.745 | 0.833 |

| category | n | Hit@1 | MRR | NDCG | Recall |
|---|---|---|---|---|---|
| keyword | 13 | 0.846 | 0.923 | 0.943 | 1.000 |
| natural | 9 | 0.556 | 0.667 | 0.696 | 0.778 |
| paraphrase | 5 | **0.000** | 0.100 | 0.126 | **0.200** |
| symptom | 8 | 0.625 | 0.775 | 0.831 | 1.000 |
| temporal | 1 | 1.000 | 1.000 | 1.000 | 1.000 |

A name, a flag, a version string or a config key is found and found first.
Everything else degrades with the distance between the asker's words and the
page's.

## The finding the fixture suites cannot produce

Five of the six misses are a question in one language against a page in the
other:

| question | page | |
|---|---|---|
| `what code word did we agree on to prove memory carried across agents` | Turkish | EN → TR |
| `başarısız araç çağrısı neden kayda geçmiyor` | English | TR → EN |
| `prova üç kez geçti ama gerçek sürüm patladı` | English | TR → EN |
| `karşılaştırdığımız projenin adını PR'a yazabilir miyiz` | Turkish, other words | TR → TR |
| `hafizanin aktarildigini kanitlamak icin sectigimiz gizli kelime neydi` | Turkish, other words | TR → TR |
| `is the local model good enough to write session pages` | English, other words | EN → EN |

This memory is written in two languages, sometimes inside one page, and no
retrieval stream crosses between them. The built-in corpora are English-only,
so this could not have appeared there at any score.

The sixth miss is the one that is not about language: an English question
against an English page that says the same thing in other words. That is the
lexical ceiling, already measured by hand on 2026-09-21 and recorded in
`measurements/recall-is-lexical-paraphrase-probe-2026-09-21.md` in the wiki.

One more case answered at rank five rather than near the top — a Turkish
symptom against an English page — which is the same gap without the total
failure.

## What it does not measure

Ranking, not the prompt-time gate. A page at rank one still has to clear
`min_similarity` or `min_coverage` before a prompt ever sees it, and the hand
probe of the same day showed a correct top-ranked hit being discarded by that
threshold. `eval --gate` measures the other half: 1 false alarm in 171.

No embedder contributed to these numbers. With vectors on, the paraphrase row
is the row expected to move, and this file is the before.
