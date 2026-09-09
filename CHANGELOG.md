# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- A session page can now say what a command returned, not only that it ran. A
  tool observation records the tail of the tool's result beside its input, so a
  transcript that said `cargo test` now says `cargo test → test result: ok. 81
  passed`. Until now the result was dropped whole at the hook, on the reasoning
  that it is the largest part of the payload and the least informative once the
  outcome flag has been read out of it — and the outcome flag, measured against
  live `PostToolUse` payloads from Claude Code, is not in there at all. So every
  page in this project's own wiki was written from a list of intentions, which
  is what a summary of them reads like. The tail rather than the head, because
  a command's verdict is printed at the end; 240 characters of it, because a
  session records hundreds of calls and all of them compete for one context
  later. `stdout` is preferred over `stderr` for the same reason a person would
  prefer it: a build tool writes `Compiling anamnesis-core` to stderr and
  whether the tests passed to stdout. The two halves of the body are clipped
  separately when the transcript is rendered, so a long command can no longer
  consume the room its own output needed, and the prompt budget goes from
  32,000 to 64,000 tokens because the results are worth having only if the
  events they belong to are still there — measured on this project's largest
  session, 686 events, which lands near 57,000
- `reconsolidate --show-prompt` prints what a model would be sent, and sends
  nothing. The transcript is squeezed to fit a token budget before anything is
  asked about it, and what falls out was invisible from either end: the page
  names what the model saw and never what it did not, so "the model ignored
  this" and "this was never in the prompt" were the same observation. They are
  now different. It needs no model configured, because the question is asked
  most often by somebody who has just been refused by a provider — requiring
  the thing under investigation to be working first is how this stayed
  unmeasured. Its first use answered a real question: one session of 526
  observations reaches the model as 76, with a three-hour hole in the middle
- Consolidation can leave durable pages, not only the session's own. The wiki
  has namespaces that outrank everything else during retrieval — `decisions/`,
  `gotchas/`, `procedures/` — and until now nothing filled them but a person
  writing a page by hand, while the one moment that has read an entire session
  and knows whether it produced a decision, hit a gotcha, or worked out a
  procedure wrote a session page and stopped. The model is now asked for those
  too, and they land beside the session page: filed by kind, at the tier that
  kind belongs to, indexed in the same call that indexes any page, and in one
  commit for the batch, because a session leaving a decision and a gotcha
  behind is a single decision about a project's memory rather than two
  unrelated ones. Almost all of the work here is in not doing it: most sessions
  teach a project nothing that outlives them, a model asked what it learned
  will answer, and a weak page in these namespaces does not get ignored — it
  outranks a real one in every later search. So the empty list is the stated
  normal answer, the ceiling is three, and the prompt spends its length on what
  a note is *not*. The path is derived from the title rather than chosen by the
  model, which cannot name a page into `_rules/` — the project's own voice —
  or collide its way onto one that exists. Nothing is written over a page this
  session did not write: a recompile replaces the notes its own earlier run
  left, because the page on disk records the session that wrote it, and
  anything else standing at that path belongs to somebody else and is left
  exactly where it is. A note that cannot be written costs one note; a note
  that fails to be written costs nothing else at all — the session's own page
  is already committed and its handoff is recorded regardless, on the same rule
  the whole model path is built on: a model may improve what a session leaves
  behind, and may never be the reason there is nothing
- A page records the session that wrote it: `session:` in the frontmatter, and
  `pages.session_id` in the index (V13). It is in the markdown and not only in
  the database on purpose — the index is rebuilt from these files, so a column
  the markdown did not carry would be a fact `reindex` quietly forgot, and
  which session wrote a page is not a statistic the wiki cannot know. Empty
  means no session wrote it — a page somebody typed, a bootstrap page, or any
  page older than the migration — and never "session unknown", because the
  thing that reads this is deciding which pages it may replace. Forgetting a
  transcript clears the field rather than deleting the pages compiled from it:
  a cascade would make forgetting a session destroy the durable knowledge that
  session produced, which is the opposite of what this is for

### Changed
- `reconsolidate` says what else it wrote. Recompiling can now create pages in
  the three namespaces that outrank everything during retrieval, and the
  command reported one path per session — so a run that also wrote eleven
  durable pages looked exactly like one that wrote none, and finding out meant
  reading `git log`. Each is now named on its own line under the session that
  produced it, counted in the summary, and the history hint no longer looks
  only in `sessions/`, where none of them are. The dry run says the one true
  thing it can: that recompiling may write durable pages and that this list
  cannot show them, because none of them exist until the model has been asked.
  A preview that quietly omits the part with the most reach is worse than one
  that admits the omission
- A session closes before a model is asked anything. Consolidation used to be
  one step — `SessionEnd` arrived, a provider was asked, and whatever came back
  became the page — which put a network call in the window between a session
  ending and any record of it existing; a server killed during it lost the page
  and left the session open. Now `finalize` writes the page, records the
  handoff and closes the session in the time a file write and a git commit
  take, and only then is a provider asked, replacing a page that already
  exists. What follows is retriable, and is retried: the `summary_source`
  column is the work queue, so an enricher takes the oldest sessions that read
  `counted` and asks again. A provider that answered `503` all afternoon now
  costs a delay rather than a permanent tally of counted pages. The handoff
  needed the care — a summary arriving after its counted note has already been
  read must not overwrite it, so superseding is conditional and shares a
  transaction with the expiry, since a claim arriving between the two is
  exactly what must not be lost

### Fixed
- A silent harness is no longer reported as a session without failures. A tool
  outcome is optional in every payload this project reads — some harnesses send
  `success`, some `is_error`, some only an `error` field, and some say nothing
  at all — and `None` has always meant "not reported" rather than "fine". One
  function away from the page that distinction was dropped: the counted page
  printed a failure line only when there were failures, so a harness that never
  states an outcome produced a page identical to a clean run, and this
  project's own dogfood sessions are exactly that harness. The page now says
  the failure count is unknown and why, the handoff says it in four words
  because it is spent out of the next session's context, and a partly
  reporting harness says how many of its calls the count actually covers. The
  model path had the same hole from the other side — the transcript annotates
  failures and nothing else, so a session with no annotations reads as a
  session that went well, and the model is told once in the header that the
  absence proves nothing. Nothing is added when outcomes did arrive: a caveat
  printed on every page is a caveat nobody reads on the one page that needed it
- A page now says how much of its session it was written from. The transcript is
  squeezed to fit before a model is asked anything, and the only party told was
  the model: the prompt carries `[… N events omitted …]` and the page carried
  nothing. So a summary written from a seventh of an afternoon read exactly like
  one written from all of it, and nobody reading the page a month later has the
  transcript open beside them to notice. Model-written pages that left something
  out end with the count — how many of the session's events they were written
  from, and how many did not fit. Pages written from the whole session say
  nothing, because a line that appears everywhere is a line nobody reads on the
  one page where it matters. `--show-prompt` counts in events rather than
  characters for the same reason: a character count cannot be compared against
  anything a reader already knows
- The prompt budget was throwing most of a session away, and saying nothing.
  The transcript is squeezed to fit `max_input_tokens` before a model is asked
  anything, and the default was 6,500 — chosen so a small local model could be
  pointed at the same code path. Measured on one real session: 526 observations
  reached the model as **76**, with a three-hour hole in the middle. The page it
  produced read perfectly well, which is the problem — nothing on it said which
  seventh of the afternoon it was written from. The two failures are not
  comparable: a budget too large is refused by the provider and logged, a budget
  too small is a plausible page. The default is now 32,000, which holds an
  ordinary session whole; a small local window is what
  `ANAMNESIS_LLM_MAX_INPUT_TOKENS` is for. Unlike the reply ceiling this is not
  free — input tokens are billed whether or not they change the answer — which
  is why it is 32,000 rather than the largest window that exists. On the session
  above the omission fell from 450 events to 145, and the model, given what it
  had been missing, wrote the first durable page this feature has produced
- A wait an API asked for is now waited. `retry-after` was read from the HTTP
  header and nowhere else, and Google sends no such header — it states the wait
  inside the body. So a 429 asking for thirteen seconds was retried after one
  and then two, which bought two more refusals out of the same twenty-a-day
  quota the wait exists to protect, and turned a rate limit into a consolidation
  that failed. Both shapes are read now, the structured `RetryInfo.retryDelay`
  before the message's own sentence, and the number is rounded up rather than
  truncated: 12.9 seconds became 12, which is one refusal early for no saving.
  The header still outranks anything the body says, and an error that states no
  wait backs off exactly as before
