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
    /// Whether the pattern is the credential itself — a prefix and a shape
    /// nothing else has — rather than the text around a value.
    shaped: bool,
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
            shaped: true,
        });
        self
    }

    /// Names of the rules that recognise a credential by its own shape and
    /// find one in `input`. Nothing is replaced.
    ///
    /// For text whose surroundings cannot be trusted: bytes read out of a
    /// database file or an archive, where JSON escaping and page boundaries
    /// put quotes and separators where the rules that read context —
    /// `key = value`, an `Authorization` header, credentials in a URL — do not
    /// expect them. Run over a backup of this project's memory, those rules
    /// matched their own `TOKENS='[redacted]'` and reported values in every
    /// archive, the clean ones included. A prefix nobody else uses has no
    /// context to misread, and a masked value no longer has its prefix.
    pub fn credentials_in(&self, input: &str) -> Vec<&'static str> {
        builtin_rules()
            .iter()
            .chain(self.extra.iter())
            .filter(|rule| rule.shaped && rule.pattern.is_match(input))
            .map(|rule| rule.name)
            .collect()
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
        let compiled = |name, pattern: &str, replacement, shaped| Rule {
            name,
            // Patterns are compile-time constants in this function; a failure
            // here is a bug in this file, not a runtime condition.
            pattern: Regex::new(pattern).expect("built-in redaction pattern is valid"),
            replacement,
            shaped,
        };
        // A credential recognised by its own shape, and a value recognised by
        // what surrounds it: see `Redactor::credentials_in` for why they differ.
        let rule = |name, pattern: &str, replacement| compiled(name, pattern, replacement, true);
        let context =
            |name, pattern: &str, replacement| compiled(name, pattern, replacement, false);

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
                // Google's older API key: fixed at 39 characters, and the
                // prefix is theirs alone, so this cannot fire on anything
                // else. Still matched although Google has stopped issuing it
                // to new keys — the ones already in circulation authorise the
                // same APIs, and a rule that stops recognising a credential
                // the day it stops being handed out protects nobody who has
                // one. The replacement is `google-auth-key`, two rules down.
                //
                // The end is a character that cannot continue the key, taken
                // and put back, rather than `\b`. A key's alphabet includes
                // `-`, a word boundary needs a word character on one side, and
                // `-` followed by a space has none — so a key ending in `-`,
                // about one in sixty-four of them, matched nothing and went
                // through whole. Found by a property test, not by a leak.
                "google-api-key",
                r"\bAIza[0-9A-Za-z_\-]{35}(?P<tail>[^0-9A-Za-z_\-]|$)",
                "[redacted:google-api-key]${tail}",
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
                // The shape the leak actually had, and *not* an OAuth token —
                // this rule was filed beside `ya29.` when it was written, on a
                // guess that has since been checked against the thing itself.
                // `AQ.` is Google's newer API key: AI Studio issues it in
                // place of `AIza`, which is being retired for new keys, and it
                // is what a `GEMINI_API_KEY` now looks like. Long-lived, no
                // OAuth flow anywhere near it.
                //
                // The distinction is not pedantry. This name is the only thing
                // a person sees in place of their secret — in a wiki page, in
                // the raw spool, in the log line saying which rule fired — and
                // `oauth-token` sends somebody looking for a flow their setup
                // does not have.
                //
                // Google does not document this shape the way `ya29.` is
                // documented, so the floor is higher here on purpose: the
                // prefix is two letters and a dot, and a short match would
                // start redacting ordinary prose. Thirty trailing characters
                // is longer than anything that reaches `AQ.` by accident and
                // shorter than any credential of this shape observed.
                "google-auth-key",
                r"\bAQ\.[0-9A-Za-z_\-]{30,}",
                "[redacted:google-auth-key]",
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
            context(
                "auth-header",
                r"(?i)(?P<head>authorization\s*:\s*(?:bearer|basic|token)\s+)\S+",
                "${head}[redacted]",
            ),
            context(
                // Up to the *last* `@` before the path, not the first. A
                // password typed into a connection string by hand often holds
                // an `@`, and so does a username that is an email address; the
                // drivers that read such strings split at the last one, and a
                // rule that split at the first left the rest of the password —
                // or, with a leading `@`, all of it — in the text.
                "url-credentials",
                r"(?P<scheme>[a-zA-Z][a-zA-Z0-9+.\-]*://)[^/\s:]+:[^/\s]*@",
                "${scheme}[redacted]@",
            ),
            // A password handed to a program on its command line. None of these
            // has a separator the assignment rule reads — `-p` is glued to its
            // value, `--password` and `-u` are followed by a space — so a shell
            // command that carried one was stored whole, and shell commands are
            // most of what a tool call records.
            //
            // Every value below stops at a quote, a backtick and a backslash as
            // well as at a space, because the text a hook redacts is the tool
            // input rendered as JSON: the command is one string field among
            // others, a line break in it is the two characters `\n`, and a value
            // that ran on would take the rest of the object with it. The stretch
            // between the program and its flag stops at a backslash for the same
            // reason, or it reaches across an escaped line break into another
            // line — measured on the spool, a comment naming `curl` reached a
            // `docker run -u 1000:1000` three lines down.
            //
            // And no value may open with `[`, so a masked value is not masked
            // again, nor with `<`, `$`, `{` or `-`, which are a placeholder, a
            // variable, a format argument and the next flag: `--password
            // <PASSWORD>` in a usage line and `-p{password}` in code name no
            // password.
            context(
                // `mysql -pSECRET`: the value is glued to the flag, and a bare `-p`
                // asks for it instead. Lower-case only — `-P` is the port.
                "command-line-credential",
                r#"(?P<head>\b(?i:mysql|mysqladmin|mysqldump|mariadb)\b[^\n"\\]{0,120}?\s-p)[^\s'"`\\\[<$\-{][^\s'"`\\]{2,}"#,
                "${head}[redacted]",
            ),
            context(
                "command-line-credential",
                r#"(?P<head>\bsshpass\b[^\n"\\]{0,64}?\s-p\s?)[^\s'"`\\\[<$\-{][^\s'"`\\]{2,}"#,
                "${head}[redacted]",
            ),
            context(
                // `curl -u user:secret`. Only after an HTTP client: `-u` is a uid
                // to `docker run -u 1000:1000`, and there it names no one.
                "command-line-credential",
                r#"(?P<head>\b(?i:curl|wget|http|xh)\b[^\n"\\]{0,200}?\s(?:-u|--user)[ =]['"]?[^\s:'"\\]{1,64}:)[^\s'"`\\\[@{][^\s'"`\\]{2,}"#,
                "${head}[redacted]",
            ),
            context(
                // `-p` is the password to `docker login` and the port to
                // `redis-cli`, whose password is `-a`; each flag is read only
                // after the program it means that to.
                "command-line-credential",
                r#"(?P<head>(?:\b(?i:docker|podman)\s+login\b[^\n"\\]{0,120}?\s-p|\bredis-cli\b[^\n"\\]{0,120}?\s-a)\s+)[^\s'"`\\\[<$\-{][^\s'"`\\]{2,}"#,
                "${head}[redacted]",
            ),
            context(
                // `--password SECRET`, with a space; the `=` form is an
                // assignment and the rule below takes it. A space is also how
                // prose names a flag — "pass the --token flag" — so the value
                // has to hold something other than lower-case letters, which
                // every word of a sentence is and nearly no credential is.
                "command-line-credential",
                r#"(?P<head>(?:^|\s)--(?:password|passwd|pwd|passphrase|db-password|token|auth-token|access-token|api-key|apikey|client-secret)\s+['"]?)[a-z]*[^a-z\s'"`\\\[<$\-{][^\s'"`\\]*"#,
                "${head}[redacted]",
            ),
            context(
                // A header copied out of a browser or a `curl -v` is a session,
                // and a session is a login. The whole value goes: which of its
                // pairs is the one that authenticates is the server's business.
                // It has to open on `name=`, which every cookie header does and
                // a sentence about cookies — "Cookie: the session lives here" —
                // does not.
                "cookie-header",
                r#"(?i)(?P<head>\b(?:set-)?cookie:[ \t]*)[^\s"'\\=;\[]{1,64}=[^"'\\\n]{4,400}"#,
                "${head}[redacted]",
            ),
            context(
                // A `.netrc` line, whole: `login` and `password` are ordinary
                // words, and only this order of all three is the file.
                "netrc-password",
                r#"(?i)(?P<head>\bmachine\s+\S+\s+login\s+\S+\s+password\s+)[^\s'"`\\\[<$][^\s'"`\\]{2,}"#,
                "${head}[redacted]",
            ),
            context(
                // A password said in a sentence — "the admin password is …" —
                // has no separator for the rules above to find. What keeps "the
                // password is wrong" out is the value: it has to hold a letter
                // and a digit or a symbol, and it ends at the first space. The
                // letter is what leaves "the password is 12 characters long"
                // alone, and a date after the word, which the spool had.
                "stated-password",
                r#"(?i)(?P<head>\b(?:password|passphrase|passwd)\s+(?:is|was)\s+[`'"]?)(?:\p{L}[^\s'"`\\]*?[\p{N}!#$%&*+=?@^_~]|[\p{N}!#$%&*+=?@^_~][^\s'"`\\]*?\p{L})[^\s'"`\\]*"#,
                "${head}[redacted]",
            ),
            context(
                // The same said in Turkish, which is how the person this runs
                // for writes: `şifre: …`, `veritabanı parolası …`, `API anahtarı
                // …`, with a colon, an equals sign or a space. The bare word
                // `anahtar` is too common to go on — `anahtar kelime` is a
                // keyword — so only the API key is named. The same value rule as
                // above keeps `şifre yok` and `parola gerekli` as they are; a
                // hyphen and a slash do not count as the symbol, or `şifre
                // e-postayla gelir` would lose its `e-postayla`.
                //
                // The word's start is a character that cannot be part of it,
                // taken and put back: the pattern language has no lookbehind.
                "stated-password",
                r#"(?i)(?P<head>(?:^|[^\p{L}\p{N}_])(?:[şs]ifre(?:si|m|miz|niz|yi)?|parola(?:s[ıi]|m|m[ıi]z|n[ıi]z|y[ıi])?|api\s+anahtar[ıi]?)\s*(?:[:=]\s*|\s)[`'"]?)(?:\p{L}[^\s'"`\\]*?[\p{N}!#$%&*+=?@^_~]|[\p{N}!#$%&*+=?@^_~][^\s'"`\\]*?\p{L})[^\s'"`\\]*"#,
                "${head}[redacted]",
            ),
            context(
                // A quoted value is everything between its quotes. The
                // unquoted rule below stops at a space, a comma or a
                // semicolon, which is right for `KEY=value` and wrong for
                // `password = "correct horse battery"`: it kept the whole value
                // when the first word was under six characters, and the rest
                // of it when it was not. Two rules, one per quote, because the
                // pattern language has no backreference to say "the same
                // quote again".
                "assignment",
                r#"(?i)(?P<head>[A-Za-z0-9_.\-]*(?:api[_\-]?key|access[_\-]?key|secret|token|password|passwd|pwd|credential|passphrase|[_.\-]pass\b)[A-Za-z0-9_.\-]*["']?\s*[:=]\s*)"[^"\n]{6,}""#,
                "${head}\"[redacted]\"",
            ),
            context(
                "assignment",
                r#"(?i)(?P<head>[A-Za-z0-9_.\-]*(?:api[_\-]?key|access[_\-]?key|secret|token|password|passwd|pwd|credential|passphrase|[_.\-]pass\b)[A-Za-z0-9_.\-]*["']?\s*[:=]\s*)'[^'\n]{6,}'"#,
                "${head}'[redacted]'",
            ),
            context(
                "assignment",
                // The secret word can sit anywhere inside the identifier, which
                // is why it is wrapped in wildcards rather than anchored: real
                // names look like `AWS_SECRET_ACCESS_KEY`, `db.password`, or
                // `githubToken`, and a `\b` would not fire inside any of them
                // because `_` is itself a word character. An optional quote sits
                // on both sides of the separator so JSON (`"api_key": "…"`) is
                // caught as well as shell (`API_KEY=…`).
                //
                // `pass` alone is a word inside `bypass` and `compass`, so it
                // counts only as the last part of a name — `DB_PASS`,
                // `smtp.pass` — which is how it is spelled when it is short for
                // password.
                r#"(?i)(?P<head>[A-Za-z0-9_.\-]*(?:api[_\-]?key|access[_\-]?key|secret|token|password|passwd|pwd|credential|passphrase|[_.\-]pass\b)[A-Za-z0-9_.\-]*["']?\s*[:=]\s*["']?)[^\s"',;]{6,}"#,
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

    /// Text read out of a file whose context is encoded — here, a masked
    /// assignment followed by more of the line — is where the context rules
    /// report a value that is not there. A credential by its shape is still
    /// found, and the same credential masked is not.
    #[test]
    fn credentials_are_found_by_shape_and_masked_text_is_not_a_credential() {
        let redactor = Redactor::new();
        let encoded = r#"{\"text\":\"export ANAMNESIS_TOKENS='[redacted]' && run\"}"#;
        assert!(!redact(encoded).is_clean(), "the context rules fire here");
        assert!(redactor.credentials_in(encoded).is_empty(), "{encoded}");

        let key = "AQ.0123456789abcdefghijklmnopqrstuvwxyz";
        let held = format!(r#"{{\"text\":\"use {key}\"}}"#);
        assert_eq!(redactor.credentials_in(&held), ["google-auth-key"]);
        assert!(redactor.credentials_in(redact(&held).text()).is_empty());
        assert!(redactor.credentials_in("password = hunter22").is_empty());
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
            ("AQ.0123456789abcdefghijklmnopqrstuvwxyz", "google-auth-key"),
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
            taken.hits().contains(&"google-auth-key"),
            "{:?}",
            taken.hits()
        );
    }

    /// The two Google prefixes are two credentials, and the name is the only
    /// thing a person sees in place of the secret. Calling an AI Studio key an
    /// OAuth token sends them looking for a flow their setup does not have, so
    /// the names are asserted rather than left to whoever edits the table
    /// above.
    #[test]
    fn each_google_credential_is_named_for_what_it_is() {
        let oauth = redact("token ya29.a0Ae4lvC0123456789abcdefghij here");
        assert_eq!(oauth.hits(), ["google-oauth-token"]);

        let key = redact("key AQ.0123456789abcdefghijklmnopqrstuvwxyz here");
        assert_eq!(key.hits(), ["google-auth-key"]);
        assert!(
            key.text().contains("[redacted:google-auth-key]"),
            "{}",
            key.text()
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

    /// The three leaks the property tests found, pinned as examples so each has
    /// a name when it comes back.
    #[test]
    fn a_password_holding_an_at_sign_is_removed_whole() {
        for (url, password) in [
            ("postgres://app:p@ssw0rd@db.internal/prod", "ssw0rd"),
            ("https://aaa:@a^**^@aaa.example/db", "a^**^"),
            (
                "redis://me@corp.example:hunter2secret@cache:6379",
                "hunter2secret",
            ),
        ] {
            let result = redact(url);
            assert!(
                !result.text().contains(password),
                "{url} → {}",
                result.text()
            );
        }

        // And a URL with an `@` but no password is not mistaken for one.
        let untouched = "ssh://git@github.com/acme/api.git and https://host:8080/a@b";
        assert_eq!(redact(untouched).text(), untouched);
    }

    #[test]
    fn a_google_key_ending_in_a_dash_is_removed() {
        let key = format!("AIza{}-", "a".repeat(34));
        let result = redact(&format!("GOOGLE_KEY {key} rest"));
        assert!(!result.text().contains(&key), "{}", result.text());
        assert!(
            result.text().ends_with(" rest"),
            "the character after the key is put back: {}",
            result.text()
        );
    }

    #[test]
    fn a_quoted_value_is_removed_between_its_quotes() {
        for (text, kept) in [
            (r#"password = "correct horse battery""#, "horse"),
            (r#"password="00a aaa""#, "aaa"),
            ("api_key: 'ab, cd; ef'", "cd"),
            (r#"{"clientSecret": "two words here", "id": 7}"#, "words"),
        ] {
            let result = redact(text);
            assert!(!result.text().contains(kept), "{text} → {}", result.text());
        }

        let json = redact(r#"{"clientSecret": "two words here", "id": 7}"#);
        assert!(json.text().contains(r#""id": 7"#), "{}", json.text());
    }

    /// Each of these was stored whole: the separator is a glued flag or a
    /// space, which the assignment rule does not read, and a shell command is
    /// most of what a tool call records.
    #[test]
    fn a_password_on_a_command_line_is_removed() {
        for (line, secret, kept) in [
            ("mysql -u root -pS3cretPass9 mydb", "S3cretPass9", "mydb"),
            (
                "run `mysql -u root -pS3cretPass9` now",
                "S3cretPass9",
                "` now",
            ),
            (
                "mysqldump -h db -u app -pS3cretPass9 app > dump.sql",
                "S3cretPass9",
                "dump.sql",
            ),
            (
                "sshpass -p hunter2xyz ssh deploy@box",
                "hunter2xyz",
                "deploy@box",
            ),
            ("sshpass -phunter2xyz scp a b", "hunter2xyz", "scp a b"),
            (
                "curl -u admin:Pa55word9 https://api.example",
                "Pa55word9",
                "https://api.example",
            ),
            (
                "curl -s --user 'admin:Pa55word9' https://api.example",
                "Pa55word9",
                "admin:",
            ),
            ("psql --password Hunter22x -h db", "Hunter22x", "-h db"),
            (
                "tool --api-key AbCd1234EfGh --verbose",
                "AbCd1234EfGh",
                "--verbose",
            ),
            (
                "docker login -u me -p Regi5tryPass registry.example",
                "Regi5tryPass",
                "registry.example",
            ),
            ("redis-cli -h cache -a R3disPass ping", "R3disPass", "ping"),
        ] {
            let found = redact(line);
            assert!(!found.text().contains(secret), "{line} → {}", found.text());
            assert!(found.text().contains(kept), "{line} → {}", found.text());
            assert!(
                found.hits().contains(&"command-line-credential"),
                "{line}: {:?}",
                found.hits()
            );
        }
    }

    /// The flags that carry a password elsewhere carry something else here,
    /// and a flag named in a sentence carries nothing at all.
    #[test]
    fn a_command_line_that_holds_no_password_is_left_alone() {
        for line in [
            "mysql -u root -p mydb",
            "mysql -h db -P3306 -u app",
            "redis-cli -h cache -p 6379 ping",
            "docker run -u 1000:1000 image",
            "git push -u origin main",
            "pass the --token flag to authenticate",
            "usage: tool --password <PASSWORD>",
            "tool --token $GITHUB_TOKEN",
            "echo $PASS | docker login -u me --password-stdin",
            "bypass_cache=enabled_everywhere",
            r#"format!("mysql -u root -p{password} app")"#,
            // A line break inside a JSON-rendered body is two characters, and
            // the program on the line above is not this line's program.
            r#"run curl first\n// then `docker run -u 1000:1000 image`"#,
        ] {
            let found = redact(line);
            assert_eq!(found.text(), line, "{:?}", found.hits());
        }
    }

    /// The text a hook redacts is the tool input rendered as JSON, so a value
    /// that ran past its closing quote would take the other fields with it.
    #[test]
    fn a_command_line_value_stops_at_the_quote_that_closes_the_command() {
        let rendered = r#"{"command":"mysql -u root -pS3cretPass9 mydb","description":"connect"}"#;
        let found = redact(rendered);

        assert!(!found.text().contains("S3cretPass9"), "{}", found.text());
        assert!(
            found.text().ends_with(r#" mydb","description":"connect"}"#),
            "{}",
            found.text()
        );
    }

    #[test]
    fn a_cookie_header_and_a_netrc_line_lose_their_values() {
        let cookie = redact("Cookie: session=9f8e7d6c5b4a39281706f5e4d3c2b1a0; theme=dark");
        assert!(!cookie.text().contains("9f8e7d6c"), "{}", cookie.text());
        assert!(cookie.text().starts_with("Cookie: "), "{}", cookie.text());

        let set = redact("< Set-Cookie: sid=abc123def456ghi; Path=/; HttpOnly");
        assert!(!set.text().contains("abc123def456ghi"), "{}", set.text());

        let netrc = redact("machine api.example login bob password Zx9plm42");
        assert!(!netrc.text().contains("Zx9plm42"), "{}", netrc.text());
        assert!(netrc.text().contains("login bob"), "{}", netrc.text());

        for line in [
            "Cookie: the session lives here",
            "the login page asks for the password first",
        ] {
            assert!(redact(line).is_clean(), "{line}");
        }
    }

    #[test]
    fn a_password_stated_in_a_sentence_is_removed() {
        for (line, secret) in [
            ("the admin password is Qw3rty!9 for now", "Qw3rty!9"),
            ("the passphrase was `blue7horse` yesterday", "blue7horse"),
            ("veritabanı şifresi: Gizli123!x", "Gizli123!x"),
            ("sifre=Gizli123!x", "Gizli123!x"),
            ("Şifre Gizli123!x olarak ayarlandı", "Gizli123!x"),
            ("parola: Kx8$mn2pq", "Kx8$mn2pq"),
            ("sunucunun parolası Kx8$mn2pq, değiştirme", "Kx8$mn2pq"),
            ("API anahtarı: AbCdEf1234567890XyZ", "AbCdEf1234567890XyZ"),
        ] {
            let found = redact(line);
            assert!(!found.text().contains(secret), "{line} → {}", found.text());
            assert!(
                found.hits().contains(&"stated-password"),
                "{line}: {:?}",
                found.hits()
            );
        }
    }

    /// A value has to hold a digit or a symbol to be taken for a password,
    /// which is what keeps a sentence about one whole.
    #[test]
    fn a_sentence_about_a_password_is_left_alone() {
        for line in [
            "the password is wrong",
            "the password was changed.",
            "şifre yok",
            "parola gerekli",
            "şifremi unuttum",
            "şifre e-postayla gelir",
            "şifre 3 kez yanlış girildi",
            "API anahtarı 2026-09-14'te reddedildi",
            "the password is 12 characters long",
            "anahtar kelime: rust2024",
        ] {
            let found = redact(line);
            assert_eq!(found.text(), line, "{:?}", found.hits());
        }
    }

    /// `pass` counts only as the last part of a name, which is how it is
    /// spelled when it is short for password.
    #[test]
    fn a_name_ending_in_pass_is_a_password() {
        for (line, secret) in [
            ("export DB_PASS=Zq8wErt6yU", "Zq8wErt6yU"),
            ("smtp.pass: Zq8wErt6yU", "Zq8wErt6yU"),
        ] {
            let found = redact(line);
            assert!(!found.text().contains(secret), "{line} → {}", found.text());
        }
    }

    /// The spool, the index and a page each run redaction over text that may
    /// already have been through it, so none of the rules above may match
    /// what it masked.
    #[test]
    fn the_rules_for_stated_and_typed_passwords_do_not_mask_twice() {
        for line in [
            "mysql -u root -pS3cretPass9 mydb",
            "sshpass -p hunter2xyz ssh deploy@box",
            "curl -u admin:Pa55word9 https://api.example",
            "psql --password Hunter22x -h db",
            "redis-cli -a R3disPass ping",
            "Cookie: session=9f8e7d6c5b4a39281706f5e4d3c2b1a0",
            "machine api.example login bob password Zx9plm42",
            "the admin password is Qw3rty!9",
            "şifre: Gizli123!x",
        ] {
            let once = redact(line);
            let twice = redact(once.text());
            assert_eq!(once.text(), twice.text(), "{line}");
            assert!(twice.is_clean(), "{line}: {:?}", twice.hits());
        }
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
