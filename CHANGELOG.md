# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **The long-run eval records what recall showed each session.** The first
  run with recall tied the control arm 2/5 to 2/5, and taken apart by hand it
  said three different things: recall had put the page each probe's knowledge
  was planted in in front of four probes out of five, the local model writing
  those pages had left the knowledge out of three of them, and one probe read
  the warning, looked the page up, and made the mistake anyway. A pass rate
  says none of that. Each memory-arm session now records the pages its recall
  block named and which session wrote each, read from Claude Code's own
  transcript since `stream-json` carries nothing a prompt hook printed, and
  `report` splits every probe by whether it was shown its plant. A transcript
  that cannot be found is recorded as unknown, not as nothing offered.

- **`anamnesis setup` wires every installed agent by default.** A bare setup
  used to wire Claude Code even when Codex was installed beside it, so the
  memory held a waiting handoff that the next agent never received. Setup now
  detects launchers on `PATH` and harness configuration already in the project,
  and plans hooks and MCP registration for each one. Repeated `--agent` flags
  still override detection, and a machine where no harness is visible keeps
  the original Claude Code default
- **A prompt is answered with no model running.** Recall at prompt time needed
  an embedder, and without one it said nothing — in a setup this system
  supports, and whenever a local model was down. Now a server with no embedder,
  or one whose embedder fails on this prompt, answers from the words the prompt
  names. Each word is weighed by how few pages carry it. A page is offered when
  it carries at least half of what the prompt names, counting words the project
  has never written against it. A session summary is offered only through a
  name it carries (a file, a command, a version), because its other words are
  the conversation's own. Over 201 real prompts on this project's pages it
  gave a block 8 times, where the cosine gate gives one 184 times. Asked of a
  project they were not about, 213 prompts got none. It cannot see a
  paraphrase, so a server with an embedder keeps the cosine gate. `[recall]
  by_name = false` turns it off, and `min_coverage` sets the share
- **`anamnesis eval --gate` measures whether recall keeps quiet, with no
  labels.** Recall at prompt time has to say nothing when the project has
  nothing to say, and that half is usually judged by reading prompts and
  labelling them by hand. The built-in suites are four unrelated systems, so a
  question written for one has no answer in the others. Asked there, every
  block it gets is a false alarm. `--gate` asks every suite's questions of
  every corpus and prints both halves: how often a corpus answered its own
  questions and led with the right page, and how often it answered questions
  it could not. Recall by name gave 1 false alarm in 171. The same count runs in
  CI as a test, so a change that makes recall chatty fails there.

### Fixed
- **Recall answers where the harness can hear it, under the event it
  answers.** 1.2.1 said the prompt hook asks `/recall` under whatever each
  harness calls a prompt, and prints the answer where the harness injects it.
  For Cursor there is no such place: `beforeSubmitPrompt` takes back
  `continue` and `user_message` and nothing else, so every Cursor prompt was
  embedded and queried for a block Cursor never read. Cursor now keeps its
  handoff at `sessionStart`, which does take `additional_context`, and is not
  asked about its prompts. And Gemini CLI's `BeforeAgent` reply named itself
  `SessionStart`, the only event that reply had answered before recall; Gemini
  reads the context without checking the name today and declares it per
  event in its own types, so the reply now names the event it answers. Checked
  against each harness's documentation and, for Codex and Gemini CLI, their
  source.

- **A Cursor session claims its handoff under its own name.** The hook
  command asked the server for a handoff by `session_id` and `cwd`, read from
  the payload itself, while capture read the same payload by the rules that
  know Cursor names the session `conversation_id` and the directory
  `workspace_roots`. Cursor sends neither of the first two on `sessionStart`,
  so a Cursor session was recorded under its own name and then claimed its
  handoff under an empty one: the handoff row pointed at a session id derived
  from nothing — the same one for every Cursor session — and the audit entry
  named nobody. Found by driving one project through Claude Code, Codex,
  Gemini CLI and Cursor in turn with each one's own payloads. The hook now
  asks by the same function capture reads with.
- **A reply too short to be about anything is not asked about.** Recall's
  similarity gate came from sixteen prompts written for it, and on the 201
  real prompts this machine's sessions had recorded it let a block through for
  184 of them — every prompt of five words or more, and ten of the twenty-two
  one- and two-word replies like `devam et` or `onay`, all of which had no
  subject; the one-word `yaptım` had 57 pages above the gate. The gate cannot
  simply be raised: in the long-run eval a probe's planting page scored 0.64 to
  0.73 against it, inside the band where the wrong pages sit on the live
  corpus. So a prompt now needs `[recall] min_words` words, three by default,
  counted by Unicode's boundaries so a sentence in a script written without
  spaces is not one word, before anything is embedded. The measurement, what
  else was tried, and what is still open are in
  `docs/measurements/2026-09-18-recall-on-real-prompts.md`. A marker that sets
  `min_words` is refused by a 1.2.1 build, whose `[recall]` table does not know
  the key — upgrade the binary before the marker.

- **A notification the harness submitted is not asked about.** A background
  task finishing reaches a Claude Code session as a prompt, through the same
  hook as a person's question, and the prompt hook sent it to `/recall` like
  one. On 2026-09-18 a finished command's `<task-notification>` came back with
  three pages about nothing it said, injected where the agent reads what it is
  being asked — and in this project's own index one recorded prompt in four is
  a notification. The handoff already told the two apart; the test it used now
  lives in `anamnesis-core` and both paths use it. The hook does not ask, and
  the server does not answer when an older hook does.

### Security
- **A password typed on a command line was stored whole.** Redaction knew
  provider keys by their prefix and any other value by the `=` or `:` in front
  of it, and a shell command — most of what a tool call records — has neither:
  `mysql -pS3cret` glues the value to its flag, `sshpass -p`, `psql --password`,
  `docker login -p` and `redis-cli -a` put a space in front of it, and
  `curl -u user:pass` hides it behind a user name. Of thirteen ordinary shapes
  of secret tried, the redactor masked none. Twelve are masked now: those, a
  `Cookie:` or `Set-Cookie:` header, a `.netrc` line, a name ending in `_PASS`,
  and a password said in a sentence — "the admin password is …", or in
  Turkish, `şifre: …`, `veritabanı parolası …`, `API anahtarı …` — where the
  value has to hold a letter and a digit or a symbol, so "the password is
  wrong", `şifre yok` and a date after the word stay as written. The
  thirteenth, a bare forty-character hex string, is what a commit hash looks
  like, and stays. Every value stops at a quote, a backtick and a backslash,
  because the text a hook redacts is the tool input rendered as JSON. Run over
  this machine's whole spool — 122,000 strings — the new rules matched nothing
  but the synthetic values and placeholders of their own tests and comments.
  What was captured before this is untouched until `anamnesis redact --apply`
  runs the current rules over it

## [1.2.1] - 2026-09-18

Until this release one thing in memory reached a model without being asked for:
the handoff, delivered once at the start of a session, saying what the session
before it did. Everything else waited for `memory_query`, and the long-run eval
measured how long that wait is — across twenty-four sessions with the MCP
server connected and its tools allowed, an agent called a memory tool **once**,
and that call was a write. A question five sessions after its answer was
written never found it.

So the question is asked for the agent now, at the one moment there is a
question in hand. A prompt goes to `/recall`, and the pages this project
already has on it come back where the harness injects them, framed as evidence
to check rather than instruction to follow.

