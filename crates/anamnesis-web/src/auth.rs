//! Who is allowed to deliver events and collect handoffs.
//!
//! The memory this server holds is a transcript of everything someone typed at
//! their editor, and until now anything that could reach the port could read
//! it. On loopback that is nearly the whole story — the port is the boundary —
//! but "nearly" stops being true the moment the server is bound to an address
//! other than localhost, or shared by two people.
//!
//! Three decisions shape this module.
//!
//! **Absence is not an error.** A server with no tokens configured accepts
//! everyone, exactly as it did before this existed. The alternative — refusing
//! to start until someone sets a secret — would break every running install by
//! turning hooks into 401s, and a hook that fails is invisible: capture just
//! stops. The one place that trade is refused is a non-loopback bind, which the
//! CLI checks, because there the open door faces a network.
//!
//! **A token may carry a name.** `ANAMNESIS_TOKEN` is one shared secret and
//! identifies nobody; `ANAMNESIS_TOKENS` maps names to secrets, so a shared
//! server can tell whose session it is recording. Nothing here uses the name
//! yet beyond reporting it, but the identity has to be established at the door
//! or it cannot be established at all.
//!
//! **Configuration that was attempted and got wrong is fatal.** An entry
//! missing its `=`, an empty secret, two operators sharing one — each is a
//! typo whose silent interpretation is a server that is less protected than
//! its operator believes. They stop startup. What is never printed back is the
//! secret itself: error messages name the position or the operator, never the
//! value, because startup output is the thing people paste into issues.

use std::sync::Arc;

use anamnesis_core::scope::OperatorName;
use base64::Engine;
use secrecy::{ExposeSecret, SecretString};
use subtle::ConstantTimeEq;

/// Environment variable holding the secret a client presents.
///
/// Read by `anamnesis hook` and by the server, which accepts it as a token
/// belonging to no particular operator.
pub const TOKEN_ENV: &str = "ANAMNESIS_TOKEN";

/// Environment variable holding the `name=secret` pairs a server accepts.
pub const TOKENS_ENV: &str = "ANAMNESIS_TOKENS";

/// Prefix on generated tokens, so a secret found in a settings file or a log
/// can be recognised for what it is.
const TOKEN_PREFIX: &str = "anam_";

/// Bytes of randomness in a generated token.
const TOKEN_BYTES: usize = 32;

/// A token configuration that cannot be honoured.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The environment describes tokens that cannot be used as written.
    #[error("{0}")]
    Config(String),

    /// The system refused to provide randomness for a new token.
    #[error("could not generate a token: {0}")]
    Random(String),
}

/// Who the server believes a request is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// No tokens are configured, so the request identifies nobody and was
    /// not asked to.
    Anonymous,

    /// A valid token that names no operator.
    Unnamed,

    /// A valid token belonging to this operator.
    Operator(OperatorName),
}

impl Identity {
    /// The operator this request belongs to, when the token named one.
    pub fn operator(&self) -> Option<&OperatorName> {
        match self {
            Self::Operator(name) => Some(name),
            Self::Anonymous | Self::Unnamed => None,
        }
    }

    /// Whether the server is accepting unauthenticated requests.
    pub fn is_anonymous(&self) -> bool {
        matches!(self, Self::Anonymous)
    }
}

/// Why a request was turned away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// Tokens are required and the request carried no `Authorization` header.
    Missing,

    /// The header was there but was not `Bearer <token>`.
    Malformed,

    /// A token was presented and is not one this server accepts.
    Unknown,
}

impl Rejection {
    /// What the caller is told, which for a hook is a line on someone's
    /// stderr and the only clue they get about why capture stopped.
    pub fn message(self) -> String {
        match self {
            Self::Missing => {
                format!(
                    "this server requires a token; set {TOKEN_ENV} for the process that runs the hooks"
                )
            }
            Self::Malformed => "Authorization header is not `Bearer <token>`".to_owned(),
            Self::Unknown => {
                format!(
                    "token was not recognised; check {TOKEN_ENV} against the server's {TOKENS_ENV}"
                )
            }
        }
    }
}

