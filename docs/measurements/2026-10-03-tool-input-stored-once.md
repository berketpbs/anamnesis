# Tool input stored once: V21 acceptance

Measured on Windows on 2026-10-03. This is an index-storage measurement,
not evidence of better recall or successful cross-agent continuity.

## Upgrade on a real backup copy

Source: `pre-v1.2.1-rc.2-20261003-143011.tar.gz`, a schema-20 backup.
Restored to a new scratch directory outside the checkout. The live service
and original archive were not changed. The installed RC2 binary (`9cdb927`)
first checked the restored index against its wiki and transcripts.

The branch was rebased onto `origin/main` (`6a2ba09`). Its new binary started
an isolated server on port 18082 with model calls and embedding disabled,
upgraded the copy to V21, and ran `doctor`. After that server stopped,
`reindex --check` compared the upgraded index with a fresh rebuild.

Both checks agreed on the project:

| Indexed material | Compared before and after |
| --- | ---: |
| Pages | 197 |
| Entities | 821 |
| Links | 143 |
| Observations | 26,389 |

Across all scopes in the copied database:

| Measurement | V20 | V21 |
| --- | ---: | ---: |
| Observation rows | 26,422 | 26,422 |
| UTF-8 body bytes | 19,422,127 | 12,779,481 |
| Tool-attempt body bytes | 6,950,508 | 307,862 |
| Truncated bodies | 71 | 60 |

Body text fell by 6,642,646 bytes (34.2%). This measures text, not the SQLite
file size: migration does not VACUUM. Raw transcripts keep both inputs.
`doctor` returned 0; its existing thin findings concern missing explicit tool
outcomes and four truncated embeddings. Embedding was not rebuilt or measured.
Reindex does not compare vectors, handoffs, access counts or session state.

## Regression checks

- V20 fixture backed up through SQLite and upgraded through the real migration
  runner: byte-for-byte observation equality with live settlement, Unicode,
  identical/prefix input, mismatched input, unfinished attempts, missing call
  identifiers, other sessions and wrong event kinds. The source is unchanged;
  reopening V21 is idempotent.
- Capture tests cover either arrival order; the CLI test checks live/reindex
  equality and rebuilding from raw transcripts with no database.
- The release-upgrade fixture now records a paired call and an unfinished call
  through the old binary's hooks, then verifies delivery, search and rebuild.

Full workspace tests, formatting and Clippy with warnings denied passed.
The ignored release-upgrade test also passed locally against the installed
RC2 (`9cdb927`) binary. The other published releases remain the CI upgrade
matrix's responsibility. Genuine Claude → Codex → Claude acceptance and
held-out usefulness runs remain pending while Claude access is exhausted.