- A durable page could cost a session its page entirely. The summary and the
  notes are asked for in one reply and so share one output budget: a long
  session whose model was generous with notes hit the ceiling, the reply came
  back truncated, and *every* field was lost — the page fell back to a tally of
  tool calls because of a field that was never required. Retrying identically
  cannot help, since the budget has not moved, and the retry that already
  existed spent two more requests proving it. A truncated reply is now its own
  error rather than one more malformed one, because the caller can act on this
  one: it asks again without the notes, once, and the session gets the reading
  it would have had before the feature existed. The retry that already existed
  is gone from this path: a request that overflowed a ceiling overflows it
  again — measured on six attempts across two runs — so it spent refusals at
  full price with no way to succeed. That measurement also showed the notes
  were **not** what overflowed the budget; the same session truncated without
  them. The ceiling was: 2,000 by default, 8,000 on the machine this was found
  on, against an answer that turned out to be 878 tokens once there was room
  for it. A reasoning model spends the budget before the answer begins, so a
  ceiling sized to the answer leaves nothing for that, and the default is now
  16,000. Nobody is billed for headroom — only for what a model generates — so
  the low ceiling bought nothing and cost long sessions their reading
- The consolidation prompt had never mentioned that wiki links exist. `[[`
  appeared in it zero times: the schema asked for a title, a body, a handoff
  and entities, so every page a model wrote arrived with no outgoing edges at
  all. A model writes most of the pages in a live memory, which means the link
  retrieval stream has been ranking over a graph the system starved itself —
  and its own recorded verdict that links "answered no question on its own in
  either ablation" was a measurement of an empty graph rather than of link
  retrieval. Telling the model links exist is not enough on its own: it cannot
  see the wiki, so inviting links without saying what exists invites invented
  paths, and an invented path resolves to nothing. The prompt now carries the
  paths and the rule is to link only to one of them. The list is bounded, in
  whole paths — a clipped path is not a shorter path, it is a broken link — and
  what did not fit is counted and said, because a model told to link only to
  what it can see should know that what it can see is not everything
- `status` reported the model the server was configured with, and nothing
  reported whether it had written anything. Those are different claims, and
  they came apart for an afternoon: the provider answered `503` to every real
  consolidation request, the documented fall back to counted summaries worked
  exactly as designed, and every diagnostic stayed green — `/health` said `ok`,
  capture said "last event just now", and the `Summaries:` line went on naming
  a model that had not written a word. The only trace was a warning in a log
  file nobody opens. The outcome was already being computed —
  `consolidate_with_source` has always returned whether a model produced the
  page — and the server was calling `consolidate_with_llm`, which drops it. It
  is now recorded on the session (V12, `summary_source` and `summary_model`)
  and read back on the same line: `written by gemini-3.8-flash — the last 2
  were counted, the model is not answering`. Two columns rather than one,
  because counted *with* a model configured and counted with *no* model are
  different faults with different fixes. Read from the sessions rather than
  held in the server, so it survives the restart that an in-memory tally would
  not — and restarting is the first thing anyone does to a server they suspect.
  Three things it deliberately does not say: a database from before the
  migration has no provenance and reads as no evidence, never as no model; a
  server with no model configured gets no outage line at all, since counted
  pages are that configuration working; and a server too old to name its model
  gets the counts without the diagnosis. `recompile` records provenance too —
  it is what runs once a model works again, and leaving the old value would
  report an outage that is over
- `bootstrap` filed file extensions as things a page is about. The repository
  overview declared the four commonest extensions in the tree — `rs`, `toml`,
  `sql`, `md` in this one — as its entities, and an entity is a claim that the
  page is about that name, matched when the query names every token of it. One
  generic token is every token, so a question naming any `.rs` file at all
  pulled back the page describing the shape of the whole repository. That is
  the failure fixed in 1.0 for names that were too *long* to match, arriving
  from the other side. The page now declares the repository's own name, read
  from the remote it answers to; a repository with no remote declares nothing,
  because the directory it happens to sit in is a fact about one machine. The
  extensions are still in the table on the page, where full text finds them at
  the strength a mention deserves
- `bootstrap` filed the branch somebody happened to be standing on as the
  repository's own. `Identity` on `bootstrap/repository.md` read `Branch:` and
  the value was `HEAD`'s shorthand, so seeding a memory from a feature branch
  wrote `Branch: fix/...` into a page whose tier is `semantic` — one that does
  not decay, and that search hands back as the answer to what branch a project
  is on. It happened to this repository's own memory. The row now names the
  branch the remote calls default, read from `origin/HEAD`, which is a fact
  about the repository rather than about a checkout. A repository nobody cloned
  has no such ref and therefore no default to read; there the page says
  `Surveyed from:` and claims nothing more. That row also appears beside the
  default when the two differ, because the history counted further down the
  page is the history reachable from the checkout, and a reader needs to know
  which one that was
- `serve` announced itself before it had the address. The banner — the server
  is serving here, this is the wiki browser's URL, this is the model writing
  the summaries — was printed, and only then was the port asked for. When
  something already held it, the record of a start that never happened was
  fifteen lines saying it had, followed by the operating system's own sentence
  about sockets, in whatever language the machine was installed in. Found on a
  machine where a scheduled task relaunches the server every minute: every one
  of those minutes wrote the whole banner into the log and then failed. Binding
  now comes first, and the banner describes the address actually taken, which
  is also the fix for `--port 0` printing `http://127.0.0.1:0` instead of the
  port it got. A refused bind says which address, and what to do about it —
  `anamnesis status` for the one already listening, `--port` and `--bind` for
  the other two — with the operating system's text kept at the end, since the
  error number is what somebody will search for. The attempt is still logged
  before the bind, so a start that fails leaves a record of having been tried
- The handoff's `Last request` line could name a background task rather than
  something somebody asked for. A harness submits through the same door it
  gives a person, so a task completing arrives as `UserPromptSubmit` like any
  other prompt, and `render_handoff` took `prompts.last()`. In this project's
  own index 18 of 71 recorded prompts open with `<task-notification>` — one in
  four, and a session is most likely to end on one precisely when it ended by
  being reaped rather than by somebody typing. The line now names the last
  prompt a person wrote, and is left out entirely when a person wrote none: a
  handoff that says nothing about the last request beats one that invents it.
  Two things kept narrow — the match is against the start of the prompt only,
  so somebody asking *about* a notification is still asking something, and
  `HARNESS_PROMPT_OPENERS` lists only the openers this index has actually seen,
  rather than the ones a harness might one day inject. The session page keeps
  every prompt; notifications are part of what happened, they are just not what
  was asked for
- Background consolidation gave up on the model in about three seconds. Two
  retries is the right budget for a person at a terminal, or for a request
  holding a connection open, and `serve` is neither: its consolidation is
  spawned and detached, and the reap pass runs at startup, on the machine's
  least reliable few seconds. On 2026-09-05 three transport errors 28 ms after
  the server started wrote the largest session in this index — 119
  observations, the one that tagged v1.0.0 — as tool counts, and the network
  was answering a moment later. `serve` now reads its model settings with the
  unhurried budget — `LlmConfig::from_vars_unhurried`, behind a seam a test can
  call, since the one line this turns on is otherwise the one line nothing
  asserts — which starts at eight retries instead of two: against the existing
  backoff, about two and a half minutes. The only cost of waiting longer is a
  page arriving later, and the cost of giving up is permanent, because
  `reconsolidate` can rewrite the page but deliberately leaves no handoff —
  the next session has already read the counted one.
  `ANAMNESIS_LLM_MAX_RETRIES` still wins over either default, the CLI keeps
  the hurried budget because somebody is waiting on it,
  and a summary that cannot be had is still counts rather than nothing
- A model escaping the quotation marks in its own prose left the backslashes on
  the page: sixteen of them across three of this wiki's twenty session pages,
  every one in prose and not one inside code. `unescape_newlines` already
  repairs the same mistake made with line breaks, and had ruled this one out in
  a comment — quotes were "rarer, more ambiguous, and were not the failure".
  Rarer is still true; the last has stopped being. The ambiguity has a shape:
  an escaped quote in prose is a mistake, and inside code it is usually the
  point, so the repair skips fenced blocks and inline spans and touches the
  rest. What is deliberately not carried over is the newline guard, which only
  unescapes a string with none of the real thing — all sixteen sat on pages
  whose paragraphs were perfectly intact, and that guard would have repaired
  none of them