/// One accepted secret, and whose it is.
struct Credential {
    /// The operator the secret belongs to, when it belongs to one.
    operator: Option<OperatorName>,
    /// The secret itself.
    secret: SecretString,
}

/// The tokens this server accepts.
///
/// Cloned into every request; the credentials themselves are shared.
#[derive(Clone, Default)]
pub struct Auth {
    /// Empty means open: every request is [`Identity::Anonymous`].
    credentials: Arc<Vec<Credential>>,
}

impl std::fmt::Debug for Auth {
    /// Never renders the secrets. A `Debug` that did would put them into the
    /// first log line someone pastes into an issue.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("credentials", &self.credentials.len())
            .field("named", &self.named().count())
            .finish()
    }
}

impl Auth {
    /// Accept everyone, which is what a server with no tokens configured does.
    pub fn open() -> Self {
        Self::default()
    }

    /// Read the accepted tokens from the environment.
    pub fn from_env() -> Result<Self, AuthError> {
        let single = std::env::var(TOKEN_ENV).ok();
        let named = std::env::var(TOKENS_ENV).ok();
        Self::parse(single.as_deref(), named.as_deref())
    }

    /// Build from the two variables' raw values.
    ///
    /// Separate from [`Self::from_env`] so the rules can be tested without a
    /// process-wide environment, which no two tests can share.
    pub fn parse(single: Option<&str>, named: Option<&str>) -> Result<Self, AuthError> {
        let mut credentials: Vec<Credential> = Vec::new();

        if let Some(value) = named {
            // Set but empty is someone who tried, exactly as it is for the
            // single-secret variable below: read as "unset" it would start a
            // server that is open while its operator believes it is not.
            if value.trim().is_empty() {
                return Err(AuthError::Config(format!(
                    "{TOKENS_ENV} is set to an empty value; list `name=secret` pairs or unset it"
                )));
            }

            for (position, entry) in value
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|entry| !entry.is_empty())
                .enumerate()
            {
                // Split at the *first* `=`: a base64 secret can end in one, and
                // an operator name can never contain one.
                let Some((name, secret)) = entry.split_once('=') else {
                    return Err(AuthError::Config(format!(
                        "{TOKENS_ENV} entry {} is not `name=secret`",
                        position + 1
                    )));
                };
                let operator = OperatorName::parse(name)
                    .map_err(|error| AuthError::Config(format!("{TOKENS_ENV}: {error}")))?;
                if secret.is_empty() {
                    return Err(AuthError::Config(format!(
                        "{TOKENS_ENV}: operator {operator} has an empty secret"
                    )));
                }
                if credentials
                    .iter()
                    .any(|existing| existing.operator.as_ref() == Some(&operator))
                {
                    return Err(AuthError::Config(format!(
                        "{TOKENS_ENV}: operator {operator} is listed twice"
                    )));
                }
                credentials.push(Credential {
                    operator: Some(operator),
                    secret: SecretString::from(secret),
                });
            }
        }

        if let Some(value) = single {
            // An empty value is someone who tried. Left to mean "unset" it
            // would run the server open while its operator believed otherwise
            // — which is the whole failure this module exists to prevent.
            if value.trim().is_empty() {
                return Err(AuthError::Config(format!(
                    "{TOKEN_ENV} is set to an empty value; give it a secret or unset it"
                )));
            }
            credentials.push(Credential {
                operator: None,
                secret: SecretString::from(value),
            });
        }

        // Two credentials on one secret means the server cannot tell who is
        // calling, and would answer with whichever it happened to check first.
        for (index, credential) in credentials.iter().enumerate() {
            let duplicate = credentials[index + 1..]
                .iter()
                .any(|other| other.secret.expose_secret() == credential.secret.expose_secret());
            if duplicate {
                return Err(AuthError::Config(
                    "the same secret is configured twice; a secret identifies exactly one caller"
                        .to_owned(),
                ));
            }
        }

