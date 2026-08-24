//! Redaction of secrets from captured text.
//!
//! This lives in core rather than in the hook layer on purpose. Hook ingestion,
//! consolidation, and wiki writes all need the same redaction, and three copies
//! of these rules would drift — one of them would be the one that leaks. Having
//! it here also means the rules are unit-testable without any hook plumbing.
//!
//! Redaction is a safety net, not a guarantee. It catches recognisable secret
//! shapes; capture exclusions (`ignore_paths`) remain the primary defence for
//! files that should never be read at all.

use std::sync::OnceLock;

use regex::Regex;

/// One redaction rule.
struct Rule {
    name: &'static str,
    pattern: Regex,
    replacement: &'static str,
}

/// Result of running redaction over a piece of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    text: String,
    hits: Vec<&'static str>,
}

impl Redacted {
    /// The redacted text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Consume the result, yielding the redacted text.
    pub fn into_text(self) -> String {
        self.text
    }

    /// Names of the rules that matched.
    ///
    /// Safe to log: rule names describe the shape of what was removed, never
    /// the value.
    pub fn hits(&self) -> &[&'static str] {
        &self.hits
    }

    /// Whether anything was removed.
    pub fn is_clean(&self) -> bool {
        self.hits.is_empty()
    }
}

/// Applies the built-in redaction rules, plus any extra patterns supplied by
/// configuration.
#[derive(Default)]
pub struct Redactor {
    extra: Vec<Rule>,
}

impl Redactor {
    /// A redactor with only the built-in rules.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a caller-supplied pattern. Every match is replaced wholesale.
    pub fn with_pattern(mut self, name: &'static str, pattern: Regex) -> Self {
        self.extra.push(Rule {
            name,
            pattern,
            replacement: "[redacted]",
        });
        self
    }

    /// Redact `input`, reporting which rules fired.
    pub fn redact(&self, input: &str) -> Redacted {
        let mut text = input.to_owned();
        let mut hits = Vec::new();

        for rule in builtin_rules().iter().chain(self.extra.iter()) {
            if rule.pattern.is_match(&text) {
                text = rule
                    .pattern
                    .replace_all(&text, rule.replacement)
                    .into_owned();
                hits.push(rule.name);
            }
        }

        Redacted { text, hits }
    }
}

