//! Compacting transcripts that have gone quiet.
//!
//! The raw spool is the part of memory that only grows: every observation is
//! kept there for good, because it is what the index is rebuilt from. Kept as
//! plain JSON it is also the largest part — on the machine this was written
//! on, 25 MB after a month, which gzip takes to 4.5. So once a transcript has
//! had nothing added to it for [`COMPACT_AFTER`], this pass folds it into the
//! compressed file beside it. Nothing is dropped and no line changes; see
//! [`anamnesis_store::RawSpool::compact`].
//!
//! An hour between passes, because nothing here is urgent: a transcript that
//! qualifies at ten past is as well compacted at eleven.

use std::time::{Duration, SystemTime};

use anamnesis_store::{COMPACT_AFTER, Compaction, RawError};

use crate::AppState;

/// How long the compactor sleeps between passes.
const EVERY: Duration = Duration::from_secs(60 * 60);

/// One pass: compact every quiet transcript in the spool.
pub async fn compact(state: &AppState, now: SystemTime) -> Compaction {
    let Some(raw) = state.raw.clone() else {
        return Compaction::default();
    };
    let done = crate::off_runtime(
        move || -> Result<Result<Compaction, RawError>, crate::WebError> {
            Ok(raw.compact_quiet(COMPACT_AFTER, now))
        },
    )
    .await;
    match done {
        Ok(Ok(done)) => {
            if done.files > 0 {
                tracing::info!(
                    files = done.files,
                    before = done.bytes_before,
                    after = done.bytes_after,
                    "compacted transcripts nothing had been added to for a week"
                );
            }
            done
        }
        Ok(Err(error)) => {
            tracing::error!(%error, "could not look for transcripts to compact");
            Compaction::default()
        }
        Err(error) => {
            tracing::error!(%error, "the compaction pass failed");
            Compaction::default()
        }
    }
}

/// Compact quiet transcripts forever.
pub async fn run_compactor(state: AppState) {
    loop {
        let passing = state.clone();
        crate::one_pass("compactor", async move {
            compact(&passing, SystemTime::now()).await;
        })
        .await;
        tokio::time::sleep(EVERY).await;
    }
}