Most prompts get nothing back, and that is the part that took the measuring. A
block that fires whether or not it has anything to say teaches an agent to skip
it, and the ordinary fused query cannot tell those apart: rank fusion keeps
ranks and throws the scores away, so on this machine's own pages `what is the
weather in Istanbul` came back with three pages and the same 0.333 at the top
as a question about the project's centre. Cosine similarity keeps the score,
and sixteen prompts over two corpora split on it — a prompt the project had
nothing to say about peaked at 0.542, one it did started at 0.573 — so
`[recall] min_similarity` sits between them, in the marker rather than the code
because it is a number about one embedder. A server with no embedder says
nothing rather than guessing.

The rest is the day that measurement took, and the four fixes already waiting
for a release. The eval's own allowlist refused 59 of its 338 tool calls, most
of them a PowerShell tool that was on no list and a way of running the
fixture's tests that the scenario had planted nothing about; a run whose first
planting page comes back written by counting now stops there rather than
spending two hours and $1.59 to measure nothing; and the memory arm can be
pointed at a model that is not on somebody's daily quota. `install-hooks`
writes where the harness reads rather than where the command was typed, a key
stored under a name nothing will read says so, and the nginx check says why it
stopped.

The index schema is unchanged at 18; nothing written by 1.2.0, 1.1 or 1.0 needs
converting. A 1.2.0 build reads the new `[recall]` table as one it does not
understand and says so in `status`, so a machine whose marker is upgraded
before its binary keeps working.

### Added
- **A prompt is answered with what this project already knows about it.** The
  handoff says what the session before this one did, and it was the only thing
  in memory that ever reached a model without being asked for. Everything else
  waited for `memory_query`, and the long-run eval measured how often that
  comes: across **twenty-four sessions with the MCP server connected and its
  tools allowed, an agent called a memory tool once**, and that call was a
  write. A question five sessions after its answer was written never found it.
  So the question is asked for the agent, at the one moment there is a question
  in hand: the prompt hook — `UserPromptSubmit` and whatever the other three
  harnesses call it — now also asks `GET /recall`, and prints what comes back
  where the harness injects it. It takes nothing and claims nothing, unlike the
  handoff, and it gets five seconds where capture gets one, because it runs
  once per prompt rather than before every tool call and the server has to
  embed the question first: measured here, that round trip took 1.03s against a
  cold Ollama and 0.05s against a warm one, so under the capture budget the
  first prompt after an idle embedder came back empty and said nothing about
  why.

  The hard part is not finding pages, it is not offering them — a block that
  fires on every prompt whether or not it has anything to say teaches an agent
  to skip it. The ordinary fused query cannot tell those apart: rank fusion
  keeps ranks and throws the scores away, so on this machine's 88 pages `what
  is the weather in Istanbul` came back with three pages and the same 0.333 at
  the top as a question about the project's centre. Cosine similarity keeps the
  score, and sixteen prompts over two corpora with `nomic-embed-text` split on
  it: a prompt the project had nothing to say about peaked at 0.542, one it did
  started at 0.573. `[recall] min_similarity` sits between them, in the marker
  because it is a number about one embedder, beside `on_prompt`, `pages` and
  `snippet_chars`. A server with no embedder says nothing rather than guessing.
  What is offered is framed as evidence rather than instruction in its own
  first sentence, and being offered does not renew a page against the decay
  sweep — a block that renewed everything it mentioned would make the top of
  the ranking immortal without anyone reading a word of it


### Fixed
- **`status` reports a hosted embedder that failed during startup.** The first
  connection attempt happens before the server's failure watcher exists, so a
  server that continued with its reconnecting embedder exposed
  `embedding_failure: null` until another embedding request failed. The startup
  refusal now seeds that watcher and a later successful request still clears it

- **A run whose first planting page was written by counting stops there.** The
  check before a run asks each model one small question, and a model out of
  quota can still answer it: on 2026-09-17 `key check` said every model
  answered, the run started, and its first page came back counted. `report`
  excludes every probe whose planting session's page was not written by a
  model, and a model that refused one session refuses the rest of the hour, so
  the eleven sessions after it would have spent two hours and $1.59 measuring
  nothing. A run now asks the same question of the work rather than of a ping:
  when a planting session leaves a counted page, or no page, it says which
  probe that costs and exits 5. `--keep-going` runs the whole scenario anyway.
  `settings.local.env.example` is the other half — a repeat is twelve
  consolidation requests against a free tier of twenty a day, shared with
  whatever server is already running on the same key, so the memory arm can be
  pointed at a local model instead; measured here, a session's page came back
  written by that model and spent no quota

- **The long-run eval stops refusing what its own scenario asks for.** Both
  arms are started with one `--allowedTools` list, and nobody is there to
  answer a prompt, so a tool left off it is refused and the session spends a
  turn finding that out. The first complete run refused **59 of its 338 tool
  calls**. Most were the list being wrong rather than strict: on Windows a
  session also has a PowerShell tool, which was on no list, so 31 calls went
  to it and 27 came back refused — and no rule narrows it, since with only
  `PowerShell(python:*)` allowed `Get-ChildItem` ran, which would hand an
  unattended nightly run an unbounded shell. It is taken away with
  `--disallowedTools` now, so it is not there to reach for. The fixture's
  tests read `LEDGER_FIXTURES` from the environment, so the natural
  `LEDGER_FIXTURES=tests/fixtures python -m unittest ...` does not begin with
  `python` and was refused four more times, while `python tools/check.py`,
  which sets the variable itself, was allowed; the scenario plants nothing
  about how the tests are run, so `env`, `export` and that variable are
  allowed. Refusals do not fall equally on the two arms — in the first run one
  probe cost the memory arm five and the control arm none — so `results.json`
  now records them per tool and `report` prints the total and a column per
  session. Run again on one session, the same prompt went from five refusals
  to two
- **A key stored under a provider's own name is no longer stored in silence
  while the generic one outranks it.** `ANAMNESIS_LLM_API_KEY` is the
  configured provider's key, so with a provider named it wins over
  `GEMINI_API_KEY` and the rest. That precedence is deliberate and was
  invisible. On 2026-09-17 a key revoked three days earlier was still stored
  under the generic name; a new one was written with `anamnesis key set
  GEMINI_API_KEY`, `key check` answered with the same `400 Please pass a valid
  API key`, and nothing anywhere said the new key had not been sent to
  anything. A refusal that means "the old key is still the live one" is
  indistinguishable from one that means "your new key is bad", and reading it
  the second way cost three days. `key set` now says when the name just
  written is not the name that will be read, `key check` says it above the
  verdict instead of leaving it to be inferred from the variable it names, and
  `serve` warns at startup, beside the warning for the opposite case

- **A command that writes a harness's configuration now writes it where the
  harness reads it.** `install-hooks`, `install-mcp` and `uninstall` anchored
  those files at the working directory, while identity has always been
  resolved by walking up to the project's marker, so the two disagreed the
  moment anything ran from a subdirectory. On 2026-09-17 `doctor`, run in
  `crates/anamnesis-evals/longrun`, reported `no harness in this project is
  wired to anamnesis` about the project it was at that moment recording, and
  sent the person to `install-hooks` — which wrote
  `.claude/settings.local.json` into that subdirectory, where Claude Code
  never looks, after which `doctor` called the same project healthy from the
  same place. Both dry runs printed `.\.claude\settings.local.json`: one
  sentence for two different files, so there was nothing to notice. All four
  resolve the project root the way `setup` always has, and the dry runs name
  the absolute path they would write. `--settings`, `--config` and `--repo`
  are still taken exactly as given

- **The long-run eval stops when what it writes is not where it says it is.**
  A Microsoft Store Python runs inside its package's filesystem redirection:
  everything it writes under %LOCALAPPDATA% lands in that package's LocalCache
  instead, and nothing says so. The anamnesis binary is not in the package, so
  it reads the path it was handed and finds it empty. On 2026-09-17 a run
  stopped at its model check with `no model is configured`, about a
  settings.env the harness had copied a second earlier. The model check was
  the only reason that run cost nothing; every later path would have been
  wrong the same way, and a run whose memory arm has no model measures
  nothing for two hours. A run now writes one probe into its own directory
  before anything else and stops if it did not land there, naming both paths
  and what to use instead

- **The nginx check says why it stopped.** Its first step makes the
  certificate it runs behind, in a container, and sent both its output and its
  error to /dev/null. When the image could not be pulled on 2026-09-17 the
  script ended there under `set -e`, and the job failed with exit 125 and an
  empty log — which reads like the check itself failing rather than a pull. It
  now keeps that output and prints it only if the step fails, naming what
  could not be made

## [1.2.0] - 2026-09-16

On 2026-09-14 the model key this project's own memory runs on stopped being
accepted. Every session that day was written by counting tool calls instead of
by a model, the server sent 820 refused requests before 15:05, and the sentence
saying why — `Please pass a valid API key` — sat on the fifth line of a log
entry nobody was reading. Finding it out took a day. Most of this release is
the answer to that day, and none of it is the key.

A refusal is now read for what it says. The log gives Google's status and its
sentence on one line instead of eight; `status` names what the model last
answered and what the embedder last refused; `doctor` gives the server's reason
for pages written by counting rather than sending people to compare
environments. `anamnesis key check` asks every model in the chain one question
with the key a server started now would use, says which variable it came from,
and notices when the running server is still holding a refused one. A model
that will not answer is asked again with a widening pause rather than every
minute, and a quota spent for the day is not retried at all. `anamnesis service
restart` is the step between a new key and a server that has read it, which on
Windows had no command before this.

The other half is what running things for the first time found. No OpenCode
session had ever been handed the note the one before it left. `serve` printed
its banner with `println!`, so a wrapper that stopped reading killed the server
one start in ten. A background pass that panicked ended its loop, and the
server went on answering `/health` with nothing being summarised behind it. A
wiki commit wrote the index it first read, so one process's commit could remove
another's. A `[[link]]` resolved only as a path from the scope root, which is
not how anyone writes one — eleven links in this machine's memory named pages
that existed. And the Docker templates described endpoints, variables and a
compose file that are not there, behind an nginx that set aside every hook
event over 1 MB.

Installing it is now `brew`, `scoop` or `cargo binstall` as well as the
scripts, from manifests rendered out of a release's checksums rather than
written by hand.

The index schema is unchanged at 18; nothing written by 1.1 or 1.0 needs
converting.

### Added
- **The server gives a page the vector it was written without, once its
  embedder answers.** A page written while the embedding endpoint is down is
  indexed without a vector and filed for `doctor`, and only a hand-run
  `anamnesis reindex` ever asked again. On 2026-09-15 this machine's server
  came up twice before Ollama did, and `doctor` counted four session pages
  missing from the vector stream by the evening. Every minute, when any page
  is missing a vector under the model it embeds with, the server now sends the
  endpoint one short string, and only when that is answered does it send the
  pages, twenty at a time, each read from the wiki under the same hold a
  consolidation takes. An endpoint still down costs one quiet request a minute
  rather than a warning per page. `doctor`'s remedy says the server does this,
  and `GETTING_STARTED.md` no longer claims `serve` refuses to start without
  its endpoint, which stopped being true in 1.1.1
- **`anamnesis service restart`** stops the server the service keeps running
  and waits until the one started in its place answers, which a new key, a
  changed `settings.env` and a new binary all need. On Windows there was no
  verb for it: `schtasks /End` ends the task and not the server — measured
  here, the task went `Ready` while the same process went on answering,
  outside the task — and the documented step was to find the `serve` process
  by its command line and stop only that one, since the MCP server a harness
  starts is also `anamnesis.exe`. The command finds the process listening on
  the port (by its remote end being port zero, not by the state column Windows
  prints in the machine's language), stops it only if it is `anamnesis.exe`,
  runs the task, and waits for a different process to answer. Linux runs
  `systemctl --user restart`, macOS `launchctl kickstart -k`. Run on this
  machine, it replaced the server in about a second, under the task's own
  `conhost --headless`
- **`status` says when the embedder is not returning vectors.** The `Vectors:`
  line named the server's embedding model and nothing else, so on 2026-09-15,
  with Ollama not started, it read `nomic-embed-text` all afternoon while
  every page written went into the index without a vector. The server now
  watches its embedder the way #233 watches its model, reports the last
  refusal in `/whoami` as `embedding_failure` and forgets it at the next
  vector, and `status` prints `Vectors: nomic-embed-text — not returning
  vectors: failed: could not load model "nomic-embed-text": …, 2m ago`
- **`anamnesis key check`** asks each configured model one small question with
  the key a server started now would use, and says what came back: the key
  accepted, refused, or out of today's quota, a model that does not exist, a
  service that did not answer. Every link of a fallback chain is asked on its
  own and once, since a chain hides the link that failed and a retry spends
  quota on the answer already given, and the line names which variable the
  key came from and whether from this shell or the credential store. It exits
  non-zero when any model could not be shown to work. Between `key set` and a
  server restarted to read the key there was nothing to run; on 2026-09-14 a
  key that had stopped working was found a day later in a log. Run on this
  machine, it named the stored Gemini key as refused for both models in one
  request each. Google words a refused key by the key's shape — `Please pass a
  valid API key`, or `Invalid Auth key.` for an `AQ.` key it does not know —
  and both are read as refused; an ignored live test that needs no key asks
  the real endpoint with both shapes and holds the refusal's format
- **`status` says what the model answered when it did not answer with a
  page.** The `Summaries:` line reads the sessions, and on 2026-09-14 it said
  exactly what they showed: "the last 4 were counted, the model is not
  answering". Why was in the server's log. A rejected key, a spent quota and
  an overloaded model all read the same from the sessions and each needs
  something different done. The server now remembers the last request its
  model did not answer, forgets it when the next one is answered, and reports
  it in `/whoami` as `consolidation_failure`; `status` prints it underneath as
  `Why: gemini-3.5-flash answered 400: Please pass a valid API key, 3m ago`.
  The reason is redacted before it is kept, in case a gateway echoes a
  credential back, and held in memory, so a server restarted after a key is
  fixed does not go on naming the old refusal
- **A long-run eval with a real agent**, in `crates/anamnesis-evals/longrun/`.
  `anamnesis eval` measures whether memory finds the page that answers a
  question; this measures whether an agent does the right thing sessions after
  it was told something. Twelve headless `claude -p` sessions on a small Python
  fixture run twice, once wired to a server and data directory of their own
  and once with nothing that carries between sessions (Claude Code's own
  memory turned off, and the setup asked whether it has any before the run
  starts). Five sessions plant something the repository does not say (amounts
  never go to logs, a generated file is never edited by hand, a cache that
  served stale rows, a staging host, how the tests are run), two are unrelated
  work, and five are tasks that need a plant. Each is judged by running the
  fixture's code, never by a model, and `checks.py` holds every probe to a
  repository that gets it right and one that makes the mistake it is there to
  catch; CI runs that. A memory-arm probe is counted only when the page of its
  planting session was written by a model and the MCP server was connected.
  Tried on two sessions: hooks, MCP, page tracking and the report worked, at
  about ten cents a session with Haiku 4.5. A run first asks the memory arm's
  model through `anamnesis key check`, with the settings its server will
  read, and stops before any session when a model cannot be shown to work,
  since every probe after a counted page is excluded; `--skip-model-check`
  starts anyway
- **Homebrew, Scoop and cargo-binstall.** This repository is the tap and the
  bucket: `HomebrewFormula/anamnesis.rb` (macOS on both architectures, Linux
  x86-64) and `bucket/anamnesis.json`, both written by `packaging/render.sh`
  from a release's `SHA256SUMS` and never by hand, since a manifest is a
  promise about bytes somebody else downloads. The release workflow renders
  them on every run, a rehearsal included, and a tag pushes the result as a
  `packaging/<tag>` branch for a pull request. CI installs from the checkout
  with `brew` on Linux and macOS and `scoop` on Windows, fails when the
  committed files differ from what the script makes of the release they name,
  and installs with `cargo binstall` from the `[package.metadata.binstall]`
  the CLI's manifest now carries. The formula's caveat asks for `anamnesis
  setup` again after every `brew upgrade`, which is what a hook naming the
  Cellar path of the version brew has just replaced needs; it no longer
  promises that hooks name a path an upgrade keeps, since what a release
  writes into a hook belongs to that release and not to the formula. On Linux
  arm64, which no release is built for, it asks for the x86-64 architecture so
  that brew names the reason rather than failing on the url the platform
  blocks left unset. It also carries a `livecheck` block, the formula's
  counterpart of the bucket's `checkver`, so that `brew livecheck` and
  `brew bump-formula-pr` see a tagged release whose manifests have not been
  merged yet.
  `WINGET=1` also writes the three winget manifests, which are submitted to
  `microsoft/winget-pkgs` rather than kept here, and the directory they land
  in is ignored so that a copy nothing regenerates cannot be committed by
  accident

### Changed
- **The vector stream answers in less than half the time.** Measured over
  5,000 pages of 768-dimensional vectors (`vector_stream_cost`, an ignored
  test): 19.4 ms a query before, 8.9 ms after. Most of it was SQLite building a
  temporary B-tree of every row, vector included, only so the rows of one page
  arrived together; they are grouped by page in Rust instead, and equal scores
  still come out in page id order. The rest was decoding each stored vector
  into a vector of its own and taking the query's norm again for every row;
  the comparison is now made in the stored bytes with the norm taken once,
  held by a test to the answers the decoding version gave. The abstract stream
  compares the same way

### Fixed
- **A server nobody reads the banner of keeps serving, and a hook nobody
  reads exits 0.** `serve` printed its banner with `println!`, which panics on
  a closed pipe: a wrapper that read the first line to learn the address and
  stopped reading killed the server one start in ten (`failed printing to
  stdout: The pipe is being closed`, exit 101). The MCP server's stderr banner
  had the same edge. The banners now skip a line nobody can receive. The hook
  command promises exit 0 whatever happens, and a harness that stopped reading
  before the handoff was printed made it exit 101; a panic inside the hook no
  longer changes its exit status. Found by the first tests that run the built
  binary: `tests/capture_loop.rs` starts a server and plays a Claude Code and a
  Gemini CLI session through `anamnesis hook` — capture, the page, the next
  session's note in each harness's shape — and `tests/mcp_stdio.rs` drives
  `anamnesis mcp` over stdio, with every stdout line required to be a protocol
  frame. Both run on every platform CI tests, Windows included
- **`uninstall` removes a configuration file it leaves empty.** `setup`
  creates `.mcp.json` and `.claude/settings.local.json` in a project that has
  neither, and taking anamnesis back out wrote them back as `{}`: a `.mcp.json`
  left in the project root for somebody to commit. A file with nothing left in
  it is removed now, with the harness directory it was in (`.claude/`,
  `.cursor/`, `.gemini/`, `.codex/`) when that is empty too; a file holding
  anything else — another server, somebody's hook, a Codex setting — is written
  back with that. Never the project directory itself: the first version took
  any empty dot-directory, and a test whose project was `.tmpXXXX` lost it
- **The image's health check can fail, and the development image builds.**
  `HEALTHCHECK` ran `anamnesis status`, which describes and always exits 0:
  a container whose server was frozen (`kill -STOP`) was still `healthy`
  thirty seconds later. It runs `anamnesis hook --probe` now — the event a hook
  would send, recorded nowhere, exiting non-zero when memory would not be
  recorded — and the same container went `unhealthy`; the compose file's check
  is the same command. `Dockerfile.dev` installed `cargo-watch` from apt, which
  has no such package, so the image never built (`Unable to locate package
  cargo-watch`); it comes from crates.io now. Both images install packages
  without apt's recommendations. CI checks the health check both ways and
  builds the development image. `DOCKER.md` no longer documents build
  arguments the Dockerfile does not take, a Helm chart that does not exist, a
  `SQLITE_CONFIG` variable nothing reads, or `memory.db` as the index — a
  `VACUUM` against that name creates an empty database and succeeds
- **A background pass that panics no longer ends its loop.** The reaper and
  the enricher ran `loop { pass().await; sleep }`, so a panic anywhere in a
  pass — a stored row that does not parse is enough — ended the task: the
  server went on answering `/health` while no abandoned session was summarised
  and no counted page was asked about again until a restart. Each pass now
  runs on its own task, a panic is logged and the next pass runs on schedule,
  and a panicking enricher pass counts as one in which nothing was written, so
  it backs off rather than logging the same panic every minute. The session
  list, each session's scope — a marker file read and a git repository opened,
  from a checkout that may be on a slow drive — and the claim on a session are
  read off the runtime, where every other index read already was
- **An OpenCode session is handed the note the last one left.** OpenCode has
  no channel from a hook's stdout to the model, so its plugin runs `anamnesis
  hook` with stdout ignored and asks `/handoff` itself, to put the note in the
  system prompt. The hook command claimed the handoff at every session start,
  whichever harness ran it, and a handoff is claimed once: the plugin reports
  the start first, the note went to the ignored stdout, and the plugin's own
  request found nothing. No OpenCode session had ever received one. The hook no
  longer claims it for OpenCode. Found by running the plugin for the first
  time: `crates/anamnesis-cli/tests/opencode-plugin/` installs it with
  `install-hooks`, starts a server, and plays two OpenCode runs through it
  under Bun — prompt, tool call, compaction, end, and a second run that must be
  handed the first one's note once — and CI runs it in the image
- **The Docker templates describe a server that exists.** Behind
  `docker/nginx.conf.example`, every hook event over nginx's default 1 MB body
  came back 413 — a large `Read` is past that, and the server reads 16 MB — so
  it was set aside and never recorded. The template also added an
  `X-Frame-Options: SAMEORIGIN` beside the server's `DENY`, a conflict a
  browser may resolve by ignoring both, rate limited `/api/search`, which the
  API does not have (search is `/api/v1/scopes/<ws>/<project>/search`), and
  used the deprecated `listen … http2`. `docker/check-nginx.sh` now runs the
  template in front of the image and sends it a 2 MB hook event, an API call
  and a browser request; the old template fails three of its five checks, and
  CI runs it. `docker/.env.example` offered `ANAMNESIS_DB`, `PORT`, `BIND`,
  `STORAGE_TYPE`, `DEBUG` and `MCP_BEARER_TOKEN`, none of which anything reads
  — the last one a server left open by whoever set it; it now names only
  variables that are read, and a test fails when it names one that is not.
  `docker/README.md` described a `docker-compose.prod.yml` that is not in the
  repository, `/api/status` and `/metrics` endpoints that do not exist, a
  database at `memory.db`, and scaling to three replicas of a single-writer
  index; it is rewritten to what is there
- **A reply cut off part way through its body is asked again, and the log
  says what cut it.** Twice on 2026-09-13 the server logged `llm transport
  failed: error decoding response body` and fell back to a counted page on the
  first attempt: a timeout or a refused connection was retried, a body that
  stopped arriving was not, though it is the same dropped connection a moment
  later. It is retried now, and a transport error prints its causes, as
  `error decoding response body: … error reading a body from connection: end
  of file before message length reached`, where the log had only the first
  clause
- **A wiki commit no longer drops what another process committed.** The
  server, the MCP server a harness starts and every CLI command that writes a
  page each open the wiki's repository, and a `git2::Repository` keeps the
  index it first read. A commit wrote that index's tree: on 2026-09-09 the
  server committed a session page from an index read before a `write-page`
  and a `recompile` had committed, and its commit removed the gotcha from the
  history and put the recompiled session page back to what it said before.
  Both files stayed on disk, so nothing looked wrong until `git status` in the
  wiki showed three session pages and two gotchas as changes nobody had made.
  Every commit now starts from HEAD's tree and applies only its own paths, and
  a HEAD another process moved in between is committed onto again rather than
  overwritten
- **An embedding endpoint on this machine that is down costs half a second,
  once.** With Ollama stopped, `anamnesis search` took 4.1 s on Windows: a
  connection to a loopback port nothing listens on is refused there only after
  two seconds of trying again (measured: 2.06 s, twice), and the command paid
  it twice — once for the probe that found the endpoint down, and again for the
  query, because the embedder it fell back to had not been told an attempt had
  just failed. A loopback endpoint now gets a 500 ms connect timeout, and the
  fallback counts the failed probe as its last attempt. The same search takes
  0.6 s; the server, the MCP server, `write-page` and `bootstrap` start the
  same way
- **`improve` counts the pages asking for a missing page however each one
  spelled the link.** A missing page is proposed once two pages link to it,
  and the links were grouped as written: `[[windows-bom]]` on one page and
  `[[windows-bom.md|the BOM trap]]` on another were two targets with one page
  each, so the page two pages asked for was never proposed. They are grouped
  by the page they ask for now — alias and heading off, `.md` on — and a page
  linking twice in two spellings counts once
- **Every "restart the server" names the command, and `key check` notices a
  server still holding a refused key.** `key set`, `key forget`, `key check`
  and three of `doctor`'s remedies said to restart the server and not how,
  which on Windows meant finding the right `anamnesis.exe` by its command
  line; they now name `anamnesis service restart`. And the step is the one
  most easily skipped after a new key: `key check` reads the credential store
  as it is now, the server read it when it started, and a check that passes
  said nothing about the process writing pages, which went on sending the
  refused key while `status` named the old refusal as if the new key had
  failed too. When every model answers the check and the running server's
  last answer was a refused key, `key check` now says the server still has the
  key it started with
- **A `[[link]]` is resolved the way Obsidian reads it.** The index accepted a
  link only as a path from the scope's root, `[[gotchas/windows-bom]]` or the
  same with `.md`, and nobody writes one that way: Obsidian resolves
  `[[windows-bom]]` to the page of that name wherever it is, and an agent
  writing beside another page names it the same way. On 2026-09-15 the index
  on this machine held eleven unresolved links and eight named a page that
  existed, each written from one page in `gotchas/` to another; the
  link-neighbour stream never saw those edges, and `anamnesis improve`
  proposed writing a page the wiki had held for nine days. A link now resolves
  from the root as before, then beside the page that makes it, then to the one
  page whose path ends that way; a name two pages share and neither beside the
  link is left unresolved rather than guessed. An alias (`[[page|shown]]`) or
  heading (`[[page#part]]`) is set aside first. The wiki browser applies the
  same rules to its live links. `anamnesis reindex` re-resolves what an index
  already holds; on a copy of this machine's memory it left three unresolved
  links, all naming pages that do not exist, and the proposal resolved itself
- **`doctor` gives the server's reason for counted pages, and stops blaming
  the terminal.** Its "pages written by counting" finding said the terminal's
  `ANAMNESIS_LLM_PROVIDER` "the server does not inherit" and sent people to
  compare environments, which stopped being true when every command started
  reading `settings.env` and the credential store; on 2026-09-15 it said that
  about a key the server had heard refused for a day. It now reads
  `consolidation_failure` from `/whoami` and appends the model's answer to the
  finding. A refused key (401, 403, or a 400 naming the key) gets the remedy
  for one — `key set`, `key check`, restart — any other refusal says the
  server rewrites the pages once the model answers, and with no reason
  reported it points at `anamnesis key check`
- **A refusal from Google is read for what it says.** Google's compatible
  surface wraps its error object in a one-element array and names the kind in
  `status` rather than `type`, and neither was read: every refusal was logged
  as `llm api error 400 (unknown):` followed by the raw body across eight
  lines. The rejected key on 2026-09-14 spent the day that way, the sentence
  saying `Please pass a valid API key` on the fifth line of each entry. The
  line is now `llm api error 400 (INVALID_ARGUMENT): Please pass a valid API
  key`. A spent quota also names which one it was, as
  `[GenerateRequestsPerDayPerProjectPerModel-FreeTier]`, and the wait Google
  states in the array's details is honoured rather than read out of the
  sentence. Every error message is one line, and a body that is not JSON is
  cut at a thousand characters
- **A quota spent for the day is asked about once, and handed to the next
  model.** Google's free tier allows twenty requests a day per model, and its
  refusal is a 429 like a per-minute limit's, asking for half a minute. The
  retry loop took it at its word: with the server's eight retries for
  background work, each session spent about five minutes and nine refusals on
  a model that would not answer before midnight Pacific, and a fallback in
  `ANAMNESIS_LLM_FALLBACK_PROVIDERS`, on a quota of its own, waited behind all
  of them. A refusal naming a per-day quota is no longer retried, is still
  handed on along the chain, and still stops `anamnesis abstracts`, where
  every later page would meet the same refusal
- **`ANAMNESIS_LLM_API_KEY` no longer selects Anthropic.** It is the key of the
  provider `ANAMNESIS_LLM_PROVIDER` names, and `anamnesis key set` keeps it in
  the account's credential store, which every data directory on the machine
  reads, while the provider is named per data directory in `settings.env`. In
  a data directory without that line the stored key still selected Anthropic:
  a server started on 2026-09-14 for an eval, in a directory of its own, sent
  this machine's Gemini key to api.anthropic.com with a session's transcript
  and logged a 401 every minute. With no provider named the key is now sent
  nowhere, pages are counted, and `serve` logs which key was set and not used.
  `ANTHROPIC_API_KEY` still selects Anthropic, and is the key sent when both
  are set. A named provider also stops taking another provider's variable:
  `openai` read `ANTHROPIC_API_KEY` when no stored key was set, and now reads
  `OPENAI_API_KEY`
- **The server stops asking a model that does not answer every minute.** The
  pass that asks again about sessions whose page was counted ran each minute
  over the three oldest, whatever had happened to them. On 2026-09-14 the
  model key on the machine this project runs on stopped being accepted, and
  from 00:11 the server asked about the same three sessions every minute: 820
  refused requests by 15:05, and with a working key and a spent daily quota
  the same loop would spend the rest of the day the same way. A session that
  can never be enriched, its checkout moved or nothing in it, also held one of
  the three places for good, so nothing behind it was ever asked about. A
  session that comes back without a page now waits before it is asked about
  again, twice as long each time up to six hours, and the pass takes the next
  due session behind it; a pass in which nothing answered doubles the pause
  before the next, up to an hour, and says so in the log. A page written
  resets both. The pacing lives in the server's memory, so a restart after
  fixing a key asks straight away
- **The install scripts try a download again when GitHub answers with a server
  error.** On the day 1.1.1 was released, both CI runs of the install job
  failed on Linux, macOS and Windows with `504 Gateway Timeout` from the
  release download, while `brew`, which retries, fetched the same files in the
  same runs. Both scripts now try a request that got no answer, a 408, a 429 or
  a 5xx seven times, waiting 1, 2, 4, 8, 16 and 32 seconds, and fail at once on
  anything else, such as a 404 for a release that does not exist. Five
  attempts over fifteen seconds were tried first and lost to an answer that
  stayed bad for over half a minute; `install.sh` also said only that it gave
  up, since an HTTP error is not a curl error, and now names the status.
  `install.sh` reads the status itself rather than passing `--retry` to curl:
  on macOS the 504 came back as a receive error (exit 56) that `--retry` does
  not count as transient. Both were run against a local server: answering 504
  twice, the file arrived on the third request; answering 503 always, both
  gave up after seven requests and about a minute; a 404 was asked for once
- **Hooks, the MCP registration and the service name the binary by the path it
  was run as**, when that path leads to the same file. They named the path
  `current_exe` reports, and on Linux that has every symlink resolved: an
  anamnesis installed by Homebrew and run as `…/bin/anamnesis` wrote
  `…/Cellar/anamnesis/<version>/bin/anamnesis` into every file it wired, the
  directory `brew upgrade` removes, leaving hooks that no longer start and
  settings that look right. A shim, a copy, or a name not on `PATH` still gets
  the path `current_exe` gives

## [1.1.1] - 2026-09-14

1.1.0's binaries started only on machines like the ones that built them: a
Linux with glibc 2.39, a Windows with the Visual C++ Redistributable. This
release's binaries start on RHEL 9, Ubuntu 22.04, Debian 12 and a Windows with
nothing installed, and the release checks that in the binaries rather than on
its runners.

The rest is getting from a downloaded binary to a memory that records, and
keeping it recording without anybody watching. The install scripts check the
archive and start the binary before replacing anything; `setup` wires a
project in one command and says which part is not done; `service` keeps the
server running at login; `key` and `settings.env` give that server the model
and embedder the shell has, without a wrapper script. When the embedder is
down after a reboot the server now starts without vectors instead of failing
every minute in silence, and `status` says why a server that does not answer
stopped. `redact` applies redaction rules added after something was captured
to what is already stored.

The index schema is unchanged at 18; nothing written by 1.1.0 or 1.0 needs
converting.

### Added
- **Install scripts**: `curl -fsSL …/install.sh | sh` on Linux and macOS,
  `irm …/install.ps1 | iex` on Windows. Installing from a release meant picking
  the archive for the machine, checking it against `SHA256SUMS` by hand — the
  step people skip — and choosing where the binary lives, which matters more
  than it looks because hooks, the MCP registration and the service all name it
  by its path. The scripts refuse an archive that does not match `SHA256SUMS`,
  start the new binary once before it replaces anything so a binary the machine
  cannot run leaves a working install alone, and replace an existing install
  where it is (on Windows by renaming the running `.exe` aside, since Windows
  will not overwrite it). `ANAMNESIS_VERSION` pins a tag and
  `ANAMNESIS_INSTALL_DIR` picks the directory. CI runs both scripts, twice, on
  Linux, macOS and Windows against the latest release
- **`anamnesis setup [--write]`** wires a project in one command. Getting
  memory to work took five commands in an order `GETTING_STARTED.md` spread
  over several hundred lines, and each had a way to be done wrongly that looked
  like being done. `setup` asks each part — the project's memory, every
  `--agent`'s hooks and MCP registration, a server the service manager keeps
  running, seed pages for an empty memory — whether it is done, and prints one
  line per part. A part is done only when the command behind it would change
  nothing, asked with that command's own inputs, so a hook pointing at another
  binary or a task running another port reads as not done. `--write` runs the
  rest with those same commands, then probes the server so the last line says
  whether an event would be recorded. It refuses `--write` from a cargo build
  directory, and names files from the project root when run from a
  subdirectory. `install-mcp` now carries a data directory that is not the
  default into the registration as `ANAMNESIS_DATA_DIR`: the hooks delivered to
  a server started with it, while the registered MCP server opened the default
- **`status` and `service status` say why nothing answers**, from what the
  server last wrote to `logs/`: the error a start failed with, a panic, the
  cause of a clean stop, or a log that ends with the server running — which is
  how a killed process and a machine that went down look from the inside.
  Both commands could tell that the port was silent and not why, while a
  scheduled task restarted a server that failed every minute; `doctor` already
  names a server running another build, so this line is only for one that is
  not running at all
- **`anamnesis redact [--apply]`** runs today's redaction rules over what is
  already stored. Capture redacts once, with the rules it has, and a rule added
  later never reached what came before: an AI Studio key typed into a prompt on
  2026-09-02, a day before the `AQ.` rule landed, was still in the raw spool, the
  index and every backup eleven days later. The command counts, per rule, the
  spool lines and index rows whose text would change — never the value — and
  `--apply` rewrites them: spool lines decoded and redacted string by string (a
  rule run over encoded JSON would eat its quotes), the file replaced whole and
  re-read if the server appended meanwhile, index rows in one transaction. The
  rows are not the only copy: an UPDATE leaves the freed bytes in place, and the
  index file keeps a rewritten row's old page until a checkpoint. Run on this
  project's live memory, the first `--apply` left every row clean and the key
  still in `anamnesis.db`. So the rewrite zeroes what it frees, and `--apply`
  checkpoints the index and then reads the file back, saying so if a reader
  kept an old page there. What is not rewritten is looked at before it is
  named. Wiki pages holding a match now, pages that held one in any commit
  git still keeps, and backups, archives and `anamnesis.db.*` copies are
  listed only if they still hold a credential. Those bytes are checked
  only with rules that recognise a credential by its own shape
  (`Redactor::credentials_in`), since the `key = value` rules misread encoded
  text and reported every archive, including one taken after the rewrite. A
  rewrite is recorded in the audit log as `memory.redacted`. `doctor` reports stored
  observations that still hold anything the rules mask, as a new `exposed`
  finding ranked above `broken`. Run over a restored copy of this project's
  memory: 11 records (not the 130 a first draft reported by counting rules that
  matched text they had already masked), 0 unmasked keys afterwards, and a
  second run finding nothing
- **`<data_dir>/settings.env`**, read by every command. A server started at
  login by the operating system gets an environment nobody chose, and on the
  machine this project is developed on the model and embedding settings had to
  be put there by a PowerShell wrapper, a VBScript and a second wrapper so the
  CLI saw the same model — none of it in the repository. `NAME=value` lines;
  the environment wins, variable by variable. Secrets (`*_API_KEY`,
  `ANAMNESIS_TOKEN`) and `ANAMNESIS_DATA_DIR` are refused by name, every refused
  line is named on stderr, `status --verbose` lists what the file set, and
  `serve` will not start while a line is refused
- **`anamnesis key set|list|forget`** keeps a model key in this account's
  credential store — Credential Manager on Windows, the Keychain on macOS —
  typed without an echo or read from `--stdin`, never taken as an argument.
  Every command reads it under the variable's name after the environment and
  `settings.env`, so a key set once is found by the server, the MCP server and
  the CLI. `list` names what is stored and never shows a value. Linux has no
  store that works without D-Bus and survives a reboot, so there the command
  says to use the environment. Server tokens stay environment-only: the hook
  reads its own on every tool call

- **`anamnesis service install|uninstall|status`** keeps the server running
  without a terminal: a scheduled task on Windows, a systemd user unit on
  Linux, a launchd agent on macOS. Dry run by default; `--write` registers,
  reads the registration back and starts the server. The Windows task carries
  every setting whose default has stopped this server before — logon plus a
  one-minute restart, `IgnoreNew`, no time limit, battery settings off — and
  runs `conhost.exe --headless`, so no window and no elevation where the only
  earlier way was a script host. Before registering it reports the model and
  vectors the service will start with, read from `settings.env` and the
  credential store rather than the shell, and names settings exported in the
  shell that the service will not see. Tried on Windows against a trial task:
  registered and read back, no visible window, the killed server back in 51
  seconds, `status` and `uninstall` as described. The first run found that
  `schtasks` cannot read a temporary file still held open, fixed before commit

### Changed
- **`serve` writes a panic to its log file.** The standard hook prints to
  stderr, which belongs to whatever started the process — a closed terminal, or
  a service manager that discards it — and on the machine this project runs on
  a PowerShell wrapper redirected stderr into a file for that reason alone. The
  message, the location and the thread now go to `logs/` first, and the standard
  hook still runs after. The hook is global, so a panic in a spawned task the
  server survives is written down too
- **Every command runs on a thread with a 16 MB stack.** In a debug build the
  entry function's frame holds every command's locals at once, Windows gives a
  main thread 1 MB, and adding one subcommand made every command — `--version`
  included — overflow before an argument was parsed. Release builds fit and CI
  never runs `main`, so nothing but running the debug binary would have shown it

### Fixed
- **Release binaries start on machines other than the one that built them.**
  v1.1.0's Linux binary was built on Ubuntu 24.04 and needed glibc 2.39, so the
  loader refused it on Ubuntu 22.04, Debian 12 and RHEL 9; its Windows binary
  imported `VCRUNTIME140.dll`, which Windows does not ship. Both passed the
  release's own check because the runners have the newest glibc and the
  Redistributable. Linux is now built on Ubuntu 22.04 and needs glibc 2.34, and
  Windows links the C runtime in. The release reads both requirements out of
  the binaries and fails when either comes back; the glibc floor is RHEL 9's
  2.34 rather than the build runner's 2.35, since a floor at the runner's
  version would have let a build that drops RHEL 9 through
- `anamnesis install-mcp` **carries a hosted embedder's settings into the
  registration**, and never its key. It used to carry none of them, on the
  grounds that a hosted endpoint wants a key — and the one this project
  recommends, nomic-embed-text in Ollama, wants none. The agent's server ran
  without the vector stream, and running `install-mcp --write` again to change
  anything else removed the variables somebody had added by hand. When a key
  is set, the line says it was left out and where it has to be instead
- **The MCP server starts when its embedding endpoint does not answer.** The
  embedder is probed at startup, and an Ollama not yet up after a reboot made
  the whole server fail — every memory tool gone for the session over the one
  retrieval stream that is allowed to be missing. It now starts, says so on
  stderr, and connects on the first query that needs a vector, trying again at
  most every 30 seconds
- **`serve` starts when its embedding endpoint does not answer, too**, and
  writes why it stopped to the log file. It refused, on the grounds that a
  refusal from a scheduled task is written where somebody looks. On this
  project's own machine, after a reboot with Ollama not started, it was
  written nowhere: the refusal came before the first log line, `conhost
  --headless` discarded stderr and reported success, and the one-minute
  restart tried and failed without a trace while 33 hook events queued. The
  server now starts without vectors, says so in the log, and connects when a
  page needs one; pages written meanwhile record the failure that `doctor`
  reports. The `starting` line now comes before anything that can fail, and
  any error that stops `serve` is logged as `serve stopped`
- **`write-page` no longer commits a page it then fails to index**, and
  `search`, `write-page` and `bootstrap` work while the embedding endpoint is
  down. `write-page` wrote the page into the wiki first and built the embedder
  after, so with Ollama not running it failed with the page committed and
  absent from the index — found again anywhere only by a watching server or
  `reindex`. The embedder is now built before anything is written, and the
  three commands take a hosted endpoint that does not answer the way the
  servers do: they say so, go on without vectors, and a page written meanwhile
  records the failure `doctor` reports. `reindex` and `reconsolidate` still
  refuse, since filling in vectors is much of what they are run for
- **`anamnesis reindex` rebuilds an index that is gone.** Pages were rebuilt
  before sessions, so into an empty database the first page a model wrote —
  which names the session behind it — failed a foreign key and the rebuild
  stopped there, the one case the command exists for. Sessions now come first,
  and a page naming a session nothing holds any more (forgotten, or never
  spooled) is indexed unlinked instead of failing. The report also counts a
  session filed under two dates once rather than twice

## [1.1.0] - 2026-09-13

What changed since 1.0 is mostly what a page is written *from*. A session page
used to be the model's reading of which tools ran; it now carries what they
returned, which calls failed on harnesses that never say so, what the agent said
it had done, what its subagents found, and — once the prompt budget stopped
throwing most of a long session away — says on the page how much of the session
it saw. A session can leave durable pages beside its own, decisions, gotchas
and procedures, linked to pages memory already holds. When the model does not
answer, a chain of fallbacks can write the page, and the page names the model
that did.

The rest is making faults visible. `status` says when the configured model has
stopped answering, `doctor` says why a memory is thinner than the work behind
it and when the server runs another build, and a page embedded from part of
itself says so. Retrieval is measured rather than argued: hit@1 and NDCG, a
label per kind of question, two suites frozen before their first run, paired
`--compare`, `explain` on `memory_query`, and questions asked of a copy of real
memory. Those measurements are why nomic-embed-text is the recommended
embedder where Ollama runs, and why the abstract stream and the quarter vector
weight ship switched off.

The promise 1.0 made holds: the index migrates itself on startup, from schema
11 to 18 in this release, and nothing written by 1.0 needs converting.

### Added
- `anamnesis eval --pages-from <archive or data dir>` **asks a questions file
  of a copy of real memory**. The wiki pages of a `backup` archive, or of a data
  directory read as files only, become the corpus, built in a throwaway
  directory as always; the questions file carries no pages and each case is
  validated against the copy. Frontmatter status comes along, so a superseded
  page stays out of the answers. `--scope` picks a project when the copy holds
  more than one. The frozen live set from 2026-09-13, re-run against that day's
  backup without exporting a single page, reproduced its no-vectors row exactly
  (hit@1 0.833, MRR 0.910, NDCG@5 0.933). Suites may now give a page a
  `status`
- **A chain of fallback models** — `ANAMNESIS_LLM_FALLBACK_PROVIDERS`, a
  comma-separated list of `provider[:model]` asked in order when the configured
  model fails transiently. On 2026-09-07 an afternoon of `503 high demand` and
  on 2026-09-13 a spent daily quota each turned a day's sessions into counted
  pages, while another model was one request away. Only a timeout, a refused
  connection, a rate limit, a server fault or an unparseable reply is handed
  on, after the link's own retries; a bad key, an unknown model, a refusal and
  a reply too long for its budget stop where they happen. A link to another
  backend takes that backend's own key and never `ANAMNESIS_LLM_API_KEY`. A
  page a fallback wrote ends with `Written by <model>, standing in for <model>,
  which did not answer.`, and its session is recorded against the model that
  wrote it. `serve` prints the whole chain; `reconsolidate` and `abstracts`
  deliberately do not use it, since one replaces pages that usually had a good
  one and the other is measured as a single writer's
- **Embedding models with a longer window were measured against MiniLM**, over
  all four suites at the shipped tuning. `nomic-embed-text` through Ollama is
  the first vector stream to beat no vectors on `long` (hit@1 / MRR 0.500 /
  0.578 against 0.312 / 0.414 shipping and 0.500 / 0.562 without vectors) and
  takes `crowded` and `adversarial` to 1.000, at full weight; gte-small,
  gte-base and bge-small read every page whole too and stay below no vectors
  on `long`. Every longer model costs `retrieval` its `sqlite` keyword (first
  to second). Nothing ships from this: the default stays MiniLM, and the
  candidate waits on questions nobody has scored. `docs/DIRECTION.md` has the
  table, `docs/measurements/2026-09-13-embedding-models.md` how to run it again
- **The abstract stream was measured, and stays at weight zero.** `long` was
  given abstracts by two writers that were shown each page and never the
  questions — gemini-3.6-flash, and qwen2.5:7b-instruct run locally through
  Ollama — and neither gained: `abstracts=1` scored hit@1 / MRR 0.312 / 0.417
  and 0.250 / 0.417 against 0.312 / 0.414, and replacing body vectors with
  abstracts scored below removing vectors outright under both. The two sets
  are kept as suite copies in `docs/measurements/` so the run can be repeated;
  the frozen `long.toml` was not edited. The same session measured the vector
  weight across all four suites: at 0.25 nothing lost ground anywhere,
  `adversarial` reached 1.000 / 1.000 and `long` 0.438 / 0.521 — and it is not
  shipped, because that grid was read off the two suites whose rule is that no
  knob is tuned against them. `docs/DIRECTION.md` has both tables and what
  comes next: a longer embedding window, then the weight under it, checked on
  questions nobody has scored
- **`anamnesis abstracts <suite.toml> [--write]`** gives an eval suite's pages
  abstracts written by the configured model, which is sent each page's title
  and body and nothing else. A suite file holds its questions beside its pages,
  so an abstract written by hand is written by someone who has read what will
  be asked; asking page by page keeps the questions out of the one place the
  line is made. The reply is checked before it is kept — one line, at most 40
  words, not a heading, a bullet, the title again, or an opening about the page
  ("This page details…", which eight of eight did on the first run) — and a
  page that is refused is left without one, so a run cut short is finished by
  running it again. A rate limit or an overloaded model stops the run and names
  the pages not asked, instead of spending a request per page to hear it again;
  `--pace` spaces requests for a per-minute limit. The key lands after `title`,
  and comments and questions are left as they were
- A page can carry an **`abstract:`** — one line saying what it is about — and
  that line gets a vector of its own (`V18`, `page_abstract_embeddings`), at
  write time and on `reindex`, whatever became of the body's. It feeds a fifth
  retrieval stream, `Tuning::abstracts`, which **ships at weight zero** and is
  not run at all until given weight: nothing has measured it here yet. The idea
  is ai-memory's #672, and the key is theirs so a page carried between the two
  keeps it — but in ai-memory the consolidator writes `summary:` while the
  stream reads `abstract:`, so the pages it writes never reach the stream; here
  the field is `page_abstract` in the code and `abstract` in the file, one name.
  A blank abstract gets no vector (the empty string is as close to every
  question as to any), and a page rewritten without one loses the vector
  rather than keeping a line it no longer says. Eval suites take `abstract =`
  on a page, `--compare abstracts=1` gives the stream weight, `eval --embed`
  reports how many pages had an abstract to rank, and `memory_query`'s explain
  shows the stream's rank beside the other four. Nothing writes abstracts yet;
  that and the measurement over `long` are the next two steps
- A page **longer than the embedding model's window is also embedded in
  sections** the model reads whole. `V17` gives `page_embeddings` a `part`:
  every existing row becomes part 0, the page as one text, unchanged, and a long
  page gains parts 1 and up — cut at headings (not ones inside a code fence),
  packed by paragraph, split by line, sentence, word and finally character where
  a paragraph will not fit, each carrying the page title and its heading so a
  piece from the middle still says where it is from. Every word of the body is in
  some section, in order, which a property test holds. What fits is asked of the
  embedder's own tokenizer. Sections are all or none: one that fails to embed,
  or a page needing more than 64, leaves the page with its whole-page vector and
  truncation row as before, and a page edited down to fit loses them rather than
  answering for words it no longer has. `anamnesis reindex` gives sections to
  long pages embedded before this. Whether a query uses them is a new tuning,
  `vector_sections`, which **ships off**, because `anamnesis eval --embed
  --compare vector_sections=1` found a trade: `long` rose from MRR 0.414 to
  0.469 at the same hit@1, five questions better and three worse — one of them
  the guard `reset cause`, where the best of a long page's many vectors beat the
  short page that answers — and the three short-page suites did not move.
  `docs/DIRECTION.md` records what that says about the next attempt. `eval
  --embed` now reports how many truncated pages were sectioned, so a comparison
  that moved nothing can be told apart from one that had nothing to move, and
  `--compare`'s help lists `vector_coverage` and `vector_sections`, which it
  accepted without saying so
- A page **longer than the model that embedded it** now says so, in a row and in
  `anamnesis doctor`. A model with a fixed window does not refuse a long page —
  it embeds as much as it can reach and returns an ordinary vector, and nothing
  about that vector says it stands for part of a page. So the page was in the
  wiki, in the index, whole in full-text, entity and link retrieval, and
  answering with a fraction of itself in the fourth stream, with no way to find
  that out. The two embedders did not even agree about it: the hosted one sends
  the text whole, the endpoint answers 400, and that already landed as a failure
  row — so switching provider silently changed whether an over-long page was
  reported at all. `V16` adds `kind` to `page_embed_failures` (`failed` or
  `truncated`) plus the `tokens` and `budget` the page met, and widens that
  table's invariant deliberately: it used to be the strict negative of
  `page_embeddings`, and a truncated page has a vector *and* a complaint. One
  sentence still covers it — a row means this page's vector is missing or
  incomplete. `doctor` reports the two separately, because they share no remedy:
  a missing vector is `broken`, a partial one is `thin`, and the second verdict
  leads with the worst page rather than the first, since the question being
  asked is how bad this gets. The count is taken from the embedder rather than
  estimated by the writer — only a tokenizer knows what a tokenizer will do, and
  a characters-over-three rule would under-report exactly the pages of file
  paths and identifiers this project writes
- **The truncation window was documented as 512 tokens and is actually 128.**
  `config.json` puts `max_position_embeddings` at 512, and the clamp in
  `embed.rs` compares against it — but `tokenizer.json` carries its own
  `truncation.max_length` of 128 and `encode` applies it first, so that clamp
  has never fired for the default model. Found by running the new report against
  this project's wiki and getting zero, which was the wrong answer: the first
  implementation counted with the embedding tokenizer, which truncates and then
  reports the truncated length, so the comparison could never be true. Counting
  with an untruncated copy instead, the real figure is **43 of 49 pages**, the
  longest embedded from about **8%** of itself, and the two most authoritative
  pages in the corpus both under 12%. `docs/DIRECTION.md` argued for a fifth
  retrieval stream on the strength of the wrong number; it now argues from the
  right one, which is four times stronger
- `anamnesis eval --embed` now says **how much of the corpus the vectors read**:
  `Vectors 22 of 22 pages read whole by …`, the least-read page when any was
  truncated, and — when none was — that the suite cannot measure anything about
  how long pages are embedded. It is the report the previous entry needed and
  did not have. Run against the real model, **all fifty pages in the three
  shipped suites fit its window**, against 43 of 49 in this project's wiki: a
  change to how long pages are embedded (an abstract, chunking, a longer model)
  would have scored identically before and after on every suite here, and read
  as a change that did nothing. Read back from the rows indexing wrote rather
  than counted again, so what is reported is what retrieval ran on
- A fourth built-in eval suite, **`long`**, whose answers sit where the
  embedding model does not read: sixteen questions over seventeen pages, frozen
  in its own commit before it was scored once. Twelve questions are deep — the
  answer is a long page, and a test holds each to naming nothing that appears
  in the first 128 words of that page, which is stricter than the first 128
  tokens. Four are guards, where a short page answers and a long one repeats its
  words further down, so a change that helps long pages has to show what it
  costs. The first run, without vectors: hit@1 0.500, MRR 0.562, every keyword
  question first and no paraphrase first. **With vectors it scores lower**
  (hit@1 0.312, MRR 0.414), and `--compare vectors=0` improves six questions and
  costs none: on pages like these the truncated vector scores an opening that is
  about something else, and at weight 1.0 it outvotes the full-text stream that
  had the answers
- `memory_query` with `explain` reports **how much of a page its vector
  stood for**: a `coverage` beside the vector stream's rank, `1.0` for a page
  the model read whole and less for one it truncated. A vector rank of one on a
  page embedded from a quarter of itself is a different fact from the same rank
  on a page read in full, and the working said nothing to tell them apart. The
  same share is behind a new tuning, `vector_coverage`, which scales each
  page's vector contribution by it — and which ships at `0`, because
  measuring it said to. Under `anamnesis eval --embed --compare
  vector_coverage=1`, the `long` suite fell from hit@1 0.312 to 0.188 with seven
  questions worse and none better, every one of them answered by a long page:
  a partial vector was supporting its own page, not undermining it. What the
  suite loses to vectors is the full-weight vote for short pages read whole,
  and that is recorded in `docs/DIRECTION.md` as what the next attempt has to
  address
- `anamnesis eval --compare rrf_k=5,links=0.5` scores what ships against a
  variant and **names every question that moved**. `docs/DIRECTION.md` adopts
  the rule that a retrieval change without a paired measurement does not land,
  and until now there was nothing to measure one with. `--sweep` ranks sixty
  settings by a mean, and a mean is exactly where a trade hides: a change that
  lifts three questions and drops two reports as a small gain and reads
  identically to one that lifted five and dropped none — those are not the same
  change and only one of them should ship. So the output is not a pair of
  numbers. Improved, regressed and unchanged are counted separately, the
  regressions are printed whether or not anybody asked for them (the
  improvements need `--verbose`, because a change is argued for by what it
  cost), and each moved question shows where it went — `1 → 3 [symptom] test
  passes locally fails in ci`. With `--check` a variant that loses ground on any
  question exits non-zero, so the rule can be a CI step rather than a habit.
  Nothing relevant coming back is treated as **worse** than any rank rather than
  better than all of them: Rust orders `None` below `Some`, and a comparison
  that leaned on that would have recorded every disappearance as a win. The
  variant is read off the command line and nowhere else — `Tuning` documents
  that nothing loads it from configuration, because a knob set per project and
  measured by nobody is the class of setting this codebase keeps deleting, so a
  variant lives for one command and then either becomes a default in code or
  does not
- `anamnesis eval --k-sensitivity` scores every suite **once per `rrf_k`** and
  reports what moved. `--sweep` answers which setting scores best; this answers
  the question that has to come first and had never been asked here — whether
  these corpora can tell the settings apart at all. A score identical at `k = 1`
  and `k = 60` would not mean the value between them is right, it would mean
  nothing here can see the difference, and a number measured where the question
  does not live reads exactly like one measured where it does. Beside the table
  it prints the count that settles it: how many questions changed their **first
  answer** anywhere in the grid, and how many changed their ordering at all.
  **It was built expecting to find that the corpora are blind to `k`, and found
  the opposite.** Every shipped suite discriminates sharply — hit@1 moves 0.300
  across the grid on `retrieval`, 0.375 on `adversarial`, 0.533 on `crowded`,
  and ten of `crowded`'s fifteen questions change their first answer somewhere
  in it. The low `k` this project ships is not an artifact of a corpus too small
  to object; these corpora object loudly and they agree. Two findings that were
  not what it was looking for: `k = 1` and `k = 2` are indistinguishable on all
  three suites to three decimals, so what is measured is "≤ 2" and the 2 is a
  choice inside that rather than a result; and the penalty for a high `k` *grows*
  with the corpus, which runs opposite to the worry that prompted the work —
  though `crowded` is crowded by construction, so that may be about
  confusability rather than size, and telling those apart still wants a bigger
  corpus. A test landed with it: nothing in the grid may beat what ships, on any
  shipped suite. `docs/DIRECTION.md` has been corrected where it asserted the
  opposite
- The consolidation schema can no longer **ask the model for something nothing
  reads**. This is ai-memory's #667 turned into a test instead of a note: they
  asked their model for typed relations in the prompt, had no field on the
  struct to hold them, and dropped every edge before the wiki write — silently,
  because a request that succeeds while losing what it asked for is
  indistinguishable from a request that succeeded. The same fault is available
  here and would be quieter, because replies are read field by field with
  `value.get(name)` rather than deserialized into a struct, so *nothing at all*
  fails when a schema property has no reader. The test asserts the property that
  would have caught it — every field the schema declares must be load-bearing —
  by removing each declared field in turn and requiring the outcome to change,
  either by being refused or by producing a different digest. A field whose
  absence nothing notices is a field nothing reads. It was verified the only way
  such a test can be: by adding a `relations` property that nothing reads and
  confirming it goes red. Two more hold the surrounding shape — the reduced
  schema used when a session will not fit keeps the same property, and is
  checked to be the full schema minus exactly `notes`, since a field spelled one
  way in one and another way in the other is a request the model answers under a
  name the reader never looks for
- A page whose embedding failed **says so**, in a table and in `anamnesis
  doctor`, instead of in a log line nobody reads on the day it is written. The
  trade itself was right and is unchanged: a failed embedding costs the page one
  retrieval stream, and refusing the write would cost the page. What was wrong
  was the reporting. The page lands in the wiki, in the index, and in full-text,
  entity and link retrieval, and is silently missing from the vector stream —
  everything looks healthy while that stream is quietly smaller than the corpus,
  which is precisely the shape of fault this project exists to make loud. `V15`
  adds `page_embed_failures`, keyed `(page_id, model)` to match
  `page_embeddings`, holding when the last attempt failed and what it said.
  Keyed by model because a page can hold a good vector under one embedder and
  have failed under another, and reporting the first as broken would be a new
  wrong answer. It is the negative of `page_embeddings` rather than a log of
  everything that ever went wrong: `embed_page` writes a row or deletes one and
  never both, so a page that gets its vector stops being reported, and
  `anamnesis reindex` is what clears the table by retrying. The reason is stored
  alongside the count because the remedy differs completely between a model that
  would not load and a page that would not fit — `doctor` says which, naming the
  page and the model, and reads differently when every failure shares one error
  (a broken embedder) from when they do not (more likely the pages). Silent when
  there is nothing wrong: an embedder is opt-in, and telling somebody who is not
  embedding that zero pages failed to embed is the kind of line that teaches
  people to skim
- `memory_query` takes **`explain`**, and answers with the working behind each
  score: which of the four streams found the page, where it ranked in each, what
  that rank contributed, and the standing multiplier applied afterwards. The
  numbers were already being computed — `StreamBreakdown` has existed since the
  streams did — and were visible only from inside the process, which is the
  wrong place for them. Every weight in the fusion was settled by an argument
  and then by one sweep over twenty-five questions, and none of those arguments
  can be re-examined from a fused list, where a page three streams agreed on and
  a page one stream liked look identical. This is the field that tells them
  apart. A stream that missed reports no rank rather than a rank of zero,
  because "full text never found this" and "full text found it last" are
  different facts and only the first one indicts a weight. Scoring is two
  stages and the field says which one it is reporting: within a scope the four
  streams are fused by weighted RRF and multiplied by standing, and *then* this
  project's ranking and the shared scope's are fused again by plain RRF — so
  `within_scope` is deliberately not the `score` beside it, and is the number a
  weight argument is actually about. The explain pass re-runs the streams
  through `Store::query_streams`, which records no access: asking which stream
  would have found a page is not the same as handing the page over, and the
  decay sweep reads those counters to decide what to keep. Off by default, and
  absent from the response rather than null when it is off
- `anamnesis-store` exports `StreamBreakdown`. `Store::query_streams` is public
  and returns one, so until now a caller outside the crate could make the call
  and had no way to name what came back
- A sixth MCP tool, **`memory_read_page`**, reads one page whole. `memory_query`
  returns a snippet, which is exactly enough to decide *which* page is the right
  one and not enough to act on it — so an agent that had already found the page
  it needed was left working from three sentences, and the only repair available
  to it was to query again with narrower words and hope for a better slice. That
  asks retrieval to do a job reading does, and it fails in the direction nobody
  checks: the agent gets a plausible answer built on a partial page. The tool
  takes the path a hit reported and returns the body untruncated, along with the
  frontmatter that qualifies it — status especially, because a
  `do-not-answer-from` page is evidence about what was once believed rather than
  about what is true, and a body handed over without that is worse than no body.
  This project's scope is searched first and the workspace's shared `_global`
  scope second, so a path copied straight out of a hit resolves whichever scope
  it came from without the caller having to say which; where both hold the same
  path the project's own page wins, specificity being the reason the two scopes
  are separate at all. Reading renews a page against the decay sweep exactly as
  a query that surfaced it does — opening a page and acting on it is use, and
  the sweep's whole question is what is still being used
- A third eval corpus, `adversarial`, and the first one **frozen before it was
  scored**: eighteen pages of an identity service and sixteen questions, each
  written so that some *other* page is the better literal match — more of the
  query's words, more authority, or both — with the trap it tests named in its
  note. `retrieval` asks whether an answer is reachable and `crowded` whether it
  wins against plausible company; neither can ask what retrieval does when the
  obvious match is the wrong page. It shares no vocabulary with the other two on
  purpose, so a knob that happens to suit queues and deploys has nothing to lean
  on here. Two rules govern it, both stricter than `crowded`'s: the corpus and
  the questions were fixed before the suite was run once, and no sweep is ever
  run against it — `crowded` was written to be the set retrieval is not tuned on
  and then had a sweep run over it anyway, which was sound reasoning and still
  cost it some of its independence. A case that fails here is a finding about
  retrieval, not a threshold to lower. The first run scored **hit@1 0.938 / MRR
  0.969 / NDCG@5 0.977 / recall 1.000**: every answer inside five, and exactly
  one of the sixteen second — `argon2id`, beaten by the session page that
  carried out the migration and says the word twice as often as the decision
  that chose it. That is precisely the trap the case was written for, and the
  case has not been edited since. By category the whole shortfall is `keyword`
  (0.800), which is the opposite of `crowded`, where the whole shortfall is
  `paraphrase` — two corpora disagreeing about which kind of question is hard
  says more than either figure on its own. All three suites are built into the
  binary, so `anamnesis eval` with no `--suite` now scores forty-one questions
  over fifty pages, and each is run as an ordinary unit test against the bar it
  sets for itself.
- An eval case says what **kind of question** it is, and every measure is
  reported per kind as well as in total. A total is where a trade goes to hide:
  a change that teaches retrieval to match a paraphrase can cost it a bare
  keyword, and one mean over both reports that nothing much happened, which is
  exactly the change worth arguing about. The label describes how somebody
  *phrased* the question — `keyword`, `natural`, `paraphrase`, `symptom`,
  `temporal` — not what the corpus is doing to the ranker, because the point is
  to see which kind of asking a change helps. All twenty-five shipped cases are
  labelled, and the table said something on the first run: the whole of
  `crowded`'s shortfall is one category. Keyword 1.000, symptom 1.000, natural
  and temporal 1.000, and **paraphrase hit@1 0.750** — the lexical-gap questions
  are the only ones retrieval is getting wrong, which is the finding the single
  0.933 was averaging away. A suite declares its categories up front and a case
  naming one it did not declare is refused, so that `paraprase` is a suite that
  fails to load rather than a sixth category with one question in it; a declared
  category no case asks is refused too, being the same typo in the other
  direction. The count sits in its own column beside the rates, because over
  four questions a rate moves in quarters and a quarter is not a finding. A
  suite that labels nothing keeps loading and is scored exactly as it was
- `anamnesis eval` reports **hit@1** and **NDCG@k** beside the mean reciprocal
  rank and recall it already had, and a suite can be gated on either. The
  shipped suites were reading 1.000 / 1.000 and 0.967 / 1.000, which looks like
  good news and is a saturated instrument: a measure at its ceiling cannot
  report an improvement, and with twenty-five questions it can barely report a
  regression. Hit@1 is the number with somewhere to go — `crowded` scores 0.933,
  because one question in fifteen is answered second, and second is not what the
  agent is handed. The NDCG is the number another project's published figure can
  be held against, which is most of the reason to have it; ours would otherwise
  only ever be comparable to our own past. It is single-relevance and says so: a
  case's `relevant` list is a set of *acceptable* answers, any one of which
  settles the question, so the ideal ranking is one page in first place and the
  textbook form — accumulating gain over every page the case listed — would
  report five of the twenty-five shipped cases as partial failures for returning
  exactly what was asked. It prints as `NDCG@5`, over the window the suite
  actually scores, because a gain quoted without its `k` is the kind of figure
  that gets compared to somebody else's by mistake. `[thresholds]` takes
  `min_hit1` and `min_ndcg`; a suite written before they existed keeps parsing
  and keeps being gated on exactly the measures it named, since a new number
  that retroactively failed somebody's checked-in suite would be a poor way to
  introduce itself
- What a subagent found is now in memory. A subagent is a whole session inside
  one tool call — an investigation that reads thirty files and hands back three
  lines — and the parent recorded the call and the prompt that started it and
  nothing of the finding, which is the only part that outlives the call.
  `SubagentStop` carries the report along with `agent_type` and `agent_id`,
  verified from a live payload. It is recorded as its own kind, the page quotes
  the reports under a heading of their own, and the model is told that the
  calls behind a report are not in its transcript. `SubagentStart` is
  deliberately not registered: it carries nothing the parent's own tool call
  does not already have. The subagent's kind and identity are read only on the
  report event, because a harness stamps `agent_id` and `agent_type` on every
  event that happens inside a subagent — reading them wherever they appear
  would file a `Bash` call made by an Explore subagent as a call to `Explore`,
  and every tool tally of a session that used one would be wrong in a way that
  looks entirely plausible. Also lands the model-facing rule for #170, which
  was written against a line that had already been reflowed and never applied
- A session now records what the agent said it did. Claude Code's `Stop` event
  carries `last_assistant_message` — the agent's own account of the turn it
  just finished, verified from a live payload rather than from documentation —
  and it is the only text in a session that says in words what happened. A
  transcript of tool calls cannot reconstruct it. `install-hooks` registers the
  moment, it is recorded as its own kind, the counted page quotes the last
  turns under a heading of their own, and the handoff carries the closing one,
  clipped, because a sentence of "here is where this was left" is worth more to
  a session starting with nothing than any tally. The model is told what those
  lines are and told to prefer the tools where the two disagree: an account
  written before a command failed is still what was believed at the time. A
  turn that ended with the agent saying nothing records nothing — a row holding
  an empty string costs a line in every transcript a model is later asked to
  read
- `anamnesis lint` says which pages are not worth what they cost to keep.
  `doctor` judges the machinery; this judges the output, and the two fail
  independently — capture can be perfect and the wiki still full of pages that
  say nothing. Four rules, each of which fires on this project's own wiki or
  came from a failure it has had: a session page thin against the session
  behind it (measured in characters per recorded event, which is the original
  complaint stated as a number), a page too short to be anything at all,
  two pages claiming one title, and an episodic page a month old that
  retrieval has never once handed to anybody. The ratio applies only to the
  page that *is* the account of a session: run without that restriction it
  fired six times here and was right once, because a gotcha is one claim and
  judging it against the length of the afternoon that produced it reports
  every good one as thin
- A build says which commit it is, and `doctor` notices when the server is not
  running it. `1.0.0` is the same string across every commit of a release
  cycle, so a server started three weeks ago and a binary compiled a minute ago
  were indistinguishable from outside — which is exactly the state this project
  spent weeks in, with hooks calling an executable that predated the code meant
  to record what tools return and nothing anywhere saying so. `anamnesis
  --version` now prints `1.0.0 (a40902a)`, with a trailing `+` when the tree
  had uncommitted changes, and the server answers `/version` with the same.
  `doctor` compares them. A server that answers liveness but not `/version` is
  not a missing answer either — the endpoint has existed since builds were
  stamped, so silence there means the server predates it. The stamp degrades to
  `unknown` rather than failing the build: a source archive, a vendored copy,
  or a Docker context without `.git` is a legitimate build, and refusing to
  compile it to protect a diagnostic would be the tail wagging the dog
- `anamnesis doctor` says why a memory is thinner than the work that went into
  it. `status` answers whether work is being recorded, and answers it well —
  but recording can be working perfectly while the pages are worth little, and
  none of the reasons look like failures. A harness wired for four moments of
  five captures sessions with holes in them. A harness that never states an
  outcome makes every tally count successes only. A hook binary older than the
  build that records what a tool returned records what it knew how to record.
  Each of those looks exactly like a quiet week. The command reads the wired
  hooks through the parser's own classifier, the last twenty sessions from the
  index, and what actually wrote their pages, then names what it found, what it
  means, and the command that fixes it. Judged from the pages rather than from
  the terminal's environment, because the model lives in the server's — a
  provider exported in this shell says nothing about the process that
  consolidates, and its absence says nothing either. Run against this project's
  own setup it found three real things on the first try
- A page now says what a session **changed**, apart from what it merely read.
  `Files mentioned` was one list of everything a path pattern found anywhere in
  a session, so a session that read forty files and edited two made
  forty-two claims about what it was about, of which two were true. Files
  written by a writing tool — `Write`, `Edit` and the names other harnesses
  give the same two moments, matched through an MCP prefix as well — now get
  their own heading, the first entity slots, and the line in the handoff, and
  the mentioned list holds only what is left. `Bash` is deliberately not a
  writing tool: it is by far the most used one here, it can obviously change a
  file, and working out *which* file from the text of a shell command means
  parsing every shell — a wrong answer is worse than none on a heading that
  says "this changed". The file comes from the tool's own declared field rather
  than from the body at large, because an edit carries the text it replaced and
  that text names files the edit never touched; and a call that never came back
  changes nothing, since nobody observed that it wrote
- A call that failed is now on the page, on a harness that never says one did.
  Claude Code fires no `PostToolUse` hook at all for a failed tool call —
  probed directly: one session running `echo AAA`, `exit 3`, `echo BBB`
  produced payloads for the first and the third and nothing for the second — so
  a failure was not an unflagged event in the record, it was missing from it,
  and every tool tally on every page counted successes only. `install-hooks`
  now registers the pre-tool moment as well, an attempt is its own event kind,
  and an attempt whose completion never arrives is reported as a call that
  never came back. Paired by the harness's own `tool_use_id` where there is
  one and by tool name in order where there is not, so three attempts and two
  completions is one unfinished call rather than three. The tool counts are
  taken from completions alone, because a pre hook and a post hook are one call
  seen twice and counting both would have doubled every number on the page the
  day the hook was registered. The transcript sent to a model drops an attempt
  that completed for the same reason, and marks the ones that did not
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
- **nomic-embed-text through Ollama is the recommended embedder where Ollama
  runs**, and the first example `GETTING_STARTED.md` gives of an embedding
  endpoint. On a question set frozen over this project's live memory before
  its first run it scored hit@1 0.875 / MRR 0.931 against MiniLM's 0.833 /
  0.903, every difference in paraphrase and none on keyword or natural
  questions, and read all 56 pages whole where MiniLM truncated 50. The default
  stays the built-in MiniLM, because nomic needs a second server running and
  the vector stream is lost the day it is not. The section says so, and that
  the MCP registration needs the same three variables by hand, since
  `install-mcp` writes no hosted embedder into a harness's config.
  `docs/DIRECTION.md` records the table and why the quarter weight is not
  confirmed
- The fallback chain's documentation says **every link is sent the prompt the
  configured input budget built**. A local fallback with a smaller window than
  that budget has the prompt cut by Ollama without a refusal, and writes a page
  from part of the session that does not say so; on the machine this was found
  on, the local model's window was 12,288 tokens against a 64,000-token budget
- **`target/` no longer grows with every edit.** It reached 27.6 GB on this
  machine with dependency debug info already off, and 17.7 GB of that was
  `target/debug/incremental`: 365 cache directories, one per distinct build of
  a crate, none of them ever removed. The dev profile now builds this
  workspace's crates with `debug = "line-tables-only"` and without incremental
  compilation. Measured from clean — test build, clippy, then one edit and the
  test build again — the tree is 1.88 GB where it was 4.09 GB, one edit adds
  nothing where it added 0.74 GB, and the clean build takes 66 s where it took
  77 s. The cost is about three seconds on a rebuild after an edit to a crate
  most of the workspace depends on; backtraces keep their files and lines. The
  measurements and the one line to revert are in `Cargo.toml`
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
- A model reply whose **letters came back damaged is asked for again**. Two
  Turkish pages in this project's memory were written by Gemini flash models
  from transcripts whose Turkish was intact, and hold not one `ş`, `ğ`, `ü`,
  `ö` or `ç`: in their place `ő`, `đ`, `œ`, and once a C1 control character —
  the first UTF-8 byte of each letter kept, the second one wrong. The JSON was
  valid and every field filled, so the pages were written as the model's, and
  no search typed with the proper letters finds them. A reply is now counted for
  letters the transcript never held, in either case, that share a first byte
  with one it did; at three or more and a tenth of the reply's non-ASCII
  letters, the same request is sent once more and the reply with fewer is kept.
  Over the 31 session pages in that memory, rendered again with
  `reconsolidate --show-prompt`, the two damaged ones scored 47 and 25 and
  every other one 0. Asking again never costs the page: a reply that trips the
  check honestly is kept
- `anamnesis doctor` **judges only the embedding model the server uses**. The
  index keeps every model's vectors and complaint rows, so a machine that
  moved from MiniLM to another embedder still holds MiniLM's truncations, and
  doctor reported them as the running system's: on the install that moved to
  nomic-embed-text, every page had a whole vector and doctor still said four
  were "embedded from 128 tokens". It now asks the server which model it
  embeds with (`/whoami`, the `Vectors:` line in `status`) and reads only that
  model's rows; when nothing answers, it judges every row as before
- `anamnesis doctor` **tells a truncated page with sections from one without**.
  Since sections, its count of pages embedded from their opening could not say
  which of them `anamnesis reindex` would change, and its remedy still named
  only a longer window or shorter pages. On the machine it was found on, four
  pages were reported alike: one written after sections already held 51, three
  written before held none. The finding now says how many hold sections — which
  retrieval as it ships does not compare, so those stay thin — and how many do
  not, with `reindex` as the remedy for those and the 64-section limit a rebuild
  cannot get past. Whether sections are compared is read from the shipped
  tuning, so a page that has them stops being reported the day they are
- A page written through `memory_write_page` is embedded **once**. The tool
  called `index_page`, which embeds the page and records a failure or a
  truncation, and then embedded the same text again in a block that recorded
  nothing. Every write paid the model twice, and when the first attempt failed
  and the second succeeded the page kept a vector beside the complaint that it
  had none — which `anamnesis doctor` reports as broken. No other writer had
  the second call
- **An anchored `ignore_paths` pattern could silently exclude nothing** when
  the project's path held a letter whose lowercase is a different number of
  bytes. The path was compared with the root lowercased and then cut at the
  byte length of the root as written: under a directory named `STRAẞE` a
  reported `straße/target/…` came out as `arget/…`, `target/**` matched
  nothing, and the events it was written to keep out of memory went in. The
  same cut landing inside a character — a Kelvin sign in the root, a `ğ` in the
  path — was a panic in the hook. The cut is now found by walking the path
  itself
- `anamnesis restore` refuses an archive holding a link, a device or a pipe,
  and checks every entry before it writes the first. `backup` follows links
  and writes nothing but files and directories, so such an entry was put there
  by something else, and a symbolic link unpacked early turns a later
  ordinary name into a write through it. Checking up front also ends a refusal
  halfway through leaving half an archive in the data directory
- A query of 20,000 distinct words no longer fails. Each word was two bound
  parameters in the entity stream, SQLite refused the statement, and the error
  carried all 80 KB of it back to the caller. Queries are now searched for
  their first 128 distinct words; 5,000 had taken 181 ms, 300 take 4
- A negative token count in `page_embed_failures` is read as no count rather
  than cast to eighteen quintillion
- `anamnesis eval --streams` no longer reports questions **nothing** answered as
  questions **fusion** answered. Its one list — "No single stream answered these
  — fusion is doing the work" — held every question no stream found within the
  suite's window, and never asked whether the fused ranking had found them
  either. Every question in the older suites is answered, so the list only ever
  held fusion's real successes and the fault never showed; on a suite with
  misses it printed all of them under the heading that fusion was answering
  them. The questions are now split three ways: no stream and fusion did, some
  stream did and fusion **lost it**, and nothing did. The middle one is new and
  the most useful: the answer was in hand and the weighing of streams dropped
  it, which is exactly the fault the 2026-08-29 sweep was run to fix, and until
  now nothing named the question it happened to
- An `error` field that is present and empty is no longer read as a failure. A
  JSON-RPC reply — which is what a tool answering over MCP sends — carries
  `error: null` on every successful call, and the outcome reader treated the
  key's presence as the answer. No such payload is in this project's index yet;
  the shape gets more likely with every tool that arrives over MCP, and a
  wrongly recorded failure is the same lie as a wrongly recorded success.
  `doctor` also notices a harness wired before the assistant's own account was
  captured, which looks complete and produces pages compiled from tool calls
  alone — and a test now fails if any of its verdicts carries the indentation
  of the source line it was written on, which had happened three times
- A Turkish page is written in Turkish letters. The prompt asked for the
  language the person wrote in and got it — `Ozet`, `gorev`, `Gerceklestirilen`
  — a page in no language at all, and unfindable by anybody who types the word
  properly. The rule now names the alphabet as well as the language. Two more
  rules follow the results that tool lines now carry: read what came back after
  the `→` rather than describing what was run, and quote the error a tool
  printed rather than summarising it away. And a caution the counted page has
  had since the outcome work: `(NO RESULT)` marks a call that never came back,
  and on a harness that reports no outcomes at all, nothing being marked means
  nothing
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

### Security
- **A web page could write to memory.** The server asks nobody for a token by
  default, on the reasoning that loopback is the boundary — but every page the
  person running it opens can send requests to `127.0.0.1:8080` too. A `POST`
  with a `text/plain` body goes out without the browser asking first, and
  `/hook` read its body as a string whatever the type, so a page on any site
  could record a prompt, which consolidation turns into a page and the next
  session is handed. An `<img>` pointing at `/handoff` spent the note the next
  session was owed. And a page whose domain was rebound to `127.0.0.1` could
  read `/api/v1` and `/ui`, which is the whole of memory. Every route but
  `/health` and `/version` now refuses a request a browser marks as coming
  from another site (`Sec-Fetch-Site`, or `Origin` where that is missing),
  except a person following a link to the wiki browser; and a loopback server
  with no tokens refuses any `Host` that is not a loopback name. Nothing that
  is not a browser sends those headers, so hooks, `status` and scripts see no
  difference. The host rule stands down once tokens are required, because
  the documented shared setup forwards the public name from a proxy and a
  rebound page has no token to present
- Every response now carries a content security policy under which no script
  runs, no page is framed and nothing loads from another host, plus `nosniff`
  and `Referrer-Policy: no-referrer`. The one visible change: an image a page
  links from somewhere else is no longer fetched when the page is opened in
  `/ui`, since that request would tell its owner which page was read, and when
- **Three shapes of secret went through redaction untouched**, and every one
  of them was found by a property test rather than a leak. Redaction runs
  before anything is written to `raw/`, the append-only copy that outlives the
  index, so a miss there is permanent. A quoted value stopped at its first
  space: `password="00a aaa"` matched nothing at all, because the first word
  was under the six-character floor, and `password = "correct horse battery"`
  kept everything after `correct`. A Google API key ending in `-` — about one
  in sixty-four — failed the rule's closing word boundary and passed whole. And
  a URL password was cut at its first `@`, so `postgres://app:p@ssw0rd@db` kept
  `ssw0rd`, and a password beginning with `@` kept all of itself. Quoted values
  are now redacted between their quotes, the key's end is a character that
  cannot continue it, and URL credentials run to the last `@` before the host

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
