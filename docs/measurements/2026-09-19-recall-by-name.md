# Recall by name, on the prompts this project actually receives

Prompt-time recall offered a page only when an embedder said it was close to the
prompt. With no embedder it said nothing, and running no model is a setup this
system supports. `Store::pages_named_by` answers from the words a prompt names
instead. This is how it was measured, what it was changed by, and where it stops.

## Method

- **Synthetic, no labels needed.** The four built-in eval suites describe four
  unrelated systems. A question written for one of them has no answer in the
  other three, so asking it there means **every block is a false alarm**. Asked
  of its own corpus, the same question has a known answer. `anamnesis eval
  --gate` asks all 57 questions of all four corpora: 57 at home, 171 away.
- **Real, from a copy of the live index.** The 99 latest pages of this machine's
  index are 50 session summaries, 37 gotchas and procedures, and 12 decisions
  and bootstrap pages. The prompts are the same 201 distinct prompts from the
  raw spool as the
  [measurement of 2026-09-18](2026-09-18-recall-on-real-prompts.md), and the
  same forty of them labelled there. Nothing was asked of the server.
- **Real, across projects.** The long-run eval of 2026-09-18 left a 22-page
  index for a different project, a currency ledger. This project's 201 prompts
  were asked of it, and its 12 session prompts were asked of this project's
  index. None of those 213 has an answer where it was asked.

## What changed it

Each row is the version measured, with the synthetic false alarms out of 171
and the real prompts, out of 201, that got a block.

| Version | Synthetic false alarms | Real prompts with a block |
|---|---|---|
| Missing words counted only when six letters or longer, at half weight | 28 | not measured |
| English and Turkish function words dropped; every other missing word counted, at half weight | 12 | not measured |
| Missing words at full weight, the weight of a word on no page | 2 | 35 |
| Session summaries recalled only through a name | 1 | 7 |
| Generic verbs dropped (`use`, `need`); a page matched by a name not held to missing ordinary words | 1 | 8 |

Two findings did most of the work:

- **A word the project never wrote is evidence.** At first only long missing
  words counted, which let `windows bom` through: `bom` was missing but short,
  and `windows` was on one page, so the prompt got that page. Counting every
  missing word that is not a function word, at the full weight of a word on no
  page, gives the rule a plain reading: a page has to carry more of what the
  prompt names than the project is missing. In a two-word prompt where one
  word is unknown to the project, the other can never carry it alone. This
  also took a setting out, where most changes add one.
- **Session summaries echo the conversation.** With every page eligible, 22 of
  the 23 blocks on real prompts of three words or more came from session
  summaries. Most were an echo: a summary titled with a reply
  like `nerede kalmıştık` matched every later one exactly. Decisions, gotchas
  and procedures are written about a subject. A session is worth recalling for
  what it touched, and that is a file, a command or a version, which are names.
  So a session summary is now recalled only through a name it carries.

## Where it stands

**Synthetic**, `anamnesis eval --gate`:

| Corpus | Own questions: block | Block led with the answer | Others' questions: block |
|---|---|---|---|
| retrieval | 4/10 | 4/4 | 0/47 |
| crowded | 4/15 | 4/4 | 0/42 |
| adversarial | 10/16 | 9/10 | 0/41 |
| long | 4/16 | 4/4 | 1/41 |

**Real prompts**, on this machine's 99 pages:

| Words in the prompt | Prompts | Block by name | Block by cosine ≥ 0.55 (2026-09-18) |
|---|---|---|---|
| 1–2 | 22 | 1 | 10 |
| 3–4 | 33 | 1 | 28 |
| 5–7 | 39 | 1 | 39 |
| 8–12 | 33 | 0 | 33 |
| 13–25 | 41 | 3 | 41 |
| 26 or more | 33 | 2 | 33 |
| **All** | **201** | **8** | **184** |

Of the eight blocks:

- The key-shadowing gotcha went to a prompt that stores `GEMINI_API_KEY`.
- The `v1.0.0` release session went to a prompt about tagging `v1.0.0`.
- The gotcha about failed tool calls never reaching the hook went to two prompts
  that tested exactly that.
- The gotcha about who owns the server went to two more probe prompts of that
  kind. It is related, but not what they asked.
- A session went to a 140-word paste of build output. This one is wrong.
- The one-word prompt that got a block would not reach recall once
  `min_words` (#276) is in, because that filters it out first.

In the labelled forty, two prompts got a block, both from the twenty where the
cosine's best page had been right. The twenty where it had been wrong got none.

**Across projects**: 0 of 213.

## Where it stops

It cannot see a paraphrase. The long-run eval's five probes ask about what was
planted five to seven sessions earlier, in their own words, and the plants are
session summaries that share no name with them. None was found. With the cosine
gate, four of the five were above 0.55. So naming is what a setup with no model
gets, and a server with an embedder keeps the cosine gate. Requiring both was
considered and not done, because it would have cost the eval every plant.

A setup with no model writes its session summaries without a model too. They
are counted rather than written: files, commands, what failed. That is the kind
of page naming finds through its names. Decisions and gotchas reach such a
setup through `memory_write_page` or `anamnesis write-page`, and naming finds those
by their subject.

## Tried and set aside

- **The operator's own prompts as a stop list.** A word in a large share of the
  operator's past prompts would be their conversational vocabulary rather than
  a subject, in any language, with no list to maintain. Over the 204 prompts
  stored in the index it does not separate. `devam` is in 13.7% of them and
  `memory`, a subject, in 13.2%. `yaptım` is at 1.0% and `nomic` at 0.5%.
  There may be enough history for it one day. There is not now.
- **A share for missing words below full weight.** A sweep over 0.3, 0.5, 0.7
  and 1.0 against coverages from 0.4 to 0.7 found the fewest false alarms, 2 of
  171, at 1.0 with a coverage of 0.5. There the weight stops being a setting at
  all. It cost three right answers at home against the best-scoring point,
  which gave 12 false alarms for them.

## Why the prompts are not here

As on 2026-09-18: the repository is public, and these are an operator's words
to an agent. The counts are what the claims rest on.
