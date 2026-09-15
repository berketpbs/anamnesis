//! Giving a page the vector it was written without.
//!
//! A page whose embedding fails is written anyway, and the failure is filed in
//! `page_embed_failures` for `doctor` to report. That is the right trade at the
//! moment of writing — it costs the page one retrieval stream instead of the
//! page — and until now it was also permanent: nothing but `anamnesis reindex`,
//! run by hand, ever asked the embedder again.
//!
//! On the machine this project runs on, the embedder is an Ollama that Windows
//! does not always start at login. On 2026-09-15 the server came up twice
//! without it, and every session page written that afternoon went into the
//! index without a vector; `doctor` counted four by the evening. The server was
//! already built to live with that — it starts, and asks the endpoint again
//! when a vector is next needed — but the pages written in between stayed out
//! of the vector stream for as long as nobody noticed.
//!
//! So the server asks again. Every [`TICK`], if any page is missing a vector
//! under the model this server embeds with, one short text goes to the
//! embedder first; only if that comes back are the pages sent, a handful at a
//! time. An endpoint that is still down costs one quiet request a minute and
//! not a warning per page, and one that has come back fills the gap within
//! minutes, without a restart and without anyone running anything.

use std::sync::Arc;

use anamnesis_core::embedding::Embed;
use anamnesis_core::page::Page;
use anamnesis_llm::Embedder;

use crate::improve::TICK;
use crate::{AppState, WebError};

/// How many pages one pass sends to the embedder.
///
/// Each one holds the wiki for the length of an embedding, the way a page
/// written by consolidation does, so a pass is kept to seconds.
const BATCH: usize = 20;

/// The text sent to find out whether the embedder answers before any page is.
const PROBE: &str = "anamnesis";

/// One pass: fill in the vectors that failed, if the embedder answers now.
///
/// Returns how many pages have a vector that did not have one before.
pub async fn fill_in(state: &AppState) -> usize {
    let Some(embedder) = state.embedder.clone() else {
        return 0;
    };
    let store = state.store.clone();
    let wiki = state.wiki.clone();
    let filled = crate::off_runtime(move || -> Result<usize, WebError> {
        fill_in_blocking(&store, &wiki, embedder)
    })
    .await;
    match filled {
        Ok(filled) => {
            if filled > 0 {
                tracing::info!(
                    pages = filled,
                    "the embedder answered again; gave pages the vectors they were written without"
                );
            }
            filled
        }
        Err(error) => {
            tracing::error!(%error, "could not fill in the vectors pages were written without");
            0
        }
    }
}

fn fill_in_blocking(
    store: &Arc<anamnesis_store::Store>,
    wiki: &Arc<parking_lot::Mutex<anamnesis_wiki::Wiki>>,
    embedder: Arc<dyn Embedder>,
) -> Result<usize, WebError> {
    let waiting = store.pages_missing_vectors(embedder.model(), BATCH)?;
    if waiting.is_empty() {
        return Ok(0);
    }
    // Asked once before any page is. Sending a page to an endpoint that is
    // down records the same failure again and logs a warning for it, every
    // minute, for every page — a log that says one thing a thousand times.
    if let Err(error) = embedder.embed(PROBE) {
        tracing::debug!(
            %error,
            pages = waiting.len(),
            "pages are waiting for vectors and the embedder does not answer yet"
        );
        return Ok(0);
    }

    let projects = store.projects()?;
    let embed: &dyn Embed = embedder.as_ref();
    for (project_id, path) in &waiting {
        let Some(project) = projects.iter().find(|row| row.project_id == *project_id) else {
            continue;
        };
        // Read and embedded under one hold of the wiki, as consolidation writes
        // a page: a page rewritten between the read and the vector would
        // otherwise get the vector of what it said before.
        let held = wiki.lock();
        let parsed = match held.read_page(&project.scope, path) {
            Ok(parsed) => parsed,
            // Gone, or mid-edit. The watcher and `reindex` own both cases, and
            // the row stays for the next pass.
            Err(error) => {
                tracing::debug!(%error, page = %path, "left a page waiting for its vector: not readable");
                continue;
            }
        };
        let page = Page::new(*project_id, path.clone(), parsed.frontmatter, parsed.body);
        store.embed_page(&page, Some(embed))?;
    }

    // Every failure, not a batch of them: a page that failed again was filed
    // with a newer time and sorts behind the ones nobody has retried yet.
    let still = store.pages_missing_vectors(embedder.model(), usize::MAX)?;
    Ok(waiting.iter().filter(|page| !still.contains(page)).count())
}

