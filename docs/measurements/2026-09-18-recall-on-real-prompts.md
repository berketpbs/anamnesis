# Recall's gate on the prompts this project actually receives

1.2.1 answers a prompt with the pages the project already has on it, and only
when a page is close enough: `[recall] min_similarity = 0.55`. That number came
from sixteen prompts written for the purpose — eight the project had something
to say about and eight it did not — and on them it split cleanly: the second
kind never passed 0.542 and the first never fell below 0.573.

In use it did not look like that. Within one working session on 2026-09-18 the
two-word `devam et`-style replies an operator sends between tasks were answered
with three pages each, none of them about anything the reply said. This is the
measurement of how often that happens and what can be done about it.

## Method

- **Prompts:** every distinct prompt this machine's sessions on this project
  had recorded, read from the raw spool — 201 of them, after dropping harness
  notifications (`<task-notification>`, see #272). Mostly Turkish, some
  English, from twenty days of work.
- **Pages:** the 93 latest, non-superseded pages of the live index, each with a
  `nomic-embed-text` vector, read from a copy of the database.
- **Scoring:** exactly what `Store::pages_like` does — cosine against each
  page's best vector, the prompt embedded with no prefix and cut to 1000
  characters, by the same Ollama model the server uses.
- **Nothing was asked of the server.** A query through it records an access,
  and the sweep reads those to decide what to keep.

## How often the gate lets a block through

| Words in the prompt | Prompts | Best page ≥ 0.55 | Best page, median |
|---|---|---|---|
| 1–2 | 22 | 45% | 0.548 |
| 3–4 | 33 | 85% | 0.621 |
| 5–7 | 39 | 100% | 0.672 |
| 8–12 | 33 | 100% | 0.688 |
| 13–25 | 41 | 100% | 0.737 |
| 26 or more | 33 | 100% | 0.743 |

184 of 201 prompts, 92%, would have been handed a block. The sixteen-prompt
split held for what it tested — prompts about nothing the project knows — and
real traffic has almost none of those. What it has instead is prompts about the
project that the pages are near without being about, and replies with no
subject at all. The one-word `yaptım` had 57 pages above the gate.

## Whether the best page was the right one

Forty prompts of five words or more, drawn with `random.seed(20260918)`, were
each labelled by whether the best page was about the prompt's subject. The
labels are the judgement of the agent doing the measuring, over one operator's
prompts; they are a direction, not a benchmark.

Twenty were, twenty were not. Kept at each gate:

| Gate | Right page kept | Wrong page kept | Share of blocks that are right |
|---|---|---|---|
| 0.55, as shipped | 20/20 | 20/20 | 50% |
| 0.65 | 19/20 | 15/20 | 56% |
| 0.70 | 14/20 | 6/20 | 70% |
| 0.75 | 11/20 | 2/20 | 85% |

Most wrong pages went to prompts asking where the work had got to or what to do
next — which is what the handoff is for — and they were the same few pages
whatever the prompt said: a gotcha about concurrent git writes was the best
page for five of the twenty, a session that ended before it began for six
more.

## Why the gate is not simply raised

Because the long-run eval's probes would lose the page they need. In the run of
2026-09-18 (`20260918T163121Z`, 1.2.1, pages by qwen2.5:7b), against that
run's final 22 pages, each probe prompt scored its planting page at:

| Probe | Planting page | Score |
|---|---|---|
| S08 logging | S02 | 0.636 |
| S09 EUR rate | S03 | 0.677 |
| S10 repeated imports | S04 | 0.663 |
| S11 DEPLOY.md | S05 | 0.491 |
| S12 mixed currencies | S01 | 0.730 |

The pages recall exists to deliver sit at 0.64–0.73. The wrong pages on the
live corpus sit at 0.63–0.81. No single cosine separates them, and a gate at
0.70 would have kept one plant of five.

## Two other signals, tried and set aside

- **How far the best page stands above the median page.** A reply with no
  subject should be near everything equally. It is not, measurably: the gap is
  0.072 / 0.089 / 0.121 (10th percentile / median / 90th) for one- and
  two-word prompts, and 0.077–0.091 / 0.113–0.128 / 0.153–0.202 for prompts
  of five words or more. The ranges overlap throughout.
- **Closeness to a fixed set of generic replies.** Fourteen written for the
  purpose — generic ones, `ok`, `continue`, `next`, `sounds good`, `tamam`,
  `evet`, `peki` and the like, though written after the prompts had been
  read — with a prompt skipped
  when it is nearer to one of them than to its best page. Plainly compared it
  still let 29 of 49 subjectless short prompts through; with a margin of 0.10
  it let 13 through and dropped 25 of 152 prompts that had a subject.

## What lands

`[recall] min_words`, default 3, checked before anything is embedded. Words
are counted by Unicode's boundaries, so a sentence in a script written without
spaces is not one word.

Every one- and two-word prompt in this set was a reply with no subject — twenty-
two of twenty-two — and ten of them were answered with a block. Three- and
four-word prompts are a mix: of 33, six named something the pages could know
about (a model to switch to, a tag to cut, where a key goes). A gate at five
words would have removed every subjectless reply and those six with them, so
it is left at three and the rest to the similarity gate. None of the long-run
eval's prompts is under five words.

## What is still open

A gate that knows whether a prompt has a subject, rather than one that knows
how near its words are to the corpus. On this corpus the cosine band that holds
the right pages also holds the wrong ones, and the next thing to measure is
whether rank-based signals — how the best page's lead compares with what the
same page scores for other prompts — or the lexical streams can tell them
apart. The eval now records what recall offered each session (#273), which is
what any such change would be measured against.

## Why the prompts are not here

The repository is public, and these are an operator's words to an agent about
their own work. The counts above are what the claims rest on; the prompts stay
on the machine whose sessions recorded them.