/// The built-in rule set, compiled once.
///
/// Order matters: specific credential shapes run before the generic
/// `key = value` rule, so a matched token is labelled by what it actually is.
fn builtin_rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let rule = |name, pattern: &str, replacement| Rule {
            name,
            // Patterns are compile-time constants in this function; a failure
            // here is a bug in this file, not a runtime condition.
            pattern: Regex::new(pattern).expect("built-in redaction pattern is valid"),
            replacement,
        };

        vec![
            rule(
                "private-key",
                r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
                "[redacted:private-key]",
            ),
            rule(
                "anthropic-key",
                r"sk-ant-[A-Za-z0-9_\-]{16,}",
                "[redacted:anthropic-key]",
            ),
            rule(
                "openai-key",
                r"\bsk-[A-Za-z0-9]{20,}\b",
                "[redacted:openai-key]",
            ),
            rule(
                "github-token",
                r"\bgh[pousr]_[A-Za-z0-9]{16,}\b",
                "[redacted:github-token]",
            ),
            rule(
                "slack-token",
                r"\bxox[baprs]-[A-Za-z0-9\-]{10,}\b",
                "[redacted:slack-token]",
            ),
            rule(
                "aws-access-key",
                r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
                "[redacted:aws-access-key]",
            ),
            rule(
                "jwt",
                r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
                "[redacted:jwt]",
            ),
            rule(
                "auth-header",
                r"(?i)(?P<head>authorization\s*:\s*(?:bearer|basic|token)\s+)\S+",
                "${head}[redacted]",
            ),
            rule(
                "url-credentials",
                r"(?P<scheme>[a-zA-Z][a-zA-Z0-9+.\-]*://)[^/\s:@]+:[^/\s@]+@",
                "${scheme}[redacted]@",
            ),
            rule(
                "assignment",
                // The secret word can sit anywhere inside the identifier, which
                // is why it is wrapped in wildcards rather than anchored: real
                // names look like `AWS_SECRET_ACCESS_KEY`, `db.password`, or
                // `githubToken`, and a `\b` would not fire inside any of them
                // because `_` is itself a word character. An optional quote sits
                // on both sides of the separator so JSON (`"api_key": "…"`) is
                // caught as well as shell (`API_KEY=…`).
                r#"(?i)(?P<head>[A-Za-z0-9_.\-]*(?:api[_\-]?key|access[_\-]?key|secret|token|password|passwd|pwd|credential|passphrase)[A-Za-z0-9_.\-]*["']?\s*[:=]\s*["']?)[^\s"',;]{6,}"#,
                "${head}[redacted]",
            ),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redact(input: &str) -> Redacted {
        Redactor::new().redact(input)
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        let result = redact("cargo build --workspace, then run the tests");
        assert!(result.is_clean());
        assert_eq!(result.text(), "cargo build --workspace, then run the tests");
    }

    #[test]
    fn provider_keys_are_removed() {
        let cases = [
            (
                "sk-ant-api03-abcdefghijklmnopqrstuvwxyz012345",
                "anthropic-key",
            ),
            ("sk-abcdefghijklmnopqrstuvwxyz0123", "openai-key"),
            ("ghp_abcdefghijklmnopqrstuvwxyz0123456789", "github-token"),
            ("xoxb-1234567890-abcdefghijkl", "slack-token"),
            ("AKIAIOSFODNN7EXAMPLE", "aws-access-key"),
        ];
        for (secret, rule) in cases {
            let result = redact(&format!("the key is {secret} ok"));
            assert!(
                !result.text().contains(secret),
                "{rule} leaked: {}",
                result.text()
            );
            assert!(result.hits().contains(&rule), "{rule} did not fire");
        }
    }

    #[test]
    fn private_key_blocks_are_removed_whole() {
        let input = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nlines\n-----END RSA PRIVATE KEY-----\nafter";
        let result = redact(input);
        assert!(!result.text().contains("MIIEow"));
        assert!(result.text().starts_with("before"));
        assert!(result.text().ends_with("after"));
    }

    #[test]
    fn assignments_keep_their_key_and_lose_their_value() {
        let result = redact("DATABASE_PASSWORD=hunter2000swordfish");
        assert!(!result.text().contains("hunter2000swordfish"));
        assert!(result.text().contains("DATABASE_PASSWORD="));
    }

    #[test]
    fn the_secret_word_is_found_anywhere_in_the_identifier() {
        // Every one of these is a name that occurs in real configuration, and
        // in none of them does the telling word sit at a word boundary.
        for (line, secret) in [
            (
                "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIK7MDENGbPxRfiCY",
                "wJalrXUtnFEMIK7MDENGbPxRfiCY",
            ),
            ("db.password: hunter2000swordfish", "hunter2000swordfish"),
            ("githubToken = ghtoken1234567890", "ghtoken1234567890"),
            ("MY_APP_CREDENTIALS=abcdef123456", "abcdef123456"),
        ] {
            let result = redact(line);
            assert!(
                !result.text().contains(secret),
                "leaked in: {}",
                result.text()
            );
        }
    }

    #[test]
    fn quoted_assignments_are_handled() {
        let result = redact(r#"{"api_key": "abcdef123456", "model": "opus"}"#);
        assert!(!result.text().contains("abcdef123456"));
        assert!(result.text().contains("model"));
        assert!(result.text().contains("opus"));
    }

    #[test]
    fn auth_headers_keep_their_scheme() {
        let result = redact("Authorization: Bearer abcdefghijklmnopqrstuvwxyz");
        assert!(!result.text().contains("abcdefghijklmnopqrstuvwxyz"));
        assert!(result.text().to_lowercase().contains("bearer"));
    }

    #[test]
    fn credentials_in_urls_are_removed() {
        let result = redact("https://someone:s3cr3t-token@github.com/acme/api.git");
        assert!(!result.text().contains("s3cr3t-token"));
        assert!(result.text().contains("github.com/acme/api.git"));
    }

    #[test]
    fn jwts_are_removed() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let result = redact(&format!("cookie={jwt}"));
        assert!(
            !result
                .text()
                .contains("dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk")
        );
    }

    #[test]
    fn hits_name_rules_without_echoing_secrets() {
        let result = redact("token=abcdef123456 and AKIAIOSFODNN7EXAMPLE");
        for hit in result.hits() {
            assert!(!hit.contains("abcdef"));
            assert!(!hit.contains("AKIA"));
        }
        assert!(!result.is_clean());
    }

    #[test]
    fn extra_patterns_are_applied() {
        let redactor =
            Redactor::new().with_pattern("internal-id", Regex::new(r"EMP-\d{6}").unwrap());
        let result = redactor.redact("employee EMP-123456 filed it");
        assert!(!result.text().contains("EMP-123456"));
        assert!(result.hits().contains(&"internal-id"));
    }

    #[test]
    fn redaction_is_idempotent() {
        let once = redact("password=correct-horse-battery");
        let twice = redact(once.text());
        assert_eq!(once.text(), twice.text());
    }
}