/// Ask again every [`TICK`], forever.
///
/// Waits before its first pass, like the enricher: the pages it revisits are
/// written and in no hurry, and a server that has just started is still
/// finding out whether its embedder is up.
pub async fn run_revectoring(state: AppState) {
    if state.embedder.is_none() {
        return;
    }
    loop {
        tokio::time::sleep(TICK).await;
        fill_in(&state).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use anamnesis_core::page::{Frontmatter, PagePath};
    use anamnesis_core::scope::{ResolvedScope, resolve_scope};
    use anamnesis_store::Store;
    use anamnesis_wiki::Wiki;
    use jiff::Timestamp;
    use parking_lot::Mutex;

    use super::*;

    /// An Ollama that can be switched on, and that remembers what it was sent.
    #[derive(Default)]
    struct Switched {
        up: AtomicBool,
        sent: Mutex<Vec<String>>,
    }

    impl Embed for Switched {
        fn model(&self) -> &str {
            "nomic-embed-text"
        }
        fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
            self.sent.lock().push(text.to_owned());
            if self.up.load(Ordering::SeqCst) {
                Ok(vec![1.0, 0.0])
            } else {
                Err("could not load model \"nomic-embed-text\": error sending request".to_owned())
            }
        }
    }

    impl Embedder for Switched {
        fn dimension(&self) -> usize {
            2
        }
    }

    struct Harness {
        _repo: tempfile::TempDir,
        _data: tempfile::TempDir,
        state: AppState,
        scope: ResolvedScope,
        embedder: Arc<Switched>,
    }

    fn harness() -> Harness {
        let repo = tempfile::tempdir().expect("repo dir");
        std::fs::write(
            repo.path().join(".anamnesis.toml"),
            "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
        )
        .expect("marker");
        let data = tempfile::tempdir().expect("data dir");
        let store = Store::open(data.path().join("index.db")).expect("store");
        store.migrate().expect("migrate");
        let wiki = Wiki::open(data.path().join("wiki")).expect("wiki");
        let scope = resolve_scope(repo.path()).expect("scope");
        store
            .upsert_project(&scope, Timestamp::now())
            .expect("project");

        let embedder = Arc::new(Switched::default());
        Harness {
            state: AppState::new(store, wiki)
                .with_embedder(Some(embedder.clone() as Arc<dyn Embedder>)),
            scope,
            embedder,
            _repo: repo,
            _data: data,
        }
    }

    /// A page written while the embedder was down, the way the server writes
    /// one: into the wiki, then indexed, with the failure filed.
    fn written_while_down(harness: &Harness, path: &str, body: &str) -> Page {
        let page = Page::new(
            harness.scope.project_id,
            PagePath::parse(path).expect("path"),
            Frontmatter::new("A session", Vec::new()).expect("frontmatter"),
            body,
        );
        harness
            .state
            .wiki
            .lock()
            .write_page(&harness.scope.scope, &page, "write")
            .expect("write");
        let embed: &dyn Embed = harness.embedder.as_ref();
        harness
            .state
            .store
            .index_page(
                harness.scope.project_id,
                &page,
                &[],
                Some(embed),
                Timestamp::now(),
            )
            .expect("index");
        page
    }

    fn missing(harness: &Harness) -> usize {
        harness
            .state
            .store
            .pages_missing_vectors("nomic-embed-text", 100)
            .expect("missing")
            .len()
    }

    /// The afternoon this was written for: pages written without Ollama, and
    /// Ollama coming back later with nobody running `reindex`.
    #[tokio::test]
    async fn pages_written_while_the_embedder_was_down_get_vectors_once_it_answers() {
        let harness = harness();
        written_while_down(&harness, "sessions/one.md", "What the first session did.");
        written_while_down(&harness, "sessions/two.md", "What the second session did.");
        assert_eq!(missing(&harness), 2);

        harness.embedder.sent.lock().clear();
        assert_eq!(fill_in(&harness.state).await, 0, "still down");
        assert_eq!(
            *harness.embedder.sent.lock(),
            vec![PROBE.to_owned()],
            "a down embedder is asked one short question, not sent every page"
        );
        assert_eq!(missing(&harness), 2);

        harness.embedder.up.store(true, Ordering::SeqCst);
        assert_eq!(fill_in(&harness.state).await, 2);
        assert_eq!(missing(&harness), 0);
        assert!(
            harness
                .embedder
                .sent
                .lock()
                .iter()
                .any(|text| text.contains("What the second session did.")),
            "the page is embedded from what the wiki holds"
        );
        assert!(
            harness
                .state
                .store
                .embed_failures(harness.scope.project_id)
                .expect("failures")
                .is_empty(),
            "doctor has nothing left to report"
        );

        harness.embedder.sent.lock().clear();
        assert_eq!(fill_in(&harness.state).await, 0);
        assert!(
            harness.embedder.sent.lock().is_empty(),
            "nothing waiting, nothing sent — not even the probe"
        );
    }

    /// A page removed from the wiki by hand is left to the watcher and
    /// `reindex`, and does not stop the pages behind it.
    #[tokio::test]
    async fn a_page_that_cannot_be_read_does_not_hold_up_the_rest() {
        let harness = harness();
        let gone = written_while_down(&harness, "sessions/gone.md", "Deleted by hand.");
        written_while_down(&harness, "sessions/kept.md", "Still here.");
        std::fs::remove_file(
            harness
                .state
                .wiki
                .lock()
                .scope_root(&harness.scope.scope)
                .join(gone.path.as_str()),
        )
        .expect("remove");

        harness.embedder.up.store(true, Ordering::SeqCst);
        assert_eq!(fill_in(&harness.state).await, 1);
        assert_eq!(missing(&harness), 1, "the unreadable one waits");
    }

    #[tokio::test]
    async fn a_server_with_no_embedder_has_nothing_to_fill_in() {
        let mut harness = harness();
        written_while_down(&harness, "sessions/one.md", "Body.");
        harness.state.embedder = None;
        assert_eq!(fill_in(&harness.state).await, 0);
    }
}
