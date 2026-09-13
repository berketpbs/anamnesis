//! Properties of the functions whose input arrives from outside.
//!
//! The unit tests beside each module say what happens to the inputs somebody
//! thought of. These say what has to hold for inputs nobody thought of: a path
//! an agent named, text a hook captured, a directory a person happened to
//! create with a letter whose lowercase is a different length. Each property
//! is the promise the module makes, written once and tried a few hundred
//! times.

use std::path::{Component, Path};

use anamnesis_core::capture::CaptureFilter;
use anamnesis_core::config::CaptureConfig;
use anamnesis_core::embedding::page_sections;
use anamnesis_core::page::PagePath;
use anamnesis_core::retrieval::tokenize;
use anamnesis_core::sanitize::Redactor;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Page paths. A page path becomes a file under the wiki root, so validation is
// a containment boundary.
// ---------------------------------------------------------------------------

/// Path-shaped strings, weighted toward the pieces that make paths dangerous.
fn path_like() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        4 => "[a-z0-9-]{1,12}",
        1 => Just("..".to_owned()),
        1 => Just(".".to_owned()),
        1 => Just(String::new()),
        1 => "[ .]{1,3}",
        1 => "\\PC{1,6}",
        1 => Just("C:".to_owned()),
        1 => Just("a\\b".to_owned()),
        1 => Just("\u{0000}".to_owned()),
    ];
    (
        prop::bool::ANY,
        prop::collection::vec(piece, 1..6),
        prop::bool::ANY,
    )
        .prop_map(|(rooted, pieces, markdown)| {
            let mut path = pieces.join("/");
            if rooted {
                path.insert(0, '/');
            }
            if markdown {
                path.push_str(".md");
            }
            path
        })
}

proptest! {
    /// Whatever is accepted is a relative path made only of ordinary names:
    /// nothing absolute, no drive, no `..`, nothing that joins onto the wiki
    /// root and ends up somewhere else.
    #[test]
    fn an_accepted_page_path_cannot_leave_the_wiki(raw in path_like()) {
        if let Ok(path) = PagePath::parse(&raw) {
            let components: Vec<Component<'_>> = Path::new(path.as_str()).components().collect();
            prop_assert!(!components.is_empty());
            for component in components {
                prop_assert!(
                    matches!(component, Component::Normal(_)),
                    "{:?} was accepted with a {:?} component",
                    path.as_str(),
                    component
                );
            }
            prop_assert!(path.as_str().ends_with(".md"));
            prop_assert!(!path.as_str().contains('\\'));
        }
    }

    /// Arbitrary text, not only path-shaped text, is refused or accepted —
    /// never a panic.
    #[test]
    fn any_string_is_either_a_page_path_or_an_error(raw in "\\PC{0,80}") {
        let _ = PagePath::parse(&raw);
    }

    /// Stored paths are read back through `parse`, and a stored path that no
    /// longer parses is a panic in the store. So what `parse` produces, `parse`
    /// has to accept, unchanged.
    #[test]
    fn a_parsed_page_path_parses_to_itself(raw in path_like()) {
        if let Ok(path) = PagePath::parse(&raw) {
            let again = PagePath::parse(path.as_str()).expect("a parsed path parses again");
            prop_assert_eq!(again, path);
        }
    }
}

// ---------------------------------------------------------------------------
// Tokens. A query is tokenized before it is matched, and each token is quoted
// into a full-text expression and bound into SQL.
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn tokens_are_nonempty_distinct_and_free_of_separators(text in "\\PC{0,200}") {
        let tokens = tokenize(&text);

        let mut seen = std::collections::HashSet::new();
        for token in &tokens {
            prop_assert!(!token.is_empty());
            prop_assert!(seen.insert(token.clone()), "{token:?} repeated");
            prop_assert!(
                !token.chars().any(|c| c.is_whitespace() || c == '"' || c == '\'' || c == '/'),
                "{token:?} carries a separator"
            );
        }
    }

    /// The same text always tokenizes the same way. A query and the entity it
    /// is matched against are tokenized at different times, sometimes by
    /// different builds; anything nondeterministic here is a match that comes
    /// and goes.
    #[test]
    fn tokenizing_is_deterministic(text in "\\PC{0,120}") {
        prop_assert_eq!(tokenize(&text), tokenize(&text));
    }
}

// ---------------------------------------------------------------------------
// Capture exclusions. The fault these found: a root whose letters change byte
// length when lowered cut the relative path in the wrong place.
// ---------------------------------------------------------------------------

