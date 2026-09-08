//! The parts of talking to an HTTP API that every provider does the same way.
//!
//! Two providers, one bug each, was the alternative. Classifying a non-2xx
//! reply and deciding how long to wait before trying again are not Anthropic's
//! or OpenAI's — they are what any JSON API over HTTP needs, and the last time
//! this codebase kept a second hand-written copy of something (five enums'
//! stored forms, in the store) the copies drifted and nothing failed to
//! compile.

use std::time::Duration;

use serde_json::Value;

use crate::LlmError;

/// Longest a `retry-after` is honoured for.
///
/// The session is already over. A page that arrives a minute late is fine; one
/// that arrives an hour late has kept a task alive for an hour to say what a
/// deterministic summary already said.
const MAX_RETRY_AFTER: u64 = 60;

/// Longest a backed-off wait can grow to.
const MAX_BACKOFF: u64 = 30;

/// Read a `retry-after` header, if it is one we can act on.
///
/// Read before the body, because reading the body consumes the response — and
/// this header is the only trustworthy answer to how long a 429 wants.
pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Classify a non-2xx response.
///
/// Both APIs answer with `{"error": {"type": ..., "message": ...}}`, and a
/// body that is not JSON at all — a proxy's HTML, a local server's plain text
/// — is carried through as the message rather than being replaced by a parse
/// error about it, because the text is the only clue anyone has.
pub fn api_error(status: u16, body: &str, retry_after: Option<Duration>) -> LlmError {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let error = parsed.as_ref().and_then(|value| value.get("error"));

    let kind = error
        .and_then(|error| error.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(body)
        .to_owned();

    // The header is the trustworthy answer and is preferred, but it is not
    // the only one: Google states its wait inside the body and sends no
    // header at all, so a 429 asking for thirteen seconds was retried after
    // one and then two — two more refusals, out of a quota the wait was
    // there to protect.
    let retry_after = retry_after.or_else(|| stated_delay(error, &message));

    // Folded into the message rather than a field: the only consumers are a
    // log line and the retry loop, and the loop reads it back below.
    let message = match retry_after {
        Some(delay) => format!("{message} (retry after {}s)", delay.as_secs()),
        None => message,
    };

    LlmError::Api {
        status,
        kind,
        message,
    }
}

/// The wait an API stated in its body, when it sent no header for it.
///
/// Two shapes, both from Google, preferred in this order because the first is
/// a field and the second is a sentence: a `RetryInfo` detail carrying
/// `retryDelay`, and the message's own "Please retry in 12.9s".
///
/// Rounded **up**. The number is the earliest moment the API will accept
/// another request, so truncating 12.9 to 12 buys one more refusal for the
/// price of the wait.
fn stated_delay(error: Option<&Value>, message: &str) -> Option<Duration> {
    let from_details = error
        .and_then(|error| error.get("details"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|detail| detail.get("retryDelay").and_then(Value::as_str))
        .and_then(|delay| seconds(delay.trim_end_matches('s')));

    from_details.or_else(|| {
        message
            .rsplit_once("retry in ")
            .and_then(|(_, rest)| rest.split_once('s'))
            .and_then(|(seconds_text, _)| seconds(seconds_text))
    })
}

/// A count of seconds, whole or fractional, rounded up to the next second.
fn seconds(text: &str) -> Option<Duration> {
    let parsed: f64 = text.trim().parse().ok()?;
    if !parsed.is_finite() || parsed < 0.0 {
        return None;
    }
    Some(Duration::from_secs(parsed.ceil() as u64))
}

/// How long to wait before trying again.
///
/// Honours a `retry-after` the API sent, and otherwise backs off exponentially
/// from a second.
pub fn retry_delay(error: &LlmError, attempt: u32) -> Duration {
    if let LlmError::Api { message, .. } = error
        && let Some(seconds) = message
            .rsplit_once("(retry after ")
            .and_then(|(_, rest)| rest.split_once("s)"))
            .and_then(|(seconds, _)| seconds.parse::<u64>().ok())
    {
        return Duration::from_secs(seconds.min(MAX_RETRY_AFTER));
    }

    Duration::from_secs(2_u64.saturating_pow(attempt).min(MAX_BACKOFF))
}

/// One usage counter, defaulting to zero rather than failing a request over a
/// missing accounting field.
pub fn usage(payload: &Value, field: &str) -> u32 {
    payload
        .get("usage")
        .and_then(|usage| usage.get(field))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .try_into()
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_error_body_is_read_for_its_type_and_message() {
        let error = api_error(
            429,
            &json!({"error": {"type": "rate_limit_error", "message": "slow down"}}).to_string(),
            Some(Duration::from_secs(5)),
        );

        let LlmError::Api {
            status,
            kind,
            message,
        } = error
        else {
            panic!("expected an api error");
        };
        assert_eq!(status, 429);
        assert_eq!(kind, "rate_limit_error");
        assert!(message.contains("slow down"), "{message}");
        assert!(message.contains("retry after 5s"), "{message}");
    }

    /// The failure this was written for. Google sends no `retry-after`
    /// header and states the wait in the body instead, so the backoff started
    /// at one second and spent two more requests being refused — out of the
    /// twenty-a-day quota the wait exists to protect.
    #[test]
    fn a_wait_stated_in_the_body_is_honoured_when_no_header_carries_it() {
        let body = json!({"error": {
            "code": 429,
            "message": "You exceeded your current quota. Please retry in 12.912293287s.",
            "status": "RESOURCE_EXHAUSTED",
        }})
        .to_string();

        let error = api_error(429, &body, None);
        // Rounded up: 12 would be one refusal early.
        assert_eq!(retry_delay(&error, 0), Duration::from_secs(13));
    }

    /// The structured form of the same thing, which is preferred because it
    /// is a field rather than a sentence that could be reworded.
    #[test]
    fn a_retry_info_detail_is_preferred_over_the_sentence() {
        let body = json!({"error": {
            "message": "Quota exceeded. Please retry in 40s.",
            "details": [
                {"@type": "type.googleapis.com/google.rpc.QuotaFailure"},
                {"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "7s"},
            ],
        }})
        .to_string();

        let error = api_error(429, &body, None);
        assert_eq!(retry_delay(&error, 0), Duration::from_secs(7));
    }

    /// The header stays the trustworthy answer where there is one.
    #[test]
    fn a_header_outranks_anything_the_body_says() {
        let body = json!({"error": {"message": "Please retry in 30s."}}).to_string();
        let error = api_error(429, &body, Some(Duration::from_secs(3)));
        assert_eq!(retry_delay(&error, 0), Duration::from_secs(3));
    }

    /// Nothing stated is the ordinary case, and it must still back off rather
    /// than read a number out of prose that has none.
    #[test]
    fn an_error_that_states_no_wait_backs_off_as_before() {
        let body = json!({"error": {"message": "internal error"}}).to_string();
        let error = api_error(500, &body, None);
        assert_eq!(retry_delay(&error, 0), Duration::from_secs(1));
        assert_eq!(retry_delay(&error, 2), Duration::from_secs(4));
    }

    /// A local server behind a proxy answers with HTML, and that text is the
    /// only clue anyone gets. Replacing it with "expected JSON" would throw
    /// away the message and keep the complaint.
    #[test]
    fn a_body_that_is_not_json_is_carried_through_as_the_message() {
        let error = api_error(502, "<html>Bad Gateway</html>", None);
        let LlmError::Api { message, kind, .. } = error else {
            panic!("expected an api error");
        };
        assert_eq!(kind, "unknown");
        assert!(message.contains("Bad Gateway"), "{message}");
    }

    #[test]
    fn a_retry_after_is_honoured_and_capped() {
        let short = api_error(429, "{}", Some(Duration::from_secs(5)));
        assert_eq!(retry_delay(&short, 0), Duration::from_secs(5));

        let absurd = api_error(429, "{}", Some(Duration::from_secs(3_600)));
        assert_eq!(
            retry_delay(&absurd, 0),
            Duration::from_secs(MAX_RETRY_AFTER),
            "the session is over; an hour later is worth less than the deterministic page"
        );
    }

    #[test]
    fn without_a_retry_after_the_wait_grows_and_stops_growing() {
        let error = LlmError::Api {
            status: 500,
            kind: "server_error".to_owned(),
            message: "boom".to_owned(),
        };
        assert_eq!(retry_delay(&error, 0), Duration::from_secs(1));
        assert_eq!(retry_delay(&error, 2), Duration::from_secs(4));
        assert_eq!(retry_delay(&error, 20), Duration::from_secs(MAX_BACKOFF));
    }

    /// Which failures are worth waiting out. Kept beside the classifier that
    /// produces them: the split between "try again" and "this will not get
    /// better" is the only thing the retry loop reads.
    #[test]
    fn rate_limits_and_server_faults_are_retryable() {
        for status in [429, 500, 529] {
            assert!(api_error(status, "{}", None).is_retryable(), "{status}");
        }
        for status in [400, 401, 403, 404, 413] {
            assert!(!api_error(status, "{}", None).is_retryable(), "{status}");
        }
    }

    #[test]
    fn a_missing_usage_counter_is_zero_rather_than_an_error() {
        assert_eq!(usage(&json!({}), "input_tokens"), 0);
        assert_eq!(
            usage(&json!({"usage": {"input_tokens": 7}}), "input_tokens"),
            7
        );
    }
}