        Ok(Self {
            credentials: Arc::new(credentials),
        })
    }

    /// Whether this server accepts unauthenticated requests.
    pub fn is_open(&self) -> bool {
        self.credentials.is_empty()
    }

    /// How many secrets are accepted.
    pub fn len(&self) -> usize {
        self.credentials.len()
    }

    /// Whether no secret is accepted, which is the same as being open.
    pub fn is_empty(&self) -> bool {
        self.credentials.is_empty()
    }

    /// The operators this server knows by name.
    pub fn named(&self) -> impl Iterator<Item = &OperatorName> {
        self.credentials
            .iter()
            .filter_map(|credential| credential.operator.as_ref())
    }

    /// Decide who a request is, given its `Authorization` header.
    pub fn authenticate(&self, header: Option<&str>) -> Result<Identity, Rejection> {
        if self.is_open() {
            return Ok(Identity::Anonymous);
        }

        let header = header.ok_or(Rejection::Missing)?;
        let presented = bearer(header).ok_or(Rejection::Malformed)?;
        self.identify(presented)
    }

    /// The same decision, for a request a person's browser made.
    ///
    /// Also accepts the token as an HTTP Basic password. A browser cannot be
    /// asked to attach a bearer token to a link somebody clicked, so without
    /// this the wiki browser would be unreachable on exactly the servers that
    /// took the trouble to configure a token.
    ///
    /// The username is ignored. The secret is the whole credential — that is
    /// what `ANAMNESIS_TOKENS` maps to an operator — and a username that had
    /// to match as well would only add a way to fail whose error message
    /// cannot say which half was wrong without saying something about the
    /// other. Only the browser routes call this; a credential a browser
    /// attaches by itself must not be able to authorise `POST /hook`.
    pub fn authenticate_browser(&self, header: Option<&str>) -> Result<Identity, Rejection> {
        if self.is_open() {
            return Ok(Identity::Anonymous);
        }

        let header = header.ok_or(Rejection::Missing)?;
        let presented = match bearer(header) {
            Some(token) => token.to_owned(),
            None => basic_password(header).ok_or(Rejection::Malformed)?,
        };
        self.identify(&presented)
    }

    /// Whose secret this is, if it is one this server accepts.
    fn identify(&self, presented: &str) -> Result<Identity, Rejection> {
        // Every credential is compared, and each comparison is constant-time.
        // Returning on the first match would make the server's answer depend on
        // where in the list the caller's secret sits.
        let mut matched: Option<&Credential> = None;
        for credential in self.credentials.iter() {
            let hit: bool = presented
                .as_bytes()
                .ct_eq(credential.secret.expose_secret().as_bytes())
                .into();
            if hit && matched.is_none() {
                matched = Some(credential);
            }
        }

        match matched {
            None => Err(Rejection::Unknown),
            Some(credential) => Ok(match &credential.operator {
                Some(operator) => Identity::Operator(operator.clone()),
                None => Identity::Unnamed,
            }),
        }
    }
}

/// The token out of an `Authorization: Bearer <token>` header.
///
/// The scheme is compared case-insensitively because HTTP says it is, and
/// clients differ on how they spell it.
fn bearer(header: &str) -> Option<&str> {
    let (scheme, token) = header.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then_some(token)
}

/// The password out of an `Authorization: Basic <base64 user:password>` header.
///
/// The username is dropped without being looked at: see
/// [`Auth::authenticate_browser`]. A password containing a colon survives —
/// the first colon separates the two halves and the rest belongs to the
/// secret, which matters because a generated token is base64url and a pasted
/// one can be anything.
fn basic_password(header: &str) -> Option<String> {
    let (scheme, encoded) = header.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (_user, password) = decoded.split_once(':')?;
    (!password.is_empty()).then(|| password.to_owned())
}