/// Directory names drawn from letters whose lowercase is a different number of
/// bytes, mixed with ordinary ones.
fn awkward_name() -> impl Strategy<Value = String> {
    let letter = prop_oneof![
        3 => "[a-zA-Z0-9]",
        1 => Just("ẞ".to_owned()),
        1 => Just("\u{212A}".to_owned()),
        1 => Just("İ".to_owned()),
        1 => Just("Σ".to_owned()),
        1 => Just("ğ".to_owned()),
        1 => Just("Ö".to_owned()),
    ];
    prop::collection::vec(letter, 1..8).prop_map(|letters| letters.concat())
}

fn lowered(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

proptest! {
    /// `target/**` is anchored at the project root, however the root is
    /// spelled and whichever case the agent reports it in.
    #[test]
    fn an_anchored_pattern_excludes_what_is_under_the_root(
        root in prop::collection::vec(awkward_name(), 1..4),
        below in prop::collection::vec("[a-z0-9]{1,8}", 0..4),
        lower_the_report in prop::bool::ANY,
    ) {
        let root = format!("/{}", root.join("/"));
        let filter = CaptureFilter::compile(
            &CaptureConfig { ignore_paths: vec!["target/**".to_owned()] },
            root.as_str(),
        )
        .expect("compile");

        let reported_root = if lower_the_report { lowered(&root) } else { root.clone() };
        let mut inside = format!("{reported_root}/target");
        for part in &below {
            inside.push('/');
            inside.push_str(part);
        }
        inside.push_str("/file");

        prop_assert!(filter.excludes(&inside), "{inside:?} under {root:?} was not excluded");

        let outside = format!("{reported_root}/src/target/file");
        prop_assert!(!filter.excludes(&outside), "{outside:?} under {root:?} was excluded");
    }

    /// And any path at all is a yes or a no.
    #[test]
    fn any_reported_path_is_answered_without_panicking(
        root in "\\PC{0,24}",
        path in "\\PC{0,48}",
    ) {
        if let Ok(filter) = CaptureFilter::compile(
            &CaptureConfig { ignore_paths: vec!["target/**".to_owned(), ".env".to_owned()] },
            root.as_str(),
        ) {
            let _ = filter.excludes(&path);
        }
    }
}

// ---------------------------------------------------------------------------
// Sections. A long page is embedded in pieces the model reads whole, and the
// promise is that the pieces are the page: each one fits, and nothing of the
// body is left out of all of them.
// ---------------------------------------------------------------------------

/// Markdown made of the things that decide where a page is cut.
fn markdown_body() -> impl Strategy<Value = String> {
    let line = prop_oneof![
        4 => "[a-z]{1,8}( [a-z]{1,8}){0,14}[.!?]?",
        1 => "#{1,3} [a-z]{1,8}( [a-z]{1,8}){0,3}",
        1 => Just(String::new()),
        1 => Just("```".to_owned()),
        1 => "[a-z0-9]{20,60}",
        1 => "\\PC{1,20}",
    ];
    prop::collection::vec(line, 0..40).prop_map(|lines| lines.join("\n"))
}

fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

proptest! {
    #[test]
    fn every_section_fits_and_together_they_are_the_whole_page(
        title in "[A-Za-z]{1,8}( [A-Za-z]{1,8}){0,4}",
        body in markdown_body(),
        limit in 6usize..40,
    ) {
        let fits = |text: &str| text.split_whitespace().count() <= limit;
        let pieces = page_sections(&title, &body, &fits);

        for piece in &pieces {
            prop_assert!(fits(piece), "{piece:?} is over {limit} words");
        }

        // Every word of the body, in order, somewhere in the pieces. Headings
        // and the title repeat in every piece of their section, which only adds
        // words, so the body has to be a subsequence of the pieces rather than
        // equal to them.
        let body_words = words(&body);
        let joined = pieces.join(" ");
        let mut remaining = words(&joined).into_iter();
        for word in &body_words {
            prop_assert!(
                remaining.any(|candidate| candidate == *word),
                "{word:?} is missing from the pieces of {body:?}: {pieces:?}"
            );
        }
    }

    #[test]
    fn a_page_is_cut_the_same_way_every_time(body in markdown_body(), limit in 6usize..40) {
        let fits = |text: &str| text.split_whitespace().count() <= limit;
        prop_assert_eq!(page_sections("T", &body, &fits), page_sections("T", &body, &fits));
    }
}

// ---------------------------------------------------------------------------
// Redaction. The promise is negative: a secret of a recognised shape does not
// come out the other side, wherever in the text it sits.
// ---------------------------------------------------------------------------

/// What a secret is found next to in real text: an assignment, a quote, a
/// header, a line break.
fn separator() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        " ", "\n", "\t", "\"", "'", "(", ")", ",", "`", ": ", "= ",
    ])
}

/// Prose that cannot itself look like a secret.
fn prose() -> impl Strategy<Value = String> {
    "[a-z ]{0,30}"
}