## [1.0.0] - 2026-09-06

What 1.0 commits to: the command line and the shapes on disk. A wiki, a raw
spool and an index written by this version are read by every version after it,
and the index migrates itself forward on startup — it has crossed eleven
schema versions that way already. The commands and their flags are settled;
anything that would break one becomes a new flag or a new command beside it.

Part of what that promise costs is spelled out in
`crates/anamnesis-store/src/migrations.rs`: a migration is identified by its
text with line endings normalised, so a database records something about this
repository rather than about the machine that compiled the binary reading it.
The first attempt at this release proved why. Built from one tag, the Windows
artifact hashed its migrations from a CRLF checkout and the Linux and macOS
ones from an LF checkout, and none of them would open a database created by
another. A database written before that was settled is corrected once, on the
first open, and says so in the log.

What it does not claim: that a shared server has been run by a team. Everything
that needs is here — tokens, per-operator handoffs, an audit log, a JSON API,
and a guide for running a server other machines can reach — and nobody has run
it that way yet. `USE_CASES.md` leaves that box empty on purpose.

### Added
- Google AI Studio as a provider: `ANAMNESIS_LLM_PROVIDER=google`, with the key
  read from `GEMINI_API_KEY` or `GOOGLE_API_KEY`. Gemini publishes an
  OpenAI-compatible surface, so this is the client that already existed, given
  its own address and model — no second request shape to drift from the first.
  Three decisions are worth naming. The provider has to be asked for: a Gemini
  key alone selects nothing, unlike `ANTHROPIC_API_KEY`, because a key that
  could select a provider is a key that could redirect one, and every session
  transcript would start going somewhere nobody chose. The key is presented
  once, as a bearer token: Google refuses a request carrying a second
  credential with `400 Multiple authentication credentials received`, a message
  that reads like a bad key rather than like two of them, and a socket test now
  proves the request carries no `x-goog-api-key` and no `?key=`. And efforts
  above `high` are sent as `high`: Google's vocabulary stops there, its refusal
  of `xhigh` and `max` names neither thinking nor the field, so the recovery
  below could not catch it and a session would have lost its page to a setting
  the model never needed. That clamp is a property of the backend, declared
  where it is known, rather than a guess made from an error message.

### Fixed
- `install-mcp` registered a server that reads memory with three of its four
  retrieval streams. The vector stream is the one part of retrieval that lives
  in the process asking the question rather than in the index, and it is opt-in
  — so a registration that carried no environment produced an MCP server
  answering every `memory_query` without it, over pages whose vectors were
  sitting in the index, written by a server that *was* configured for it. The
  two halves of one memory disagreed, and nothing said so.
  Measured here rather than reasoned about: an English question about a page
  written in Turkish came back without the page, and came back with it at rank
  three the moment the same query ran with the stream on — and the registered
  server's answers matched the stream-off run exactly. The registration now
  carries `ANAMNESIS_EMBED_ENABLED` (and the model, when it is not the default)
  from the environment `install-mcp` runs in. A hosted embedder's key is
  deliberately not carried: a secret in a settings file is a different decision
  from a setting in one, and the report says the queries will run without
  vectors rather than making that decision for somebody. The environment also
  counts as part of what an entry *is* now — a registration that gains the
  vector stream is replaced rather than reported as unchanged, on the JSON side
  and the TOML one
- The `AQ.` credential was redacted under the wrong name. It was filed beside
  `ya29.` as `google-oauth-token` when the rule was written, on the reasoning
  that the credential a person holds in a terminal is an access token. Half of
  that is right — `ya29.` is exactly that — and the other half has since been
  checked against the thing itself: `AQ.` is Google's newer **API key**, which
  AI Studio now issues in place of `AIza`, long-lived, with no OAuth flow
  anywhere near it. Nothing about what is removed changes; both shapes were
  matched before and are matched now, at the same floors. What changes is the
  only part a person ever sees. `[redacted:google-oauth-token]` is what stands
  in the wiki page, in the raw spool and in the log line naming the rule, and
  somebody reading it beside their AI Studio key goes looking for a flow their
  setup does not have. The two shapes now carry the two names they have:
  `ya29.` stays `google-oauth-token`, `AQ.` becomes `google-auth-key`. Older
  redactions keep the label they were written with, which is correct — those
  strings record what the rules said at the time
- A file named as an entity by a model was named in a way no search could
  match. Entity matching asks for *every* token of a name to be in the query,
  so `crates/anamnesis-core/src/sanitize.rs` wants six of them and gets none,
  while `sanitize.rs` wants the two somebody would type. The counted path has
  filed basenames since entities existed; a model asked for names "spelled as
  they appear" hands back the path it saw, so the two writers were building
  different indexes out of the same session — the failure this repository has
  already had twice, in #30 and #32. Measured, not guessed: Gemini answered
  with the full path on the first live session it summarised. The prompt now
  asks for the short name and the reply is normalised as well, because a
  prompt is guidance and an index is not the place to find out which one the
  model followed. Only paths are shortened: a branch name or a host path keeps
  every word, since there the leading segments are the name rather than a
  place to find it
- `reconsolidate --apply` could replace a summary a model wrote with one
  produced by counting. Found by running it: eight sessions went out to Google,
  every request came back `429` against a free-tier daily quota, and the
  command reported "8 page(s) rewritten" — having turned eight pages of prose
  into eight pages of tool tallies. The guard was there and could not fire.
  `consolidate_with_llm` never fails by design, because capture must always end
  with a page; it answers a refused request with the counted summary, so a
  caller asking only *whether* a digest came back cannot tell "the model wrote
  this" from "the model was unreachable". `consolidate_with_source` now says
  which, `reconsolidate` skips the counted ones and names them, and the
  distinction is a type rather than a footer somebody greps for. The pages this
  found were restored from the wiki's git history — which is the argument for
  the wiki being a git repository, and not a reason the command may overwrite
  them
- Ollama, with any model that has no thinking mode — which is most of the ones
  anyone runs locally. `reasoning_effort` goes out on every chat-completions
  request, and the client was written against Ollama 0.32, which dropped fields
  it did not recognise; its comments said as much, and said it had been checked
  rather than assumed. Ollama 0.33 reads that field, and answers `400 "<model>"
  does not support thinking`. Nothing looked broken from outside: consolidation
  treats a failed request as a model being unavailable and writes the counted
  summary instead, so a local setup came up, ran, logged one warning per
  session, and gave every session the page it would have got with no model
  configured at all. The refusal is now told apart by its message — the 400 and
  the `invalid_request_error` it arrives with cover a dozen faults that must
  still be reported — and the request is sent again without the field. Once,
  and not counted against the retry budget, because the same request would be
  refused the same way.

### Added
- `anamnesis rename`: the same memory, under a name that resolves. A project's
  identity is derived, and so is every page's identity from it — which is what
  lets two clones of one repository share one memory without configuring
  anything, and what makes renaming a repository look like losing everything:
  the next session resolves a different key, finds an empty project, and
  nothing says the old memory is still there under a name nobody types any
  more. Four things now move together: the wiki's pages as one commit git
  reads as a move, the index in one transaction with every derived identifier
  recomputed, the transcripts, and the marker file — that last one because
  without it the very next event re-derives the old identity and the rename
  reads as having quietly failed. The index migration defers its foreign keys
  to the commit, since the parent row has to move before its children can
  point at it and no ordering satisfies an immediate check; half a rename is
  worse than none, because a page whose links were left behind is a page
  retrieval ranks wrongly and nothing reports. Renaming into a project that
  already has memory is refused: merging two memories is a different operation
  with different answers, and deciding them silently inside a rename would be
  the worst way to decide them
- `anamnesis uninstall`: the counterpart to `install-hooks` and `install-mcp`.
  Trying something and being unable to remove it cleanly is a reason not to
  try it, and until now backing out meant editing four settings files by hand
  and hoping. It takes out the hooks in every harness's file, the MCP
  registration in JSON and in TOML, and the OpenCode plugin — and only those:
  a project's own `PostToolUse` hook beside ours stays, and so does a wrapper
  script somebody wrote that happens to call anamnesis, because what counts as
  ours is the same narrow predicate the installer uses. A matcher left holding
  no hooks is removed, and so is an empty `hooks` object or `mcpServers` map: a
  settings file that reads as configured when nothing is configured is the
  state at the bottom of every silent failure this project has had. Memory is
  untouched, and the command says where it is rather than deciding — stopping
  the recording is not the same as removing what was recorded