/// Mint a token nobody has to invent.
///
/// 32 bytes of system randomness, base64url so it survives being pasted into a
/// shell, an environment file, and JSON without quoting or escaping, and
/// prefixed so it is recognisable as an anamnesis secret wherever it turns up.
pub fn generate_token() -> Result<String, AuthError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|error| AuthError::Random(error.to_string()))?;
    Ok(format!(
        "{TOKEN_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operator(name: &str) -> OperatorName {
        OperatorName::parse(name).expect("valid operator name")
    }

    #[test]
    fn no_configuration_accepts_everyone() {
        let auth = Auth::parse(None, None).expect("parse");
        assert!(auth.is_open());
        assert_eq!(auth.authenticate(None), Ok(Identity::Anonymous));
        // Even a wrong token: an open server was never asked to check.
        assert_eq!(
            auth.authenticate(Some("Bearer whatever")),
            Ok(Identity::Anonymous)
        );
    }

    #[test]
    fn a_shared_secret_identifies_nobody_but_is_still_required() {
        let auth = Auth::parse(Some("s3cret"), None).expect("parse");
        assert!(!auth.is_open());
        assert_eq!(
            auth.authenticate(Some("Bearer s3cret")),
            Ok(Identity::Unnamed)
        );
        assert_eq!(auth.authenticate(None), Err(Rejection::Missing));
        assert_eq!(
            auth.authenticate(Some("Bearer nope")),
            Err(Rejection::Unknown)
        );
    }

    #[test]
    fn a_named_token_says_whose_it_is() {
        let auth = Auth::parse(None, Some("alice=alpha,bob=beta")).expect("parse");
        assert_eq!(
            auth.authenticate(Some("Bearer beta")),
            Ok(Identity::Operator(operator("bob")))
        );
        assert_eq!(
            auth.authenticate(Some("Bearer alpha"))
                .map(|id| id.operator().cloned()),
            Ok(Some(operator("alice")))
        );
    }

    #[test]
    fn named_tokens_may_be_separated_by_whitespace_or_newlines() {
        // Written across lines is how a list of operators is actually kept.
        let auth = Auth::parse(None, Some("alice=alpha\n  bob=beta\n")).expect("parse");
        assert_eq!(auth.len(), 2);
        assert_eq!(
            auth.named().cloned().collect::<Vec<_>>(),
            vec![operator("alice"), operator("bob")]
        );
    }

    #[test]
    fn a_secret_may_contain_the_separator_it_was_split_on() {
        // Base64 padding ends in `=`, and splitting on the last one would
        // quietly hand out a truncated secret that never matches.
        let auth = Auth::parse(None, Some("alice=YWxpY2U=")).expect("parse");
        assert_eq!(
            auth.authenticate(Some("Bearer YWxpY2U=")),
            Ok(Identity::Operator(operator("alice")))
        );
    }

    #[test]
    fn the_scheme_is_matched_case_insensitively_and_a_bare_token_is_not_one() {
        let auth = Auth::parse(Some("s3cret"), None).expect("parse");
        assert_eq!(
            auth.authenticate(Some("bearer s3cret")),
            Ok(Identity::Unnamed)
        );
        assert_eq!(auth.authenticate(Some("s3cret")), Err(Rejection::Malformed));
        assert_eq!(
            auth.authenticate(Some("Basic s3cret")),
            Err(Rejection::Malformed)
        );
        assert_eq!(
            auth.authenticate(Some("Bearer   ")),
            Err(Rejection::Malformed)
        );
    }

    /// What a browser sends after somebody types the token into the prompt.
    #[test]
    fn a_browser_may_present_the_token_as_a_basic_password() {
        let auth = Auth::parse(None, Some("alice=s3cret")).expect("parse");
        let header = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("anyone:s3cret")
        );

        assert_eq!(
            auth.authenticate_browser(Some(&header)),
            Ok(Identity::Operator(operator("alice")))
        );
        // The API keeps the header-only rule, so a credential the browser
        // attaches on its own cannot reach `POST /hook`.
        assert_eq!(auth.authenticate(Some(&header)), Err(Rejection::Malformed));
    }

    /// The username is not a second half of the secret.
    #[test]
    fn any_username_will_do_and_a_wrong_token_still_fails() {
        let auth = Auth::parse(None, Some("alice=s3cret")).expect("parse");

        for user in ["alice", "bob", ""] {
            let header = format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:s3cret"))
            );
            assert_eq!(
                auth.authenticate_browser(Some(&header)),
                Ok(Identity::Operator(operator("alice"))),
                "username {user:?}"
            );
        }

        let wrong = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("alice:not-it")
        );
        assert_eq!(
            auth.authenticate_browser(Some(&wrong)),
            Err(Rejection::Unknown)
        );
    }

    /// A generated token is base64url and a pasted one can be anything, so the
    /// split has to be at the first colon and no other.
    #[test]
    fn a_password_may_contain_a_colon() {
        let auth = Auth::parse(Some("a:b:c"), None).expect("parse");
        let header = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("user:a:b:c")
        );

        assert_eq!(
            auth.authenticate_browser(Some(&header)),
            Ok(Identity::Unnamed)
        );
    }

    #[test]
    fn a_browser_route_still_takes_a_bearer_token() {
        let auth = Auth::parse(Some("s3cret"), None).expect("parse");

        assert_eq!(
            auth.authenticate_browser(Some("Bearer s3cret")),
            Ok(Identity::Unnamed)
        );
        assert_eq!(
            auth.authenticate_browser(Some("Basic not-base64!")),
            Err(Rejection::Malformed)
        );
        assert_eq!(auth.authenticate_browser(None), Err(Rejection::Missing));
    }

    /// An open server is open to a browser too: no prompt, nothing to type.
    #[test]
    fn a_server_with_no_tokens_lets_a_browser_in() {
        let auth = Auth::open();

        assert_eq!(auth.authenticate_browser(None), Ok(Identity::Anonymous));
    }

    #[test]
    fn an_entry_without_a_secret_is_a_configuration_error() {
        let error = Auth::parse(None, Some("alice")).expect_err("no separator");
        assert!(error.to_string().contains("entry 1"), "{error}");

        let error = Auth::parse(None, Some("alice=")).expect_err("empty secret");
        assert!(error.to_string().contains("alice"), "{error}");
    }

    #[test]
    fn an_error_never_repeats_the_secret_back() {
        // Startup output is what people paste into issues.
        let error = Auth::parse(None, Some("Alice Smith=hunter2")).expect_err("invalid name");
        assert!(!error.to_string().contains("hunter2"), "{error}");
    }

    #[test]
    fn an_empty_shared_secret_is_refused_rather_than_read_as_unset() {
        let error = Auth::parse(Some("   "), None).expect_err("empty");
        assert!(error.to_string().contains("unset it"), "{error}");
    }

    /// `ANAMNESIS_TOKENS=${ANAMNESIS_TOKENS:-}` in a compose file expands to
    /// this. Reading it as "unset" would start the server open.
    #[test]
    fn an_empty_operator_list_is_refused_too() {
        let error = Auth::parse(None, Some("")).expect_err("empty");
        assert!(error.to_string().contains("unset it"), "{error}");
    }

    #[test]
    fn one_secret_cannot_belong_to_two_callers() {
        let error = Auth::parse(None, Some("alice=same,bob=same")).expect_err("shared secret");
        assert!(error.to_string().contains("exactly one caller"), "{error}");

        let error = Auth::parse(Some("same"), Some("alice=same")).expect_err("shared secret");
        assert!(error.to_string().contains("exactly one caller"), "{error}");
    }

    #[test]
    fn an_operator_listed_twice_is_refused() {
        let error = Auth::parse(None, Some("alice=one,alice=two")).expect_err("duplicate");
        assert!(error.to_string().contains("listed twice"), "{error}");
    }

    #[test]
    fn debug_does_not_render_secrets() {
        let auth = Auth::parse(Some("s3cret"), Some("alice=alpha")).expect("parse");
        let rendered = format!("{auth:?}");
        assert!(!rendered.contains("s3cret"), "{rendered}");
        assert!(!rendered.contains("alpha"), "{rendered}");
    }

    #[test]
    fn generated_tokens_are_unique_and_recognisable() {
        let first = generate_token().expect("generate");
        let second = generate_token().expect("generate");
        assert_ne!(first, second);
        assert!(first.starts_with(TOKEN_PREFIX), "{first}");
        // Safe to paste anywhere without quoting.
        assert!(
            first
                .trim_start_matches(TOKEN_PREFIX)
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
            "{first}"
        );
    }
}
