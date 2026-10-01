# Targeted mutation audit, 2026-10-01

The first [weekly mutation audit](https://github.com/berketpbs/anamnesis/actions/runs/36898409799)
ran on `409e83b` with cargo-mutants 27.1.0. It covered
`anamnesis-core/src/handoff.rs`, `anamnesis-core/src/retrieval.rs` and
`anamnesis-store/src/ops.rs`. Three shards finished in about 8–11 minutes
each and uploaded the full logs and mutant outcomes.

| Result | Count |
| --- | ---: |
| Generated and tested | 233 |
| Caught by the mutated package's tests | 154 |
| Missed by those tests | 34 |
| Unviable (the mutation did not compile) | 45 |

The scheduled workflow fails when it finds a missed mutant, so the red result
is expected until these cases are triaged. By default cargo-mutants tests the
mutated package, not every crate that consumes it. A miss is therefore a local
test gap candidate, not proof that the whole workspace misses the behavior.
For example, `Store::insert_observation` returning the opposite boolean was
missed by store-package tests but **caught** when the same single mutant was
rerun with `--test-workspace true` on the unchanged store code. Re-run a
candidate against the full workspace before calling it an end-to-end gap.

`Slot::shared()` returning `Default::default()` is an exact equivalent of its
implementation and needs no test. The other survivors below still need
classification. Names and line numbers refer to the audited commit; the
uploaded `missed.txt` and `outcomes.json` files hold the original results.

## Missed with package-local tests

```text
crates/anamnesis-core/src/handoff.rs:38:9: replace Slot::shared -> Self with Default::default()
crates/anamnesis-core/src/handoff.rs:43:9: replace Slot::for_workstream -> Self with Default::default()
crates/anamnesis-core/src/handoff.rs:51:9: replace Slot::for_operator -> Self with Default::default()
crates/anamnesis-core/src/handoff.rs:57:9: replace Slot::workstream_key -> Option<String> with None
crates/anamnesis-core/src/handoff.rs:57:9: replace Slot::workstream_key -> Option<String> with Some(String::new())
crates/anamnesis-core/src/handoff.rs:57:9: replace Slot::workstream_key -> Option<String> with Some("xyzzy".into())
crates/anamnesis-core/src/handoff.rs:62:9: replace Slot::operator_key -> Option<String> with None
crates/anamnesis-core/src/handoff.rs:62:9: replace Slot::operator_key -> Option<String> with Some(String::new())
crates/anamnesis-core/src/handoff.rs:62:9: replace Slot::operator_key -> Option<String> with Some("xyzzy".into())
crates/anamnesis-core/src/retrieval.rs:285:9: replace Tuning::weights -> [f64; 5] with [1.0; 5]
crates/anamnesis-core/src/retrieval.rs:285:9: replace Tuning::weights -> [f64; 5] with [-1.0; 5]
crates/anamnesis-core/src/retrieval.rs:285:9: replace Tuning::weights -> [f64; 5] with [0.0; 5]
crates/anamnesis-core/src/retrieval.rs:430:45: replace / with * in fuse_standing
crates/anamnesis-core/src/retrieval.rs:462:20: replace *= with += in authority_multiplier
crates/anamnesis-core/src/retrieval.rs:465:20: replace *= with += in authority_multiplier
crates/anamnesis-core/src/retrieval.rs:468:20: replace *= with += in authority_multiplier
crates/anamnesis-store/src/ops.rs:454:9: replace Store::awaits_enrichment -> Result<bool> with Ok(true)
crates/anamnesis-store/src/ops.rs:454:9: replace Store::awaits_enrichment -> Result<bool> with Ok(false)
crates/anamnesis-store/src/ops.rs:512:21: replace == with != in Store::insert_observation
crates/anamnesis-store/src/ops.rs:672:27: replace > with >= in Store::embed_sections
crates/anamnesis-store/src/ops.rs:979:9: replace Store::page_is_latest -> Result<Option<bool>> with Ok(Some(false))
crates/anamnesis-store/src/ops.rs:979:9: replace Store::page_is_latest -> Result<Option<bool>> with Ok(None)
crates/anamnesis-store/src/ops.rs:979:9: replace Store::page_is_latest -> Result<Option<bool>> with Ok(Some(true))
crates/anamnesis-store/src/ops.rs:1275:64: replace > with >= in Store::latest_handoff
crates/anamnesis-store/src/ops.rs:1300:9: replace Store::last_work -> Result<Option<Timestamp>> with Ok(Some(Default::default()))
crates/anamnesis-store/src/ops.rs:1300:9: replace Store::last_work -> Result<Option<Timestamp>> with Ok(None)
crates/anamnesis-store/src/ops.rs:1345:60: replace > with >= in Store::open_taker_of
crates/anamnesis-store/src/ops.rs:1437:9: replace Store::last_event_kind -> Result<Option<EventKind>> with Ok(None)
crates/anamnesis-store/src/ops.rs:1546:22: replace && with || in Store::scrubbed
crates/anamnesis-store/src/ops.rs:1546:33: replace > with >= in Store::scrubbed
crates/anamnesis-store/src/ops.rs:1696:9: replace Store::standing_decisions -> Result<Vec<StandingDecision>> with Ok(vec![])
crates/anamnesis-store/src/ops.rs:1696:18: replace == with != in Store::standing_decisions
crates/anamnesis-store/src/ops.rs:1739:9: replace Store::standing_among -> Result<Vec<StandingDecision>> with Ok(vec![])
crates/anamnesis-store/src/ops.rs:2038:18: replace > with >= in newer_account
```

Prioritize the handoff slot keys, retrieval weights and authority, and the
store's latest-page and enrichment answers for focused follow-up. Preserve
the exact input and expected observable result in each new test, then rerun
the relevant mutant to confirm that the test catches it.