- `anamnesis purge`: this project's memory, all of it. The end of the family
  `forget` and `forget-session` start, for the memory that is wrong rather
  than incomplete — a repository re-scoped by accident, a `bootstrap` run
  against the wrong directory, a project that was never meant to be
  remembered. Nothing happens without `--apply`, and the order is chosen by
  what can be got back: the pages leave first as a git commit, so they stay in
  the wiki's history; the index second, because it is rebuildable from what is
  still there; the transcripts last, because when they are gone they are gone,
  and an interruption anywhere before that leaves the only irreplaceable part
  standing. The counts are taken before the delete rather than read from it —
  a cascade reports only the rows the statement touched, and "1 row removed"
  is not a description of losing a year of sessions. The audit line survives
  the project it describes, which is the whole reason `audit_log` has no
  foreign key: after a purge, `anamnesis audit` still answers the question
  somebody asks next
- A container image for both architectures, published on a tag to
  `ghcr.io/berketpbs/anamnesis`. CI has built the image on every pull request
  since it was written, and that image existed only on the runner that built
  it — the way to run anamnesis in a container was still to build it. Each
  architecture is now built on a runner of that architecture rather than under
  emulation: this workspace compiles a tensor library, and emulating arm64 to
  do it turns a seven-minute job into most of an hour. The two are pushed by
  digest and a manifest list is assembled afterwards, because two jobs pushing
  the same tag leave whichever finished last and the other architecture
  unreachable. A dispatched run builds both and pushes neither, the same
  rehearsal the binaries get
- Embeddings can come from an API instead of from this machine.
  `ANAMNESIS_EMBED_PROVIDER=openai` points the vector stream at any
  OpenAI-compatible `/v1/embeddings` endpoint — one shape rather than one
  vendor, the same reason there is an OpenAI-compatible completion provider
  rather than one per company. The local model stays the default and a
  misspelled provider name resolves to local rather than erroring: the setting
  exists to opt *into* sending every page and every query somewhere else, and a
  typo must not be a way to end up doing that. The endpoint is probed once
  while the embedder is built, which is how its vector length is learned and
  also how a wrong key becomes an error somebody sees at startup instead of a
  log line hours later. The key falls back to `OPENAI_API_KEY`, because a
  machine that has one for completions has one for this and asking for the same
  secret twice is how a setup ends up with two of them
- Release binaries. A tag now builds `anamnesis` for Linux x86-64, macOS on
  both architectures and Windows, checks that each one starts, packages it with
  the README, the licence and the changelog, and attaches the archives and a
  `SHA256SUMS` to a GitHub release — a release nobody can verify is a release
  nobody should run. Until now the only way to have anamnesis was to build it,
  which is a Rust toolchain and several minutes before the first useful thing
  happens. The workflow also runs on demand and stops before publishing, which
  is not a convenience: it is how the thing gets exercised without inventing a
  version, and a release workflow first exercised on release day is exercised
  on the worst possible day
- `anamnesis bench`: how many events a second this machine can record. A hook
  runs before every tool call and gives up after a second, and on a shared
  server every session's events arrive at the same index — "will that hold"
  had been answered by argument until now. What it measures is the path an
  event actually takes: parsed and redacted the way a harness's payload is,
  then recorded the way `POST /hook` records it, marker file and all. Not
  `INSERT` in a loop, because those costs are paid per event in production
  too. On the machine this was written on, in release: **1 866 events/s with
  the durable transcript, 3 708 without, p95 0.66 ms** — and redaction, which
  looked like the expensive part, runs at 274 000/s. The transcript costs
  almost exactly 2×, which is the price of the copy that survives losing the
  index. Against a hook's one-second budget there is room for about 1 500
  events, so recording is not what a session waits on. It runs against a
  temporary data directory and writes nothing to this project's memory: a
  benchmark that filled somebody's wiki with invented sessions would be a
  strange thing to ship
- `anamnesis run <harness>` and `anamnesis continue`. Everything else in this
  system is careful about recording a session; this is about the minute before
  one starts, and it exists because of the only failure anamnesis has had
  twice — an afternoon of work that reached nothing, discovered days later.
  Both times the cause was ordinary (a server that was not running, then a
  server too old to read the marker file) and both times the session had
  already happened by the time anybody looked. `run` checks first: the server
  has to answer and this harness's hooks have to point at anamnesis, or nothing
  is launched and the message carries the command that fixes it. `--anyway`
  starts regardless and says what is being given up, in those words. The server
  address travels in the environment the harness inherits rather than in the
  hook command, so a project wired to one server can be run against another
  without rewriting a settings file, and a token is passed on only when this
  process has one — an empty variable would make a harness present an empty
  credential to a server that accepts none. Everything after `--` reaches the
  harness untouched, and its exit code comes back out. `continue` is the same
  launch aimed at whichever harness this project last used
- OpenCode is wired. It was the one harness anamnesis knew about and could not
  connect, because it extends through a plugin module rather than a command in
  a settings file — there is nothing for `install-hooks` to merge, and no
  stdout for a hook to answer on. `anamnesis install-hooks --agent opencode
  --write` now writes `.opencode/plugins/anamnesis.js`: it subscribes to
  `chat.message`, `tool.execute.after` and `experimental.session.compacting`,
  synthesises the session's start from the first event carrying a session id,
  and sends the end on `dispose` — a session that ends another way is
  summarised by the reaper, as one from any harness would be. The handoff is
  pushed into the system prompt through `experimental.chat.system.transform`,
  labelled as coming from anamnesis, once per session because claiming one
  consumes it. The plugin is a checked-in file rather than string fragments
  assembled at run time, with the binary path and server address baked in
  through a JSON serialiser: a Windows path in a JavaScript string is a path
  full of escapes, which is the character class that cost this repository two
  days of capture once already. A plugin at that path that anamnesis did not
  write is never overwritten
- A JSON API under `/api/v1`: the scopes this server holds, one scope's pages,
  one page with the body the wiki holds, the same fused search an agent runs,
  the sessions recorded, and the audit log. The browser at `/ui` already
  rendered these facts for a person; nothing served them to a program, and the
  rest of the HTTP surface answers only two questions — take this event, hand
  me my handoff — neither of which is "what does memory hold". Read-only on
  purpose: every write in this system is either capture, which has its own
  endpoint, or a decision somebody made, and those stay CLI commands so that
  changing memory takes a machine somebody has rather than a token somebody
  has. Behind the same header-only guard as the rest of the API, so a
  credential a browser attaches on its own cannot read the whole of memory from
  a page on another site. Errors are JSON too — a program should not have to
  parse an HTML page to find out that a scope does not exist. Versioned from
  the first line, so a consumer written today keeps working
- An audit log, and `anamnesis audit` to read it. Capture already records what
  happened *inside* sessions; nothing recorded who reached in and changed the
  memory itself. A page rewritten by hand, a session forgotten, a handoff
  claimed, a proposal carried out, a sweep that dropped a dozen pages — each of
  those replaces or removes something a later session would otherwise have been
  told, and until now the only evidence was that memory said something
  different afterwards. Every deliberate change is now a line: who, through
  which door (`cli`, `mcp`, `http`), what, and to what. Events arriving from
  hooks are deliberately not audited — the observations table is that record,
  and duplicating it would bury the handful of lines that matter. The log
  outlives what it describes: the project reference is text rather than a
  foreign key, because a reference that cascaded would delete the record of the
  deletion, which is the one line somebody goes looking for. Writing a line
  never fails the change it describes; the failure is said out loud instead.
  This is the precondition for pointing more than one person at one server — a
  shared memory whose changes cannot be traced is one nobody can trust an
  answer from
