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
                // The rule above only sees the original shape. Project,
                // service-account, and admin keys put a hyphenated word
                // between the prefix and the secret, which ends the run of
                // alphanumerics that rule counts on — so every key OpenAI has
                // issued since projects existed went through untouched. Each
                // form is named rather than the middle being made optional:
                // `sk-` followed by anything long enough would start redacting
                // ordinary hyphenated identifiers out of somebody's prompt.
                "openai-scoped-key",
                r"\bsk-(?:proj|svcacct|admin)-[A-Za-z0-9_\-]{20,}",
                "[redacted:openai-key]",
            ),
            rule(
                // Google's shape is fixed at 39 characters and the prefix is
                // theirs alone, so this cannot fire on anything else. It
                // belongs here because this project already speaks to Gemini
                // through a harness and a provider.
                "google-api-key",
                r"\bAIza[0-9A-Za-z_\-]{35}\b",
                "[redacted:google-api-key]",
            ),
            rule(
                // The API key above is not the only Google credential that
                // reaches a prompt, and the other one is the one people hold
                // in a terminal: an OAuth access token, handed out by a
                // `gcloud` command, an AI Studio page, or a curl somebody
                // pasted. It authorises the same APIs and it does not begin
                // with `AIza`, so the rule above matched none of it.
                //
                // This was found the way these are always found. A real token
                // arrived in a prompt, went through the sanitizer untouched,
                // and was written to `raw/` in full — the append-only copy
                // that outlives the index, where redaction is the only
                // defence there is.
                "google-oauth-token",
                r"\bya29\.[0-9A-Za-z_\-]{20,}",
                "[redacted:google-oauth-token]",
            ),
            rule(
                // The shape the leak actually had. Google does not document
                // it the way `ya29.` is documented, so the floor is higher
                // here on purpose: the prefix is two letters and a dot, and a
                // short match would start redacting ordinary prose. Thirty
                // trailing characters is longer than anything that reaches
                // `AQ.` by accident and shorter than any credential of this
                // shape observed.
                "google-oauth-token",
                r"\bAQ\.[0-9A-Za-z_\-]{30,}",
                "[redacted:google-oauth-token]",
            ),
            rule(
                "stripe-key",
                r"\b[sr]k_(?:live|test)_[A-Za-z0-9]{16,}\b",
                "[redacted:stripe-key]",
            ),
            rule(
                "npm-token",
                r"\bnpm_[A-Za-z0-9]{30,}\b",
                "[redacted:npm-token]",
            ),
            rule(
                // A webhook URL is a bearer credential wearing a path: anyone
                // holding it can post as that integration, and it travels in
                // documentation and pasted commands where nothing looks like a
                // secret.
                "slack-webhook",
                r"https://hooks\.slack\.com/services/[A-Za-z0-9/+_\-]{20,}",
                "[redacted:slack-webhook]",
            ),
            rule(
                // This system's own token. A memory that records prompts and
                // shell output is exactly where the key to it ends up — in an
                // export line, a curl, a settings file somebody pasted — and
                // storing that would hand the reader of one session the run of
                // every other. `anamnesis token` mints this shape on purpose,
                // so it is recognisable wherever it turns up.
                "anamnesis-token",
                r"\banam_[A-Za-z0-9_\-]{20,}",
                "[redacted:anamnesis-token]",
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

    /// Every key OpenAI has issued since projects existed has a hyphenated
    /// word between the prefix and the secret, which is exactly what the
    /// original rule could not see past.
    #[test]
    fn scoped_provider_keys_are_removed_too() {
        for key in [
            "sk-proj-abcdefghij0123456789ABCDEFGHIJ",
            "sk-svcacct-abcdefghij0123456789ABCDEFGHIJ",
            "sk-admin-abcdefghij0123456789ABCDEFGHIJ",
        ] {
            let found = redact(&format!("the key is {key} and that is all"));
            assert!(!found.text().contains(key), "{key}: {}", found.text());
            assert!(found.text().contains("[redacted:openai-key]"));
        }
    }

    /// The line between a secret and a hyphenated identifier is the named
    /// prefix; without it this rule would start eating ordinary prose.
    #[test]
    fn an_ordinary_hyphenated_name_is_not_a_key() {
        let found = redact("the branch is sk-refactor-the-storage-layer-again");

        assert!(found.is_clean(), "{}", found.text());
    }

    #[test]
    fn keys_from_the_other_providers_are_removed() {
        // The webhook is assembled rather than written out. A literal one here
        // is indistinguishable from a real one to anything scanning this file
        // — GitHub's push protection refused the commit that had it — which is
        // the same reason the rule below exists at all.
        let webhook = format!(
            "https://hooks.{}.com/{}/{}",
            "slack", "services", "T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX"
        );
        let cases = [
            ("AIzaSyA0123456789abcdefghijklmnopqrstuv", "google-api-key"),
            ("ya29.a0Ae4lvC0123456789abcdefghij", "google-oauth-token"),
            (
                "AQ.Ab0123456789abcdefghijklmnopqrstuvwx",
                "google-oauth-token",
            ),
            ("sk_live_0123456789abcdefghij", "stripe-key"),
            ("npm_0123456789abcdefghijklmnopqrstuvwxyz", "npm-token"),
            (webhook.as_str(), "slack-webhook"),
        ];

        for (secret, name) in cases {
            let found = redact(&format!("value: {secret}"));
            assert!(!found.text().contains(secret), "{name}: {}", found.text());
            assert!(found.hits().contains(&name), "{name}: {:?}", found.hits());
        }
    }

    /// `AQ.` is the loosest prefix in the set — two letters and a dot — so
    /// the only thing keeping it from eating prose is the length floor. A
    /// floor is a claim until both sides of it are shown, which is why this
    /// asserts the character below it as well as the one above.
    #[test]
    fn the_floor_under_that_prefix_is_where_it_says_it_is() {
        let under = format!("AQ.{}", "a".repeat(29));
        let over = format!("AQ.{}", "a".repeat(30));

        let kept = redact(&format!("ticket {under} was filed"));
        assert!(kept.text().contains(&under), "{}", kept.text());
        assert!(kept.is_clean(), "{:?}", kept.hits());

        let taken = redact(&format!("ticket {over} was filed"));
        assert!(!taken.text().contains(&over), "{}", taken.text());
        assert!(
            taken.hits().contains(&"google-oauth-token"),
            "{:?}",
            taken.hits()
        );
    }

    /// The key to this memory is the one secret guaranteed to be in reach of
    /// the thing capturing prompts and shell output.
    #[test]
    fn this_systems_own_token_is_redacted() {
        let found = redact("run it with ANAMNESIS_TOKEN=anam_0123456789abcdefghijKLMNOP now");

        assert!(
            !found.text().contains("anam_0123456789"),
            "{}",
            found.text()
        );
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
