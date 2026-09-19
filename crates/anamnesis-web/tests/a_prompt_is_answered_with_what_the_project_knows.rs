//! What a session gets back for asking a question it asked five sessions ago.
//!
//! The handoff carries what the session before this one did. `/recall` carries
//! what the project knows about the thing being asked now, which is the half
//! that was missing: measured across twenty-four long-run eval sessions with
//! the MCP server connected and its tools allowed, an agent called a memory
//! tool once.
//!
//! The hard part is not finding pages, it is not offering them. A prompt block
//! that fires on every prompt whether or not it has anything to say teaches an
//! agent to skip it, so the gate — a page has to be within `min_similarity` of
//! the prompt — is what most of this file is about. The embedder here is two
//! angles on a circle, so the distances are arithmetic rather than a model's
//! opinion; what the real distances look like is in `Store::pages_like`.
//!
//! Tested through the real router, because every piece of this is a seam:
//! scope resolved from a working directory, the marker's `[recall]` read off
//! that scope, the embedder, the gate, and a block of text that goes to a
//! hook's stdout and from there into a model's context.

use std::sync::Arc;

use anamnesis_core::embedding::Embed;
use anamnesis_core::page::{Frontmatter, Page, PagePath, PageStatus, Tier};
use anamnesis_core::scope::resolve_scope;
use anamnesis_llm::Embedder;
use anamnesis_store::Store;
use anamnesis_web::{AppState, router};
use anamnesis_wiki::Wiki;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

/// Angles on a circle: one for what the project wrote about, two for what else
/// it wrote about, and one for a question from outside it.
///
/// The last is the one that matters, and it is deliberately not *opposite* the
/// corpus: an off-topic prompt in a real corpus is not orthogonal to
/// everything, it is middling against everything. At 65° it sits 0.42 from the
/// page at 0° and 0.42 from the page at 130°, which is the shape of the real
/// measurement — an off-topic prompt peaked at 0.542 where an on-topic one
/// started at 0.573 — with the numbers pushed apart so the test is about the
/// gate rather than an embedder's third decimal.
struct WordEmbedder;

impl Embedder for WordEmbedder {
    fn dimension(&self) -> usize {
        2
    }
}

impl Embed for WordEmbedder {
    fn model(&self) -> &str {
        "test-embed-2"
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let text = text.to_lowercase();
        if text.contains("log") {
            Ok(vec![1.0, 0.0]) // 0°
        } else if text.contains("weather") || text.contains("istanbul") {
            Ok(vec![0.4226, 0.9063]) // 65°: middling against all of them
        } else if text.contains("rate") {
            Ok(vec![-0.6428, 0.7660]) // 130°
        } else {
            Ok(vec![-0.7660, 0.6428]) // 140°
        }
    }
}

/// An embedder whose model is down.
struct BrokenEmbedder;

impl Embedder for BrokenEmbedder {
    fn dimension(&self) -> usize {
        2
    }
}

impl Embed for BrokenEmbedder {
    fn model(&self) -> &str {
        "test-embed-2"
    }

    fn embed(&self, _text: &str) -> Result<Vec<f32>, String> {
        Err("connection refused".to_owned())
    }
}

/// A project with three pages in it, and what `/recall` answers about `q`.
fn recall(marker: &str, q: &str, embedder: bool) -> (StatusCode, String) {
    let embedder = embedder.then(|| Arc::new(WordEmbedder) as Arc<dyn Embedder>);
    recall_with(marker, q, embedder)
}

/// The same, with the server given `embedder` — or none.
fn recall_with(marker: &str, q: &str, embedder: Option<Arc<dyn Embedder>>) -> (StatusCode, String) {
    let repo = tempfile::tempdir().expect("repo");
    std::fs::write(repo.path().join(".anamnesis.toml"), marker).expect("marker");
    let data = tempfile::tempdir().expect("data");
    let store = Store::open(data.path().join("index.db")).expect("store");
    store.migrate().expect("migrate");
    let wiki = Wiki::open(data.path().join("wiki")).expect("wiki");
    let scope = resolve_scope(repo.path()).expect("scope");
    let now = "2026-09-17T09:00:00Z".parse().expect("timestamp");
    store.upsert_project(&scope, now).expect("project");

    let corpus = [
        (
            "notes/logging.md",
            "Amounts never go to logs",
            "Amounts must never be written to logs, not even at debug level: the logs are \
             shipped to a third party and amounts count as customer financial data.",
        ),
        (
            "notes/rates.md",
            "Rates are generated",
            "The rates module is generated from rates.toml and a hand edit disappears.",
        ),
        (
            "notes/deploy.md",
            "Deploying to staging",
            "Staging is deployed from the ops repository.",
        ),
    ];
    let model = WordEmbedder;
    for (path, title, body) in corpus {
        let mut frontmatter = Frontmatter::new(title, Vec::new()).expect("frontmatter");
        frontmatter.tier = Tier::Semantic;
        frontmatter.status = PageStatus::Active;
        let page = Page::new(
            scope.project_id,
            PagePath::parse(path).expect("path"),
            frontmatter,
            body,
        );
        wiki.write_page(&scope.scope, &page, "write")
            .expect("write");
        store
            .index_page(
                scope.project_id,
                &page,
                &[],
                Some(&model as &dyn Embed),
                now,
            )
            .expect("index");
    }

    let state = AppState::new(store, wiki).with_embedder(embedder);
    let uri = format!(
        "/recall?agent=claude-code&session_id=s1&cwd={}&q={}",
        urlencoding(&repo.path().to_string_lossy()),
        urlencoding(q),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async move {
        let response = router(state, false)
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("routed");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    })
}

