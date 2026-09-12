//! What the wiki browser does with a page body nobody vetted.
//!
//! Page bodies are written by models, by consolidation of captured prompts, and
//! by hand, and `/ui` renders them. The unit tests in `ui.rs` show that the
//! obvious `<script>` is shown rather than run. This tries the rest of the
//! ways markdown has of producing live HTML — raw tags, event attributes,
//! link and image destinations, autolinks, reference definitions, entities —
//! mixed with ordinary text, through the real router.

use anamnesis_core::page::{Frontmatter, Page, PagePath, PageStatus, Tier};
use anamnesis_core::scope::resolve_scope;
use anamnesis_store::Store;
use anamnesis_web::{AppState, router};
use anamnesis_wiki::Wiki;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use proptest::prelude::*;
use tower::ServiceExt;

/// Pieces of markdown that would produce executable HTML if passed through.
fn hostile_fragment() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[a-zA-Z0-9 .,]{0,24}",
        1 => "\\PC{0,12}",
        2 => prop::sample::select(vec![
            "<script>alert(1)</script>",
            "<SCRIPT SRC=//evil.example/x.js></SCRIPT>",
            "<img src=x onerror=alert(1)>",
            "<svg onload=alert(1)>",
            "<iframe src=javascript:alert(1)></iframe>",
            "<a href=\"javascript:alert(1)\">x</a>",
            "<div style=\"background:url(javascript:alert(1))\">x</div>",
            "<details open ontoggle=alert(1)>",
            "<!-- <script>alert(1)</script> -->",
            "<body onload=alert(1)>",
        ])
        .prop_map(str::to_owned),
        2 => prop::sample::select(vec![
            "[click](javascript:alert(1))",
            "[click](JaVaScRiPt:alert(1))",
            "[click]( javascript:alert(1) )",
            "[click](&#106;avascript:alert(1))",
            "[click](java&#x09;script:alert(1))",
            "[click](vbscript:msgbox(1))",
            "[click](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)",
            "![img](javascript:alert(1))",
            "![img](data:image/svg+xml,<svg onload=alert(1)>)",
            "<javascript:alert(1)>",
            "[ref]\n\n[ref]: javascript:alert(1)",
            "[x](\"onmouseover=alert(1))",
            "[[<script>alert(1)</script>]]",
            "`<script>alert(1)</script>`",
            "```html\n<script>alert(1)</script>\n```",
            "| a | <script>alert(1)</script> |\n|---|---|\n| b | c |",
        ])
        .prop_map(str::to_owned),
    ]
}

/// Fragments joined inline, on lines of their own, or as paragraphs — markdown
/// reads a raw tag differently in each.
fn hostile_body() -> impl Strategy<Value = String> {
    (
        prop::collection::vec(hostile_fragment(), 1..8),
        prop::sample::select(vec![" ", "\n", "\n\n"]),
    )
        .prop_map(|(fragments, joiner)| fragments.join(joiner))
}

/// Render one body through `/ui`, as the browser would fetch it.
fn render(body: &str) -> String {
    let repo = tempfile::tempdir().expect("repo");
    std::fs::write(
        repo.path().join(".anamnesis.toml"),
        "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
    )
    .expect("marker");
    let data = tempfile::tempdir().expect("data");
    let store = Store::open(data.path().join("index.db")).expect("store");
    store.migrate().expect("migrate");
    let wiki = Wiki::open(data.path().join("wiki")).expect("wiki");
    let scope = resolve_scope(repo.path()).expect("scope");
    let now = "2026-09-12T09:00:00Z".parse().expect("timestamp");
    store.upsert_project(&scope, now).expect("project");

    let mut frontmatter = Frontmatter::new("A page", Vec::new()).expect("frontmatter");
    frontmatter.tier = Tier::Semantic;
    frontmatter.status = PageStatus::Active;
    let page = Page::new(
        scope.project_id,
        PagePath::parse("notes/hostile.md").expect("path"),
        frontmatter,
        body,
    );
    wiki.write_page(&scope.scope, &page, "write")
        .expect("write");
    store.upsert_page(&page, now).expect("index");

    let state = AppState::new(store, wiki);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async move {
        let response = router(state, true)
            .oneshot(
                Request::builder()
                    .uri("/ui/default/widget/notes/hostile.md")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("routed");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn a_page_body_never_renders_as_live_html(body in hostile_body()) {
        let page = render(&body);
        // The shell around the body is the server's own markup, `<body>` and
        // `<style>` included; what came from the page is inside `<article>`.
        let start = page.find("<article>").expect("an article") + "<article>".len();
        let end = page.rfind("</article>").expect("its end");
        let html = &page[start..end];
        let lower = html.to_ascii_lowercase();

        for tag in ["<script", "<iframe", "<svg", "<object", "<embed", "<body", "<details"] {
            prop_assert!(!lower.contains(tag), "{tag} rendered from {body:?}:\n{html}");
        }

        // An event handler is only live inside a tag; escaped text such as
        // `&lt;img onerror=…` is not one.
        let handler = regex::Regex::new(r"(?i)<[a-z][^>]*\son[a-z]+\s*=").expect("regex");
        prop_assert!(!handler.is_match(html), "event attribute rendered from {body:?}:\n{html}");

        // No destination the browser would execute or render as a document —
        // again only inside a real tag, since a quoted `src=javascript:` in
        // escaped text is the page describing one.
        let destination = regex::Regex::new(
            r#"(?i)<[a-z][^>]*\b(?:href|src)\s*=\s*["']?\s*(?:javascript|vbscript|data):"#,
        )
        .expect("regex");
        prop_assert!(
            !destination.is_match(html),
            "an executable destination rendered from {body:?}:\n{html}"
        );
    }
}
