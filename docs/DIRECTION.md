# Direction

What this project is betting on, what a comparable project is betting on
instead, and the order the next work should happen in.

This is a discussion document. It changes no behaviour and no default. It exists
because `docs/USE_CASES.md`'s feature roadmap now has exactly one unchecked box
left — the old plan is finished, and the question of what replaces it deserves
an argument rather than a list.

Everything measured here was measured on 2026-09-07 against this machine's live
memory and this repository at `2a50487`. Where a number is quoted from another
project it is attributed and dated, because a number without a corpus behind it
is a decoration.

---

## The occasion

Two pull requests from [akitaonrails/ai-memory][ai-memory] were put to this
project as candidates to follow:

- **#672** — opt-in session-recall query routing, plus a fifth retrieval stream
  over an "L0 abstract" embedding. Measured on a two-year production wiki with
  138 categorised queries: hit@1 0.609 → 0.746, NDCG@10 0.782 → 0.879, with
  paired bootstrap confidence intervals above zero on every metric.
- **#667** — a bug fix: their multi-page consolidation asked the model for typed
  relations (`causes`, `fixes`, `contradicts`) in the prompt, the struct had no
  field to hold them, and the edges were silently dropped before the wiki write.

The feature-by-feature answer is short — we have neither — and it is the least
useful thing that can be said about them. The useful answer is that the two
projects are making different bets, and most of ai-memory's recent work is an
investment in a bet anamnesis has not made.

[ai-memory]: https://github.com/akitaonrails/ai-memory

## Two bets