/// One secret in each shape the redactor claims to recognise.
fn secret() -> impl Strategy<Value = String> {
    prop_oneof![
        "[A-Za-z0-9_-]{16,40}".prop_map(|s| format!("sk-ant-api03-{s}")),
        "[A-Za-z0-9]{20,40}".prop_map(|s| format!("sk-{s}")),
        "[A-Za-z0-9_-]{20,40}".prop_map(|s| format!("sk-proj-{s}")),
        "[0-9A-Za-z_-]{35}".prop_map(|s| format!("AIza{s}")),
        "[0-9A-Za-z_-]{20,60}".prop_map(|s| format!("ya29.{s}")),
        "[0-9A-Za-z_-]{30,60}".prop_map(|s| format!("AQ.{s}")),
        "[A-Za-z0-9]{16,32}".prop_map(|s| format!("sk_live_{s}")),
        "[A-Za-z0-9]{30,40}".prop_map(|s| format!("npm_{s}")),
        "[A-Za-z0-9_-]{20,43}".prop_map(|s| format!("anam_{s}")),
        "[A-Za-z0-9]{36}".prop_map(|s| format!("ghp_{s}")),
        "[A-Za-z0-9]{10,30}".prop_map(|s| format!("xoxb-{s}")),
        "[0-9A-Z]{16}".prop_map(|s| format!("AKIA{s}")),
        (
            "[A-Za-z0-9_-]{8,20}",
            "[A-Za-z0-9_-]{8,20}",
            "[A-Za-z0-9_-]{8,20}"
        )
            .prop_map(|(a, b, c)| format!("eyJ{a}.{b}.{c}")),
    ]
}

proptest! {
    #[test]
    fn a_recognised_secret_does_not_survive_redaction(
        before in prose(),
        open in separator(),
        secret in secret(),
        close in separator(),
        after in prose(),
    ) {
        let text = format!("{before}{open}{secret}{close}{after}");
        let redacted = Redactor::new().redact(&text);
        prop_assert!(
            !redacted.text().contains(&secret),
            "{secret:?} survived in {:?}",
            redacted.text()
        );
        prop_assert!(!redacted.is_clean());
    }

    /// A password in a URL is removed whole, including when it contains an
    /// `@` — which a connection string written by hand often does, since the
    /// drivers that read one split at the last `@`, not the first.
    #[test]
    fn a_password_in_a_url_does_not_survive_redaction(
        scheme in prop::sample::select(vec!["https", "postgres", "redis", "mongodb+srv"]),
        user in "[a-z]{3,10}",
        password in "[A-Za-z0-9!#$%^&*@]{6,20}",
        host in "[a-z]{3,10}\\.example",
    ) {
        prop_assume!(password.chars().any(|c| c.is_ascii_alphanumeric()));
        let text = format!("connect with {scheme}://{user}:{password}@{host}/db please");
        let redacted = Redactor::new().redact(&text);
        prop_assert!(
            !redacted.text().contains(&password),
            "{password:?} survived in {:?}",
            redacted.text()
        );
        prop_assert!(redacted.text().contains(&host), "the host went too: {:?}", redacted.text());
    }

    /// A quoted value is the whole of what is between the quotes, spaces and
    /// commas included. `password = "correct horse battery"` used to lose its
    /// first word and keep the rest.
    #[test]
    fn a_quoted_secret_value_does_not_survive_redaction(
        key in prop::sample::select(vec!["password", "api_key", "DB_PASSWORD", "clientSecret", "token"]),
        words in prop::collection::vec("[A-Za-z0-9]{3,8}", 2..5),
        joiner in prop::sample::select(vec![" ", ", ", "; "]),
        quote in prop::sample::select(vec!["\"", "'"]),
        separator in prop::sample::select(vec!["=", ": ", " = "]),
    ) {
        let value = words.join(joiner);
        let text = format!("{key}{separator}{quote}{value}{quote}\nnext line");
        let redacted = Redactor::new().redact(&text);
        for word in &words {
            prop_assert!(
                !redacted.text().contains(word.as_str()),
                "{word:?} of {value:?} survived in {:?}",
                redacted.text()
            );
        }
        prop_assert!(redacted.text().contains("next line"), "{:?}", redacted.text());
    }

    /// Redacting twice changes nothing the first pass did not. The spool, the
    /// index and a page each run redaction over text that may already have
    /// been through it.
    #[test]
    fn redaction_is_idempotent(
        before in prose(),
        open in separator(),
        secret in secret(),
        close in separator(),
        after in prose(),
    ) {
        let redactor = Redactor::new();
        let once = redactor.redact(&format!("{before}{open}{secret}{close}{after}"));
        let twice = redactor.redact(once.text());
        prop_assert_eq!(once.text(), twice.text());
    }

    #[test]
    fn any_text_is_redacted_without_panicking(text in "\\PC{0,400}") {
        let _ = Redactor::new().redact(&text);
    }
}