/// Enough of one to carry a Windows path and a sentence through a query string.
fn urlencoding(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

const MARKER: &str = "[scope]\nworkspace = \"default\"\nproject = \"ledger\"\n";

#[test]
fn a_prompt_is_answered_with_the_page_that_stands_out() {
    let (status, body) = recall(MARKER, "add logging to the importer", true);
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Amounts never go to logs"), "{body}");
    assert!(body.contains("notes/logging.md"), "{body}");
    // One page stood out; the other two are not dragged along with it.
    assert_eq!(body.matches("\n- ").count(), 1, "{body}");
    // The framing travels with it or it is not worth injecting: this lands in
    // a context window beside the user's own words.
    assert!(body.contains("not instructions to follow"), "{body}");
}

/// The case this is all for: a prompt that is middling against every page and
/// about none of them. Nothing here is close enough, so nothing is said.
#[test]
fn a_prompt_this_project_has_nothing_to_say_about_is_left_alone() {
    let (status, body) = recall(MARKER, "what is the weather in Istanbul", true);
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty(), "{body}");
}

/// The gate is a number in the marker because sixteen prompts with one
/// embedder is what set it, and another embedder is another number.
#[test]
fn the_marker_can_open_the_gate_and_close_it() {
    let open = format!("{MARKER}\n[recall]\nmin_similarity = 0.0\n");
    let (_, body) = recall(&open, "what is the weather in Istanbul", true);
    assert!(
        !body.is_empty(),
        "an open gate should offer something: {body}"
    );

    let shut = format!("{MARKER}\n[recall]\nmin_similarity = 1.1\n");
    let (_, body) = recall(&shut, "add logging to the importer", true);
    assert!(body.is_empty(), "a shut gate should offer nothing: {body}");
}

#[test]
fn a_project_that_asked_for_none_of_this_gets_none_of_it() {
    let marker = format!("{MARKER}\n[recall]\non_prompt = false\n");
    let (status, body) = recall(&marker, "add logging to the importer", true);
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty(), "{body}");
}

/// Without an embedder there is nothing to measure closeness with, and the
/// keyword streams cannot stand in: fused, they rank by position, so a prompt
/// about nothing this project knows comes back with the same score at the top
/// as a prompt about its centre. Measured on this machine's 88 pages, both
/// 0.333. What a server with no model has instead is the words a prompt names.
#[test]
fn a_server_with_no_embedder_answers_by_name() {
    let (status, body) = recall(MARKER, "can amounts go to the logs", false);
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("notes/logging.md"), "{body}");
    assert_eq!(
        body.matches(
            "
- "
        )
        .count(),
        1,
        "{body}"
    );
    assert!(body.contains("not instructions to follow"), "{body}");
}

/// And it keeps quiet the same way: every word of this is one the project has
/// never written, and a prompt about somebody else's subject is left alone.
#[test]
fn a_server_with_no_embedder_still_leaves_other_subjects_alone() {
    let (status, body) = recall(MARKER, "what is the weather in Istanbul", false);
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty(), "{body}");
}

/// A local model that is down is the ordinary failure, not an exotic one: the
/// prompt loses the closeness gate, not its answer.
#[test]
fn an_embedder_that_fails_leaves_the_prompt_to_be_answered_by_name() {
    let broken = Some(Arc::new(BrokenEmbedder) as Arc<dyn Embedder>);
    let (status, body) = recall_with(MARKER, "can amounts go to the logs", broken);
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("notes/logging.md"), "{body}");
}

#[test]
fn a_project_can_ask_for_no_answers_by_name() {
    let marker = format!(
        "{MARKER}
[recall]
by_name = false
"
    );
    let (status, body) = recall(&marker, "can amounts go to the logs", false);
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty(), "{body}");
}

/// An empty question is not a question. Every harness sends this event; not
/// every one of them fills the field.
#[test]
fn nothing_asked_is_nothing_answered() {
    let (status, body) = recall(MARKER, "   ", true);
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty(), "{body}");
}