**ai-memory is betting on retrieval fidelity at scale.** Read what they build:
bitemporal pages with `valid_from`/`valid_to` and `as_of` queries against
version-filtered FTS (#666), link windows since V56, typed edges, multi-page
consolidation, a fifth RRF stream and a query-intent router (#672). Their schema
is at **V61**. Their measurements run over a two-year production wiki with 138
categorised queries and paired bootstrap resampling. The hard problem they are
solving is: *given a great deal of memory, some of it since revised, return the
right thing as of the right time.*

**anamnesis has been betting on trustworthiness.** Read what we have shipped:
four days of silent capture loss, and the work to make that loss visible from
the outside (#57, #58); a checkout that decided which databases a binary could
open (#135); redaction that names what it redacted; hooks that fail without
taking an editing session with them; a server that has an owner; and a model
that was configured but had not answered (#145). The sentence this repository
keeps rediscovering is not *the system broke* — it is **the fault was not the
failure, it was the invisibility of the failure**. There are 896 passing tests
across 92 source files, and a disproportionate number of them exist to make a
silent wrongness loud.

Neither bet is wrong. They are answers to different amounts of memory. ai-memory
has a corpus and is making it searchable. We have a capture pipeline and — see
below — not much of a corpus.

The conclusion this document argues for is therefore not "catch up on
migrations". It is: **copy their method, not their features, and spend the next
stretch on the thing our own bet says is broken.**

## What we verified

### The corpus

The live wiki this project keeps about itself holds **26 pages**: 14 session
summaries (54%), 7 gotchas, 1 decision, 4 bootstrap. By tier, 15 episodic,
6 procedural, 5 semantic.

### The instrument cannot produce their numbers

`anamnesis eval` computes **mean reciprocal rank and recall@limit, and nothing
else**. There is no hit@1, no NDCG, no precision@k anywhere in `crates/` or
`docs/`. It runs **25 questions over 32 fixture pages** — `suites/retrieval.toml`
(10 cases) and `suites/crowded.toml` (15) — and the shipped tuning already scores
**1.000 / 1.000** and 0.967 / 1.000. There is no bootstrap, no confidence
interval, no paired significance test, no per-query win/loss count, and no
persisted run history.

The code says so itself, in `crates/anamnesis-evals/src/sweep.rs`: *"Ten or
fifteen questions are far too few to crown a winner."*

Two consequences follow, and both matter more than they look:

1. **A score of 1.000 is not good news.** It is a saturated instrument. It cannot
   move up, so it cannot report an improvement; and with 25 questions it can
   barely report a regression. Quoting it as evidence that retrieval is in good
   shape is quoting the ceiling of a ruler as the height of a room.

2. **We cannot evaluate against real memory at all.** `anamnesis backup` and
   `anamnesis restore` exist and are careful (`crates/anamnesis-cli/src/archive.rs`),
   and **nothing in `anamnesis-evals` references either of them**. The eval
   corpus is rebuilt from checked-in TOML into a temporary directory with a
   pinned clock. That is excellent for reproducibility and useless for measuring
   the wiki we actually have — and running against a restored snapshot is exactly
   how #672 was measured.

### Three findings that change the ordering

**1. The links stream is starved by construction.**
The consolidation schema requested from the model is exactly
`{title, body, handoff, entities}`, and the string `[[` appears **zero times** in
`crates/anamnesis-consolidate/src/llm.rs`. The model is never told that wiki
links exist. So the 54% of the corpus it writes arrives with **no outgoing edges
at all**, and the graph is populated only by hand-written pages, MCP writes, and
four hardcoded `bootstrap/` cross-links.

The links stream is weighted 0.25, and its own comment in
`crates/anamnesis-core/src/retrieval.rs` records that it "answered no question on
its own in either ablation". That has been read as a verdict on link-based
retrieval. It is at least as likely a verdict on an empty graph — and the two are
indistinguishable from the outside. **Adding a fifth stream while the third one
is starved is the wrong order.**

*Overtaken, 2026-09-11.* #149 put the link form in the prompt and the schema
description. The live wiki now holds **24 edges across 16 pages, four of them
session pages the model wrote**, against the four hardcoded `bootstrap/` links
this paragraph counted. The starvation is over and the ordering argument with
it. What survives is the second half of the finding, and it is now sharper: the
0.25 weight was measured on fixture corpora whose graphs were written by hand,
and has never once been scored against a graph consolidation produced.

**2. #672's router would be uncompensated here.**
ai-memory charges session pages a bounded authority penalty (−0.15 by kind,
−0.08 by tier) precisely because session pages are rarely the answer to a fact
query; their router recognises a session-recall phrasing and hands that penalty
back. **We charge no such penalty.** `sessions/` is simply outside
`AUTHORITY_NAMESPACES` (`crates/anamnesis-core/src/page.rs`), so a session page
takes multiplier 1.0 while a `decisions/` page takes ≈1.107 and a pinned,
canonical one ≈1.237. Nothing is being withheld from session pages that a router
could restore.

Ported as-is, the router would be a **new** boost rather than a refund — and it
would arrive with the adversarial cost the author honestly published: on 24
fact queries artificially prefixed with "上次/之前", hit@1 falls 0.542 → 0.417.
We would import the cost and none of the offset.

**3. #667 is a field report from a road we have already chosen.**
`docs/ARCHITECTURE.md` § Future work already names *"consolidation writes one
page per session"* as **the largest single gap**: a session containing a
decision, a gotcha and a procedure leaves all three inside one `sessions/` page,
so retrieval can only ever return the whole session, and `notes/` is empty in
every memory this has run against.

ai-memory built that. #667 is what broke when they did — the prompt asked for
typed edges the type could not carry, and the write path dropped them without
saying so. That is worth considerably more to us as a **warning about the shape
of the failure** than as a patch to port. It is the same failure class this
project keeps writing tests against: a request that succeeds while silently
losing what was requested.

## The roadmap

### Phase 0 — finish what is in flight

Merge #145, and swap the installed binary when convenient — rename first, then
copy, and leave the restart to the scheduled task that owns the server.

### Phase 1 — make the instrument able to fail

Small, and everything after it depends on it.

- ~~Add **hit@1** and **NDCG@k** beside the existing MRR and recall in
  `crates/anamnesis-evals/src/score.rs`. They are pure functions over the same
  ranked list, and having them makes our numbers comparable to anyone else's
  rather than only to our own past.~~ **Landed.** Both are gated: `[thresholds]`
  takes `min_hit1` and `min_ndcg`, and the suite that was reading 1.000 / 1.000
  now reads hit@1 0.933 on `crowded`, which is a number with somewhere to go.
  The NDCG is over the suite's own scored window and prints as `NDCG@5` for that
  reason — it is single-relevance, because a case's `relevant` list is a set of
  acceptable answers rather than a set of pages that all must appear.
- Let `eval` run against a **restored snapshot**. `archive.rs` already does the
  difficult part; nothing connects it to the evals crate.
- Grow the question set and **freeze it before evaluating**, with category
  labels — fact-keyword, fact-NL, session-recall, temporal — plus a small
  deliberately adversarial set. *Category labels have landed*, on all
  twenty-five existing cases, and the first table they printed found that the
  whole of `crowded`'s shortfall is `paraphrase`: hit@1 0.750 there against
  1.000 in every other kind. *The adversarial set has landed too*, and it is the
  first suite here frozen before it was scored: sixteen questions over eighteen
  pages of an identity service, every one written so that some other page is the
  better literal match, and no sweep will ever be run against it. Its first run
  scored hit@1 0.938 / MRR 0.969 / NDCG@5 0.977 / recall 1.000, and its whole
  shortfall is `keyword` (0.800) — the opposite of `crowded`, where the whole
  shortfall is `paraphrase`. Two corpora disagreeing about which kind of
  question is hard is worth more than either figure on its own. Growing the set
  beyond forty-one questions is still open.
- ~~Make the fused score **show its working**.~~ **Landed** in #179.
  `memory_query` takes `explain` and reports, per hit, each stream's rank, what
  that rank contributed, the summed fusion, and the standing multiplier. Every
  weight here was settled by an argument and then by one sweep, and none of
  those arguments can be re-examined from a fused list, where a page three
  streams agreed on and a page one stream liked look identical. A stream that
  missed reports no rank rather than a rank of zero: only "never found it"
  indicts a weight.
- Measure **`rrf_k` against corpus size**, which nothing here has done. This is
  the item this phase was missing, and it hides behind a number that looks
  settled. The 2026-08-29 sweep lowered `k` from the canonical 60 to 2.0 and
  recorded it as the single biggest win — on corpora of ten and twenty-two
  pages. `k` is precisely the knob deciding how far one stream's first place
  outranks everything else: at `k = 2` rank one is worth four times rank ten,
  at `k = 60` the two are nearly level. On thirty pages "the stream that is
  sure is right" is a safe bet; on five hundred it is how a single full-text
  false positive takes the ranking. ai-memory, whose corpus is two years old,
  still ships `k = 60` and calls it canonical — not evidence that they are
  right, but evidence that our answer was measured somewhere their question
  does not live. **A measured parameter is more dangerous than an unmeasured
  one, because nobody looks at it twice**: `vectors = 1.0` is flagged in
  `retrieval.rs` as standing on an argument, and is for that reason safer than
  `rrf_k`, which is flagged as settled. The restored-snapshot item above is
  what makes this askable. Until it is asked, `rrf_k = 2.0` should be read as
  "true of a corpus this size" rather than as true.
- Add an explicit baseline-versus-variant mode, and adopt the rule: **a
  retrieval change without a paired measurement does not land.**

This is where #672's *method* is worth copying wholesale even though its feature
is not: freeze the set before you evaluate, report per category, and publish the
adversarial number next to the favourable one. Bootstrap confidence intervals
are worth adding only once the question count earns them; `sweep.rs` already
explains why 25 will not.

### Phase 2 — feed the corpus

The actual work, in two pieces of very different size.

**2a — tell the consolidation model that `[[links]]` exist.** ~~One change to
the prompt and the returned schema.~~ **Landed** in #149. The prompt lists the
pages this memory already holds, names the form, and admits only paths from that
list, on the grounds that a link to a page that does not exist points nowhere
and a page that links to everything distinguishes nothing. The constraint from
#124 held: what the model writes has to be the name the index will match.

**2b — more than one page per session.** ~~The documented largest gap.~~
**Landed** in #152. The schema carries `notes`: at most three durable pages per
session, each typed `decision`, `gotcha` or `procedure`. #156 added the half
that makes it safe — `schema_without_notes()` is what a session gets when the
full answer will not fit, so a note can never cost a session its own page.

In the four days since, eleven new sessions were accompanied by seven new
durable pages: gotchas 7 → 11, procedures 0 → 2, decisions 1 → 2. Not every
one of those is necessarily the consolidator's — hand-written pages and MCP
writes land in the same namespaces — but before #152 the model had no way to
write any of them at all.

It did not fill `notes/`, and that is the better outcome: the pages land in the
typed namespaces where authority and supersession already apply to them.

The #667 constraint still governs whatever adds **typed** relations: **if the
schema asks for relations, the type must be able to carry them, and the
frontmatter serializer must be shared with the single-page path.** Their fix was
exactly that — a defaulted field, an aligned allowed-key list, and one
serializer for both paths — and it is cheaper to adopt as a constraint than to
rediscover as a bug.

### Phase 3 — ranking signals, and only after 1 and 2

Then #672's two ideas become testable rather than fashionable.

The **abstract stream** first. It contributes nothing until pages carry an
abstract, it has no measured adversarial cost, and its rationale applies to us
*more strongly* than to them: we embed `title + "\n\n" + body` as a single
vector, and the default MiniLM embedder **silently truncates at 512 tokens**
(`crates/anamnesis-llm/src/embed.rs`). A one-line summary would embed sharply
where a long page currently embeds its first half and drops the rest without a
word.

**Session-recall routing: probably never**, for the reason in finding 2.

### Phase 4 — typed edges, if the graph is non-empty

`page_links` is untyped today: primary key `(from_page_id, to_target)`, no
`kind` column. The only typed relation in the system is `supersedes`,
single-valued, on the page row. That is a real limit — a page cannot state two
different relations to the same target — and it is not worth lifting at 26 pages
with one decision page in them. Worth re-asking at 200.

### Side track, unphased

A **provider fallback chain**: an ordered list of providers, tried on transient
failure. ai-memory's counterpart (#649) was accepted as a design and merged onto
their `release/2.1`, with implementation tracked separately.

Our own evidence is from 2026-09-07 and is unusually direct. `gemini-3.8-flash`
and `gemini-3.7-flash` answered `503 high demand` to every real consolidation
request for an afternoon, while `gemini-3.5-flash` served the same session in the
same minutes, and a local `qwen2.5:7b-instruct` under Ollama answered it in 16
seconds with no key and no quota. A fallback chain would have turned a
counted-summary afternoon into a slightly-worse-page afternoon.

The constraint that comes with it: a page written by a fallback must **say so**.
Silently substituting a weaker writer is the failure this project exists to
prevent — see `sessions.summary_source` (#145) for the shape the answer should
take.

## What we deliberately do not follow

Stated so that the reasoning does not have to be rediscovered.

- **Bitemporal `as_of` retrieval (#666).** Their answer to a corpus with two
  years of history and facts that have since been revised. We have `supersedes`
  and `is_latest`, and at this size that is enough. Revisit if the corpus grows
  and pages start contradicting each other rather than replacing each other.
- **Session-recall routing (#672).** Finding 2, plus the author's own adversarial
  number.
- **Hotness weighting.** Worth recording that *they measured it and did not ship
  it*: `sigmoid(ln(1+access_count)) × exp(−ln2·age/7d)` was monotonically
  negative across their grid, NDCG@10 0.782 → 0.713. We have the ingredients —
  `access_count` and `last_accessed_at` already feed the decay sweep — which is
  exactly why the negative result is worth writing down.
- **A recency boost under a detected "current state" intent.** −1 hit@1 on its
  own target category in their run.
- **ANN indexing.** Already deferred in `crates/anamnesis-store/src/query.rs`;
  brute-force cosine over 26 pages is not a bottleneck, and an index that is
  wrong is worse than a scan that is slow.

~~One near-term candidate that is *not* ranking and deserves naming here:
`ARCHITECTURE.md` Future work item 2 — `memory_query` returns snippets, so an
agent that finds exactly the right page still has no tool to read the rest of
it, and works from three sentences.~~ **Landed** in #178. `memory_read_page`
takes the path a hit reported and returns the body whole, along with the
frontmatter that qualifies it — status above all, since a `do-not-answer-from`
page is evidence about what was once believed rather than about what is true,
and a body handed over without that is worse than no body. Worth recording how
it was found: by reading what a comparable project ships. ai-memory has had the
tool for some time, and a feature the neighbour shipped and we named as a gap
is the cheapest evidence available that the gap is real.

## Discrepancies found while writing this

Recorded rather than fixed, so that this document stays discussion. Each is a
separate, small change.

- **`docs/ARCHITECTURE.md` claims a ranking term that does not exist.** It says
  "Tier is a bounded signal applied *after* candidates are generated". Tier is
  loaded, echoed on every hit, and **never enters the score**; the post-fusion
  multiplier reads namespace, canonical and pinned. Either the document or the
  code is wrong, and no test would notice.
- ~~**`docs/GETTING_STARTED.md` prints stale example output** in its "Score
  Retrieval" section — `MRR 0.708 (bar 0.700)`, from before the 2026-08-29 sweep.
  The suite now scores 1.000 against a bar of 1.000.~~ **Fixed** alongside the
  first Phase 1 item, since that change rewrote the same block of output.
- **`page_links.to_project_id` is in the schema and never written.** Cross-project
  links are declared and unpopulated; the `_global` scope is reached by fusing
  two searches instead.

## Checking any of this

Every claim above is meant to be verifiable without trusting the author.

    # the corpus
    anamnesis status
    find "$(anamnesis status --verbose | ...)/wiki" -name '*.md' | wc -l

    # the instrument, and its ceiling
    anamnesis eval
    anamnesis eval --streams

    # the starved graph: this prints 0
    grep -c '\[\[' crates/anamnesis-consolidate/src/llm.rs

    # what the model is actually asked for
    sed -n '/pub fn schema/,/^}/p' crates/anamnesis-consolidate/src/llm.rs

The ai-memory pull request numbers resolve at the repository linked above, and
each of their figures quoted here appears in the pull request body it is
attributed to.