- `anamnesis backup` and `anamnesis restore`. The wiki has carried its own git
  history since the beginning, so the compiled half of memory was always
  recoverable; the other half was not. `db/` can be rebuilt from the wiki and
  the transcripts, but `raw/` can be rebuilt from nothing — the observations a
  page was compiled from live in exactly one place, on one disk, in a directory
  that is in no repository. One archive now carries the index, the transcripts
  and the wiki including its `.git`, because the history *is* the wiki: page
  restores and checkpoints read it. `models/` and `logs/` are left out, being a
  download any machine can repeat and a record of one machine's afternoons.
  The index is copied through SQLite's own backup API rather than by copying
  the file: it runs in WAL mode, so at any instant the committed database is
  spread across `anamnesis.db` and a `-wal` file beside it, and copying the
  first without the second yields a database that opens, reports a plausible
  schema version, and is missing whatever was written most recently — a backup
  that is quietly stale, discovered on the day it is needed. The server can be
  running and writing throughout. Restoring says what it would do and writes
  nothing until `--apply`, and a data directory that already holds memory is
  left exactly as it was unless `--force` says otherwise, because restoring is
  the one operation here that running the other one cannot undo. An archive
  from a newer format is refused rather than half-read, and an entry whose name
  would put a file outside the data directory is refused by name — the tar
  crate skips such an entry and reports success, and a restore that silently
  dropped part of itself is the failure this whole command exists to prevent

### Fixed
- Recording an event no longer holds up the rest of the server. Everything the
  capture path does is blocking — SQLite is synchronous, a git commit writes
  several files, an embedding is arithmetic on a CPU — and all of it ran on a
  Tokio worker thread, of which there is one per core. That is not slow for
  the request doing the work; it is slow for every request scheduled behind it
  on the same thread, `/health` included. `/health` is exactly what `anamnesis
  status` reads to tell a server that is down from one that is up and refusing
  this machine's token, so a worker held by a git commit made a working server
  look dead — the same confusion this repository has already spent an
  afternoon on, from the other direction. Recording, probing, claiming a
  handoff, consolidating with a model, the reaper's summaries, and every page
  the wiki browser renders now run on the blocking pool. The one thing that
  stays on the runtime is the part that is genuinely a network wait: asking the
  model. A panic in that work used to drop the connection without a word, which
  a hook reads as "the server is unreachable" and queues; it is now a 500 with
  a sentence in it
- A session that runs past midnight has one transcript again, and forgetting
  it forgets all of it. The capture path built a fresh `Session` for every
  event and stamped it with *now*, and the spool derives a transcript's path
  from that field — so a session that started at 17:41 and ended at 00:07 was
  filed under two dates, with the second file's header claiming the session had
  begun hours after it did. Measured on this repository's own spool, where such
  a session is sitting. The second file was also unreachable: every command
  that looks a transcript up by name asks for the one the *stored* start date
  names, so `anamnesis forget-session` removed the file it could name and left
  the other one on disk, with somebody's prompts in it, after they had asked
  for it to be gone. The transcript is now written against the session as
  stored — one indexed lookup per event, on a path that already runs several
  statements — and `forget-session` removes every file that carries the
  session, so the ones already filed twice go too
- A marker file written for a newer anamnesis no longer stops capture on an
  older one. The rule was that unknown keys are rejected, so that a typo
  surfaces instead of silently sending memory to the wrong project — and on
  2026-09-01 this repository paid for the half of that rule nobody had thought
  about. The marker gained a `[sessions]` table hours before the installed
  server was rebuilt, and the older server answered `400` to every event of
  every session for three hours: 173 events queued, nothing recorded, and the
  only sign was a line in `anamnesis status` nobody was looking at. The events
  were fine. The configuration was fine. One of them was simply newer than the
  other. An unknown *table* is now taken as a feature this build does not have:
  it is skipped, everything else in the file still applies, and `anamnesis
  status` names it where somebody is already asking whether memory is working.
  An unknown *scalar* outside every table is still refused, because that is the
  shape a typo takes — `workspace = "x"` written above `[scope]` rather than
  inside it — and nothing inside a known table is relaxed at all, so a silently
  wrong scope remains impossible
- The same event delivered twice is recorded once. The hook gives up after a
  second, and a server that was in fact recording can take longer than that —
  so the event goes into the queue and is offered again later, and until now
  the second arrival was a second prompt in the session. Nothing downstream
  could tell the copy from the original: the summary counted it, the transcript
  kept it, and the count was simply wrong. The hook now names each delivery
  before its first attempt and reuses that name for every later one, including
  the copy the queue carries, so a repeat lands on the conflict clause the
  observations table already had and changes nothing. A sender that names
  nothing is recorded exactly as before — two identical prompts in one session
  are two events, and collapsing them would lose one to a de-duplication
  nobody asked for
- An event the server has read and refused no longer stops every event behind
  it. The queue replays in order and stops at the first failure, which is right
  for a server that is down, restarting, or holding a token somebody is about
  to fix — all of those take the event later. It is wrong for an event the
  server has already answered about: a 400 or a 413 is the same answer however
  long it waits, and one of those at the head of an ordered queue ends capture
  quietly, which is the failure the queue was written to prevent. Those now
  leave the line and are kept beside it, in `pending/refused/`, where
  `anamnesis status` counts them and a person can read them. Kept rather than
  dropped because a refusal is not proof the event was bad: this repository's
  own server spent 2026-09-01 answering 400 to every event, and the reason was
  a `[sessions]` table in the marker file that the *server* was too old to
  know about. Every one of those events was fine, and a restart would have
  taken them
- A hook payload larger than two megabytes is accepted rather than refused.
  One `Read` of a large file makes one, so this was an ordinary event, and the
  server's own limit on what it keeps of a body is 16 KB applied after parsing
  — it was refusing an event it was about to shorten anyway, and leaving the
  hook holding a payload no retry could ever deliver. The ceiling is now
  explicit and 16 MB: the body is buffered whole and scanned for secrets before
  a byte of it is kept, so it cannot be unbounded either
- An event the hook cannot deliver is kept and delivered later, rather than
  dropped. The hook's timeouts are a quarter of a second to connect and one to
  answer, because the case that matters is the server being down and a generous
  timeout there turns "memory is not running" into "the agent feels broken" —
  but the price of those budgets was the event itself. This repository lost
  capture that way twice: four days in August while a server was not running,
  and nine hours on the day the queue was written, both invisible until someone
  went looking. Payloads are redacted *before* they reach the queue, by the
  same rules the server applies, because a queue outlives the process that
  wrote it and a secret reaching it would be the most durable copy in the
  system. The next hook that finds the server up delivers what is waiting
  first, oldest first, and stops at the first event that will not go: a session
  is a sequence, and replaying its middle ahead of its beginning would leave
  the index with a session it cannot make sense of. A queue that will not drain
  is named by `anamnesis status`, which is the failure worth having — the
  alternative is dropping the event at the head to keep things moving, and
  invisible loss is what the queue exists to end. When it is full it refuses
  the newest rather than discarding the oldest, since a queue holding the end
  of every session and the start of none is worse than one that is honestly
  full. The notice a starting session is given now matches what happened: an
  event that was kept is not reported as lost
- A model that escapes its own newlines gets them back. Seen in a real reply
  from a local model: every paragraph break in the handoff was the two
  characters `\` and `n`, so the JSON was valid, the fields were non-empty
  strings, every check passed, and the next session would have been handed one
  unbroken wall of text with the escapes printed in it. Unescaped only when the
  text contains no real newline at all — a page that has line breaks *and*
  writes the sequence is explaining it, most likely in code, and rewriting that
  would corrupt the one thing it was trying to say
- The hook says, where it will be read, that capture is not working. This
  repository's own memory recorded nothing for four days: the server was not
  running, every hook failed to connect, and the only report was a line on
  stderr from a process that exits zero, which no harness surfaces. A session
  that starts while the server is unreachable now says so through stdout — the
  channel the handoff already uses — naming itself, so it cannot be read as
  memory. A handoff that fails when capture is working gets a different
  sentence, because sending someone to restart a running server is its own
  false alarm. Every path out of the hook now goes through one function:
  a failed POST used to return early and print nothing, so Gemini CLI, which
  parses stdout as one JSON object on every event, got silence exactly when
  the server was down
- `serve` writes to `logs/`, which the data-directory layout has documented as
  "rolling trace output" since the first commit with nothing ever written to
  it. The server is the one command nobody watches — it runs for days in a
  terminal that gets closed — so when it stopped there was no way to say when
  or why. One file a day, fourteen kept, written straight through rather than
  buffered, because what a buffer loses is the last few lines before a crash
- Link-neighbour expansion no longer throws away the rank of the page it
  expanded from. Neighbours were ordered by `COUNT(*)` of the edges reaching
  them, so a neighbour of the best full-text hit and a neighbour of the
  thirtieth ranked identically, and two neighbours of the thirtieth outranked
  one neighbour of the first. Each edge now counts for `1 / (k + rank)` of its
  seed — the same reciprocal-rank form fusion uses, and the same constant,
  because both are answering how much a ranking's order should matter. The
  stream's own MRR on the crowded suite rises from 0.178 to 0.256; what it is
  worth in the fusion was re-measured and left at 0.25, which is the weight
  a stream that answers no question alone should carry. Ties within the stream
  are broken by page id rather than by whatever order SQLite returned

### Changed
- The vector stream is measured, and stays where it is. `anamnesis eval
  --embed` embeds the corpus page by page and every question with the same
  model, through the same call the server makes. On its own the stream has the
  second-best recall of the four (0.700 on the retrieval suite, 0.533 on the
  crowded one) and takes full text's *unique* answers from 3 to 1 and from 8 to
  2 — it independently reaches most of what only full text reached. Its weight
  does not separate: the best rows use 0.5 and 1.0 alike, so 1.0 stays. Nothing
  else changes either: the twelve settings that beat what ships all need
  embeddings on **and** a shallower candidate pool, and a corpus of ten and
  twenty-two pages cannot say whether a shallow pool is safe on a real wiki
- The sweep's acceptance rule was wrong at the ceiling. It required a rise on
  *every* suite, which was written before either suite could reach a perfect
  score; once `retrieval` sat at 1.000 nothing could raise it, so a setting
  that took `crowded` from 0.967 to 1.000 was reported as no improvement, and
  six thousand rows produced none. It now asks that nothing falls anywhere and
  something rises
- There is one way to index a page, and every path that writes one embeds it.
  `Store::index_page` writes the row, the entities, the links and — when an
  embedder is enabled — the vector; five hand-written copies of that sequence
  became calls to it, and the two rebuilds that resolve links in a second pass
  keep their own sequence and say so. The copies had drifted twice: once when
  the live path wrote pages without their links, so the link-neighbour stream
  was blind to everything this system wrote for itself, and again in a way that
  was still true — exactly one of the seven embedded anything, so switching the
  embedder on bought a vector stream over the pages an agent had written
  through `memory_write_page` and nothing else. No session summary, no
  bootstrap page, no hand edit. `serve` now builds the embedder the MCP server
  already built, and says on startup whether it has one; a rebuild embeds too,
  or a wiki rebuilt from disk would answer differently from the same wiki
  written page by page
- The entity stream's rule that *every* token of a name must appear in the
  query is a setting rather than a fact of the SQL, and was swept like the
  rest. It stays: admitting partial matches was never better in two thousand
  comparisons across both corpora, and at the tuning that ships it costs the
  crowded suite 0.967 → 0.889. The rule was written on an argument — a
  two-word name answering one word would drown the streams it is fused with —
  and that argument now has a number behind it. Partial matches, when enabled,
  rank by how complete they are rather than beside a name said in full
- Candidate depth — how deep each stream reaches before fusion — is part of the
  tuning rather than a constant, and was swept along with everything else. The
  answer came back empty: at the tuning that ships, 10, 30 and 120 score
  identically on both corpora, so it stays at 30. Depth only mattered where the
  rest of the fusion was wrong, a shallower pool leaving fewer also-rans to
  outvote the stream that had the answer — which is worth knowing, because it
  is the shape of a fix somebody would otherwise reach for. The suites are 10
  and 22 pages, so nothing in them can tell 30 from 120
- **Retrieval is tuned against measurement rather than argument.** The RRF
  constant is 2 rather than 60, the link stream is weighted a quarter, and the
  authority multiplier is applied at a quarter power (about 1.24× rather than
  2.34×). Under the old constants a page sitting anywhere in two streams
  outscored the page one stream had ranked first — at `k = 60` a stream's whole
  thirty-deep spread is 1.47×, and the authority multiplier alone was larger
  than that — so on a corpus with enough linked, entity-bearing pages, fusion
  buried the answers full-text search had found. The shipped suites go from
  0.708 / 1.000 to **1.000 / 1.000**, and from 0.436 / 0.533 to **0.967 /
  1.000**: the second now scores above the 0.900 full text reaches on its own,
  which is the only thing that makes fusing four streams worth doing. Where the
  sweep was indifferent the design was kept — the link stream is quietened
  rather than silenced, entities stay level with full text, and canonical pages
  are still preferred

### Fixed
- Redaction sees the keys providers actually issue now. Every OpenAI key
  minted since projects existed — `sk-proj-`, `sk-svcacct-`, `sk-admin-` —
  went through capture untouched, because the rule counted alphanumerics
  straight after `sk-` and the hyphenated word in the middle ends that run.
  Demonstrated against the shipped binary rather than argued: the same prompt
  through the installed build leaves the key in the raw spool and the SQLite
  write-ahead log, and through this one leaves `[redacted:openai-key]`. Google
  (`AIza`), Stripe, npm, and Slack webhook URLs are recognised too, and so is
  this system's **own** token: a memory that records prompts and shell output
  is exactly where the key to it turns up, and storing that would hand the
  reader of one session the run of every other. The new rules name their
  prefixes rather than loosening the old one, so an ordinary hyphenated
  identifier is still left alone, and there is a test that says so The wiki watcher was spawned and its
  handle dropped, so the loop returning — or panicking — left the server
  running, still claiming at startup that hand edits are watched, with nothing
  anywhere saying they had stopped being indexed. A consolidation task had the
  same shape: a provider crate is third-party code running where a panic has
  nowhere to go, and the only trace of one would have been a session that
  ended and left no page. Both endings are now awaited and logged, and a test
  puts a panicking provider through the real hook path to show the server
  stays up, the session stays open and recoverable, and the shutdown drain is
  not left waiting on a task that already died
- The server finishes the summaries it owes before it stops. A session's page
  is written *after* the response goes out, because the hook that delivered
  the event is a subprocess of somebody's editor that gives up after a second
  — so a server killed in the seconds that follow took the page with it, and
  nothing rebuilds a summary. `serve` now stops on SIGTERM as well as Ctrl-C
  (the container case was the one taking the abrupt path, unattended), stops
  accepting, and then waits up to fifteen seconds for work already in flight.
  Not longer, because `docker stop` sends SIGKILL after ten and a longer
  promise would be one the runtime breaks; when the wait runs out the log
  names how many sessions ended without a summary and says their transcripts
  are kept. Only finite work is waited for — the scheduler and the watcher are
  loops, and tracking them would turn a shutdown that waits into one that hangs
- The Docker image's binary starts. The builder tracked whichever Debian the
  Rust image is on — trixie, glibc 2.41 — and the runtime stage was pinned to
  bookworm, glibc 2.36, so the binary that came out reported
  `version 'GLIBC_2.39' not found` the first time anything ran it. Nothing
  about that appears while building: the image is produced and only refuses to
  work. Both stages now name the same release, with the reason written where
  the next person will change one of them
- The Docker image builds at all. `.dockerignore` excluded `Cargo.lock` under
  the heading "Rust build artifacts", and both Dockerfiles copy that file, so
  every build of either one failed on the `COPY` — an image nobody could have
  built, sitting in the tree next to documentation explaining how to run it.
  The lock is not an artifact: it is the pinned resolution the repository was
  tested with, and the release build now passes `--locked` so a build that
  would resolve something else fails loudly instead of shipping quietly

### Added
- CI builds the Docker image, runs it, and checks that it answers `/health`.
  The image, `Dockerfile.dev`, and the compose profiles have been in the tree
  since the first weeks with nothing ever building them — a documented way to
  run anamnesis that nobody had checked. Building alone would not be enough,
  since an image that builds and will not start is the same broken promise, so
  the job starts the container and waits for the one route that answers
  without a token, printing the container's log either way. Two failures of
  exactly this shape are already in the history: a byte order mark ahead of
  `FROM`, and an entrypoint script the image never referenced
- A session page says who ran the session, when the server could name them.
  The index has recorded a session's operator since per-user slots existed and
  the page never mentioned it, so a shared server's wiki was an anonymous pile
  of sessions. The line is added where the page is committed rather than in
  either summariser: whose session it was is a fact about the session, not
  about how its summary was written, and the counted path and the model path
  must not be able to disagree about it. The model is never told the name —
  an operator's identity is not something to hand a provider along with their
  transcript — and a test asserts it never appears in a prompt. A server with
  no tokens has no name to write and the line is absent, rather than stamping
  "unknown" on every page of every single-person install
- Each page says what retention has in store for it: whether the decay sweep
  can reach it at all, and when it can, the tier, age and read count it is
  judged on. Until now "will this page still be here next month" could only be
  answered by running the sweep over the whole project from the machine memory
  lives on. The score is deliberately not shown — it comes from the `[decay]`
  table in a marker the server may not be able to see, and a number from
  default settings would be a claim about what `anamnesis sweep` will do, made
  by something that has not read what the sweep reads. A page that is both
  exempt and past its own `expires_at` shows the contradiction rather than
  resolving it, as the sweep does; a page with no index row says so where
  somebody is reading it
- `Store::sweep_row` reads one page's facts through the same projection
  `sweep_rows` uses, so a page's own account of itself and a sweep's account
  of it cannot disagree
- Open proposals are listed on the scope that has them, each with the
  `anamnesis improve --apply <id>` that carries it out. Auto-improve has filed
  them since it existed, and the only way to see one was to run a pass from
  the machine memory lives on. They are shown and not offered: every proposal
  changes somebody's memory — promoting a page is a retention decision,
  because the durable tiers are the ones the decay sweep cannot reach — and
  `require_approval` defaulting to true means a person running a command, not
  a button anything that can reach the port could press. A scope with nothing
  to propose says nothing at all
- A scope says when its wiki and its index have drifted apart, in both
  directions: pages in the wiki the index has never seen, and rows whose file
  is gone. The first is why "search cannot find a page I am looking at in my
  editor" had no answer anywhere — a page written while the server was down
  reaches the index only through `anamnesis reindex`, and nothing said which
  pages those were. An absent scope directory is reported as itself rather
  than as every page having been deleted: `Wiki::pages` cannot tell an empty
  scope from a missing one, and the second is a data directory pointing
  somewhere unexpected far more often than it is a wiki somebody emptied,
  which is the same distinction `reindex` refuses to delete rows over. A wiki
  and an index that agree say nothing at all
- The browser's front page says whether memory is still recording. Each scope
  now shows its sessions and how long ago it last captured an event, and the
  page above them says what this server is doing: whether a token is required,
  which model consolidates, whether embedding is on. This repository once lost
  four days to a server that was not running and nothing said so; `status`
  answers that on the machine memory lives on, and this answers it from any
  browser that can reach the port. No secret appears — the token count says
  whether a door is locked, never what opens it
- Search in the wiki browser: `?q=` on a scope, which is where the page list
  already was. It runs `query_pages_across` — the workspace's shared scope
  included, the opt-in embedder with it, at the same default limit — so what a
  person is shown is what an agent asking the same question would have been
  handed, rather than a second retrieval nothing measures. Hits say which
  scope they came from, because a policy that applies to every project and a
  note about this one are different kinds of answer and the path does not say
  which is which. Unlike opening a page, a search *does* record an access for
  what it returns: it hands somebody a page it chose, which is the act those
  counters are about, and `anamnesis search` has always recorded it
- The workspace's shared scope is derived in one place, `Wiki::global_scope`.
  Where its pages sit and what project identifier its rows carry have to
  agree between every reader, or a page written through one is invisible to
  the other; the MCP server now asks the wiki instead of rebuilding the path
- A wiki browser at `/ui`, served by `anamnesis serve`. Until now memory could
  only be read by asking it something — `search`, `show-page`, or an agent's
  MCP query — which meant a stale page, a summary the model wrote badly, and a
  page that never got indexed all looked identical from outside. Three routes:
  the scopes this server holds, the pages in one, and one page rendered.
  It is read-only, and `serve --no-ui` leaves it out: it is the only part of
  this server that can read the whole of a memory, where the API accepts events
  and delivers a single handoff. Three deliberate limits. It never records a
  page access, because the decay sweep reads exactly those counters and
  browsing an index is not the claim that retrieval found a page useful.
  Bodies come from the wiki rather than the index's copy of them, since the
  file is what a person edits and what git holds. And raw HTML in a body is
  shown as text with non-`http(s)`/`mailto` link destinations defused, because
  a page body is written by models and by capture. `[[wiki links]]` become
  links, and ones with no page behind them are marked rather than hidden —
  the same signal `improve` turns into a proposal
- The browser's credential: `/ui` also accepts the server's token as an HTTP
  Basic password, so a token-protected server is still openable. A browser
  cannot be asked to attach a bearer header to a link somebody clicked, but it
  will ask for a password. Any username is accepted — the secret is the whole
  credential. The API is unchanged and stays header-only, so a credential the
  browser attaches by itself cannot authorise `POST /hook`
- `anamnesis handoff --discard` throws away the note waiting for the next
  session. A handoff written from a bad model reply had exactly one way out:
  let a session claim it, which puts it in that session's context — the thing
  being avoided. It prints what it dropped, and the row is kept and marked
  expired, the same state a newer handoff already puts an older one in, because
  a record saying a note was written and never delivered is more honest than no
  record. Slots are separate here as everywhere: discarding one operator's note
  leaves everyone else's pending
- `anamnesis forget <path>...` removes named pages from the wiki and the index.
  `sweep` forgets what decayed; nothing forgot what was *wrong*. A page written
  from a bad model reply, a note that turned out to be untrue, a duplicate —
  the only ways out were to wait for a decay that never comes for a pinned or
  durable page, or to delete the file by hand and hope the watcher was running
  to notice. Index row first and file second, the recoverable order; every path
  resolved before anything is removed, so one typo does not leave a half-done
  job; and the commit it prints still holds the content, because the wiki is a
  git repository
- An OpenAI-compatible provider, which is also the Ollama one: `openai` and
  `ollama` are one client over one wire format, differing in their default
  address and in whether a credential is expected. A model running on this
  machine has none to present, and requiring one would have made the only
  configuration that costs nothing — and sends nobody's transcript anywhere —
  impossible to express. Verified against Ollama before it was written: a
  `response_format` carrying a JSON schema is honoured, and unknown request
  fields are ignored, which is what lets the same body carry `reasoning_effort`
  to a backend that has never heard of it. Two failure modes are named rather
  than guessed at: a reasoning model that spends its whole budget thinking
  returns HTTP 200 with an empty answer, and a structured-output refusal
  arrives as a field on a success rather than as an error status
- `anamnesis install-mcp` registers with Cursor, Gemini CLI and Codex as well
  as Claude Code, each format checked against its own documentation first, as
  the hooks were. Three of them keep the same `mcpServers` object in different
  files — Gemini's being the settings file its hooks already live in, where
  only that one key is touched — and Codex keeps TOML under `mcp_servers`,
  merged with `toml_edit` so an existing `config.toml` comes back with its
  comments and key order intact. OpenCode is refused with the reason its hooks
  are refused
- `anamnesis install-mcp` registers the MCP server with a harness, the half of
  connecting an agent that had no command. Hooks had one because setup steps
  nobody writes down fail silently; MCP had a line of documentation that
  assumed the binary was on `PATH`. On the machine this project is developed on
  it is not — it is copied out of `target/` so `cargo build` can overwrite it —
  so following that line would have registered a server that cannot start. It
  was never run at all: four months of captured sessions the agent could not
  search, and nothing said so. The registration names the executable that ran
  the command, merges rather than replaces, is idempotent, refuses to touch a
  file it cannot parse, and replaces a stale entry of its own only while saying
  what it replaced
- A second eval corpus, `crowded`: twenty-two pages, a plausible competitor for
  most questions, half the answers on pages with no authority, and a link
  cluster dense enough to offer noise as readily as signal. It exists to be the
  set no knob is tuned on, and it found the fusion defect above on its first
  run — which the ten-page suite could not show, because there fusion gained
  recall and looked like it was working
- `anamnesis eval --sweep` scores the same questions once per candidate
  setting, through the same call the server makes. The rule for accepting one
  is in the code rather than in whoever reads the table: rank up **and** recall
  held, on every suite. An eval fixture can now say that one page replaces
  another, so a corpus can ask what a wiki asks whenever somebody revises a
  decision
- Initial project structure as Rust workspace
- Core data types and abstractions
- SQLite storage layer with migrations `V01`–`V10`
- Git-versioned wiki system
- MCP server: `memory_query`, `memory_write_page`, `memory_handoff_accept`
- Lifecycle hooks capture system, with redaction before storage
- LLM provider abstraction (Anthropic Messages API), optional throughout
- Session consolidation, deterministic when no model is configured
- HTTP server for hook delivery and handoff pickup (`/hook`, `/handoff`,
  `/whoami`, `/health`) — no UI
- The workspace-wide `_global` scope is read. The data directory has reserved
  it since the beginning and the layout was designed around it, but retrieval
  answered from the current project only, so anything written there was a file
  nobody read. A query now searches the project and the shared scope as two
  rankings fused into one — ties going to the project, which is the more
  specific answer — and a hit says which scope it came from. `write-page
  --global` writes there, `reindex` rebuilds it alongside the project, and the
  wiki watcher indexes a page edited there by hand. One shared scope per
  workspace, and inheritance rather than merging: nothing is copied into a
  project
- `anamnesis write-page` reaches the rest of a page: `--tier`, `--status`,
  `--canonical`, `--entity`, and `--supersedes`, which the MCP tool has always
  accepted and the CLI could not. A page written from the command line can now
  be durable, authoritative, or a replacement for another, and it says which it
  was written as. Its entities reach the index too — the command wrote none
  before, so a page written this way was reachable through its words alone
- `install-hooks` wires Cursor, the first harness whose payload differs rather
  than only its event names. It identifies a session by `conversation_id` and
  sends `session_id` on only some events, with different values — keyed on the
  latter, one Cursor session would have been recorded as two, its boundaries in
  one and its work in another. It gives the working directory as
  `workspace_roots` except on tool events, and serialises tool results as a
  JSON string rather than an object. Its `hooks.json` declares a schema
  version, which `install-hooks` writes when the file is silent and never
  overwrites, and it takes injected context back as a top-level
  `additional_context`
- `install-hooks` wires Gemini CLI. The same five moments under four different
  names — `BeforeAgent` when a prompt is submitted, `AfterTool` when one
  finishes, `PreCompress` before the context goes — which the parser now reads
  as the boundaries they are. The way back differs too: Gemini parses a hook's
  stdout as one JSON object and rejects anything else, so the handoff travels
  as `hookSpecificOutput.additionalContext`, and every other event prints an
  empty object rather than nothing
- `install-hooks` wires Codex CLI as well as Claude Code. Codex reads
  `.codex/hooks.json`, names the same five lifecycle events, and delivers a
  payload with the same field names, so nothing downstream had to learn a
  second shape — and what a `SessionStart` hook prints on stdout becomes
  developer context there too, which is how the handoff arrives. An agent that
  cannot be wired this way is now told why rather than "not yet": OpenCode
  extends through a TypeScript plugin API, not a command hook
- `anamnesis eval --streams` scores each retrieval stream on its own and names
  the cases only it can answer — the measure that decides whether a stream
  earns its place, which a fused ranking cannot show. `Store::query_streams`
  is the diagnostic behind it, and deliberately records no access: asking
  which stream *would have* found a page is not somebody reading it, and the
  decay sweep reads those counters. First run over the shipped suite: full
  text alone scores a higher MRR than the fused ranking (0.800 against 0.708)
  while missing a fifth of the questions — fusion is buying recall with rank,
  and the link stream answers nothing on its own that the others miss
- `anamnesis eval` — retrieval scored against a checked-in corpus and the
  questions asked of it, through the same `query_pages` call `memory_query`
  makes. Reports mean reciprocal rank and recall against thresholds the suite
  file declares, so a change that costs recall has to edit a number in the
  diff. The corpus is built in a throwaway directory, never real memory: every
  query would otherwise count as a read, and the decay sweep believes those.
  The shipped suite runs as an ordinary unit test, so CI fails on a regression
  without a job of its own
- `[slots] per_user` is enforced rather than merely parsed. A project that
  sets it keeps one pending handoff per operator, so two people sharing a
  server are each handed what their own last session left instead of whichever
  note was written last. The operator comes from the bearer token a request
  presents; callers the server cannot name share the one slot they always
  shared, and a project that has not set it is unchanged. Sessions record whose
  they were either way, `anamnesis sessions` shows it, `anamnesis handoff
  --operator` peeks one slot, and `memory_handoff_accept` takes an `operator`
- Bearer-token authentication. `ANAMNESIS_TOKEN` is the secret a machine
  presents; `ANAMNESIS_TOKENS` is the `name=secret` set a server accepts, so a
  shared server can tell whose session it is recording. With neither set the
  server is open, as it always was — except on a non-loopback bind, which is
  refused unless `--allow-anonymous` says it was meant. `/health` stays open so
  `anamnesis status` can tell a server that is down from one that is refusing
  this machine, and says which on its `Auth:` line. `anamnesis token` mints a
  secret and stores nothing
- CLI entry point
- Cross-harness workstreams: named threads of work with per-thread handoff
  slots, plus the `workstream_start` and `workstream_status` MCP tools
- Retrieval over four fused signals: FTS5, entities, link neighbours, and an
  opt-in local embedder (`ANAMNESIS_EMBED_ENABLED=1`)
- Raw spool: every observation appended to `raw/` as immutable JSONL
- `anamnesis reindex` — rebuild the index from `wiki/` and `raw/`
- `anamnesis bootstrap` — seed a new project's memory from its git history
- CI: fmt, clippy, and tests on Linux and Windows for every push and PR
- `[capture] ignore_paths` is enforced: events naming an excluded path are
  dropped before an observation exists, so nothing about them reaches the
  index, the spool, or a summary
- `anamnesis sweep` — forget pages that have decayed below a retention
  threshold, or whose `expires_at` has passed. Reports and changes nothing
  without `--apply`; pinned, durable, canonical, and `do-not-answer-from`
  pages are never swept; deleted pages remain in the wiki's git history, in
  one commit that names each page and why it went
- `[decay]` in `.anamnesis.toml` — retention tuning as half-lives, read by
  the sweep and refused at load time when a value would make it nonsense
- Session pages name their entities. Consolidation produces them in both
  modes — the names a model says a later search would type, or the basenames
  of the files the session touched when no model is configured — so the entity
  retrieval stream finally sees the pages the system writes for itself
- `anamnesis improve` — file proposals from what the index already records: a
  page several sessions kept coming back to should be durable, and a page
  several pages link to should exist. Proposals are identified by what they
  are about, so a dismissal sticks and a condition someone fixed themselves
  resolves
- `[auto_improve]` is enforced rather than merely parsed: `require_approval`
  decides whether a pass may carry out its own applicable proposals, and
  `[auto_improve.scheduler]` makes the server run that pass per project, on
  that project's interval, measured from its last pass rather than from
  server start

### Changed
- Configuration marker is `.anamnesis.toml`; `.ai-memory.toml` is read as a
  fallback for projects migrating from upstream `ai-memory`
- `ANAMNESIS_DB` became `ANAMNESIS_DATA_DIR`; memory lives outside the
  repository it describes

### Fixed
- Entity matching finds names that are not a single word. Names were stored
  whole and compared against tokenized queries, so `Windows BOM` or
  `anamnesis-llm` could never match anything, and the pages they named were
  reachable through full text alone. Names are now split at write time, and an
  entity matches when every one of its tokens is in the query; names stored
  before this still match whole, and are split the next time their page is
  written or reindexed
- Supersession reaches the index. `supersedes` was accepted by the MCP tool,
  written into frontmatter, and then dropped: no column was written and
  `is_latest` never changed, so an agent recording that one page replaced
  another kept being answered with the page it replaced. The claim is now
  stored as authored and resolved in both directions, so it survives the two
  pages being written in either order, and `show-page` says when a page has
  been replaced
- Session pages written by the server have their wikilinks indexed, instead of
  only after a rebuild
- Backlinks now resolve when the target page is written after the page that
  links to it
- Rebuilt sessions come back closed when the transcript records their end
- The first run is readable again. Refinery logs the entire SQL text of every
  migration at info, so `anamnesis init` saying where memory now lives scrolled
  away under several screens of schema that is checked into this repository.
  Quieted to `warn` unless `--debug` or an explicit `RUST_LOG` asks — a
  migration that fails halfway is exactly when the statement is worth seeing
- A handoff request that fails is no longer printed as a handoff. The hook read
  the body without looking at the status, so an error page — a 401 among them —
  went to stdout, where the harness injects it into the model's context as
  though the last session had written it

### Removed
- The empty `anamnesis-workstream` crate; workstreams live in core, store,
  and mcp instead
- The `new-session` CLI command; sessions are created by hooks and MCP

## [0.1.0] - 2026-08-19

### Added
- Project initialization
- Workspace structure with 10 modular crates
- Configuration templates
- Documentation and contribution guidelines
