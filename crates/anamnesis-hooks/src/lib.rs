//! Parsing agent lifecycle hook payloads into observations.
//!
//! Hook payloads are the least stable input this system has: every harness
//! shapes them differently, and any of them may add fields between releases.
//! The parser is therefore permissive about structure and strict about
//! boundaries — unknown fields are ignored rather than rejected, but every body
//! is bounded and redacted before it leaves this crate.
//!
//! Rejecting an unrecognised payload would mean losing the session it came
//! from. Recording it as an unclassified observation loses only the
//! classification.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::PathBuf;

use anamnesis_core::observation::{BoundedBody, EventKind, ToolRef};
use anamnesis_core::sanitize::Redactor;
use anamnesis_core::session::AgentKind;
use serde_json::Value;

/// Errors produced while parsing a hook payload.
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    /// The payload was not a JSON object.
    #[error("hook payload must be a JSON object")]
    NotAnObject,

    /// The payload carried no session identifier to correlate on.
    #[error("hook payload has no session id")]
    MissingSessionId,
}

/// Convenience alias for results produced by this crate.
pub type Result<T> = std::result::Result<T, HookError>;

/// A hook payload, reduced to what the capture path needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedHook {
    /// Harness that sent it.
    pub agent: AgentKind,
    /// The harness's own session identifier.
    pub agent_session_id: String,
    /// Working directory the agent is running in, when reported.
    pub cwd: Option<PathBuf>,
    /// Lifecycle boundary this event represents.
    pub kind: EventKind,
    /// Tool involved, for tool events.
    pub tool: Option<ToolRef>,
    /// Redacted, bounded payload.
    pub body: BoundedBody,
    /// Redaction rules that fired, safe to log.
    pub redactions: Vec<&'static str>,
}

impl ParsedHook {
    /// Whether anything was redacted from this payload.
    pub fn was_redacted(&self) -> bool {
        !self.redactions.is_empty()
    }
}

/// Parse a hook payload sent by `agent`.
pub fn parse(agent: &AgentKind, raw: &Value) -> Result<ParsedHook> {
    let object = raw.as_object().ok_or(HookError::NotAnObject)?;

    let agent_session_id = string_field(object, "session_id")
        .or_else(|| string_field(object, "sessionId"))
        .ok_or(HookError::MissingSessionId)?;

    let kind = classify(string_field(object, "hook_event_name").as_deref());
    let tool = tool_from(object);
    let raw_body = body_for(kind, object);

    let redacted = Redactor::new().redact(&raw_body);
    let body = BoundedBody::truncating(redacted.text(), kind.body_limit());

    Ok(ParsedHook {
        agent: agent.clone(),
        agent_session_id,
        cwd: string_field(object, "cwd").map(PathBuf::from),
        kind,
        tool,
        body,
        redactions: redacted.hits().to_vec(),
    })
}

/// Map a harness event name onto a lifecycle boundary.
///
/// An unrecognised name becomes a notification rather than an error: a harness
/// that invents a new hook should still be captured, just not classified.
fn classify(name: Option<&str>) -> EventKind {
    let normalized = name.unwrap_or_default().to_ascii_lowercase();
    match normalized.as_str() {
        "sessionstart" | "session_start" => EventKind::SessionStart,
        "userpromptsubmit" | "user_prompt_submit" => EventKind::UserPrompt,
        "pretooluse" | "posttooluse" | "pre_tool_use" | "post_tool_use" => EventKind::ToolUse,
        "precompact" | "pre_compact" => EventKind::PreCompact,
        "postcompact" | "post_compact" => EventKind::PostCompact,
        "sessionend" | "session_end" => EventKind::SessionEnd,
        _ => EventKind::Notification,
    }
}

/// Extract the tool name and, where the harness reports it, the outcome.
fn tool_from(object: &serde_json::Map<String, Value>) -> Option<ToolRef> {
    let name = string_field(object, "tool_name").or_else(|| string_field(object, "toolName"))?;
    Some(ToolRef {
        name,
        ok: tool_outcome(object),
    })
}

/// Decide whether a tool call succeeded, if the payload says so at all.
///
/// Harnesses disagree here — some send `success`, some `is_error`, some only an
/// `error` field when things went wrong, and some say nothing. `None` means
/// "not reported", which is different from "succeeded".
fn tool_outcome(object: &serde_json::Map<String, Value>) -> Option<bool> {
    let response = object
        .get("tool_response")
        .or_else(|| object.get("toolResponse"))?;

    if let Some(response) = response.as_object() {
        if let Some(success) = response.get("success").and_then(Value::as_bool) {
            return Some(success);
        }
        if let Some(is_error) = response.get("is_error").and_then(Value::as_bool) {
            return Some(!is_error);
        }
        if response.contains_key("error") {
            return Some(false);
        }
    }
    None
}

/// Choose what to record as the body for this kind of event.
fn body_for(kind: EventKind, object: &serde_json::Map<String, Value>) -> String {
    match kind {
        EventKind::UserPrompt => string_field(object, "prompt")
            .or_else(|| string_field(object, "user_prompt"))
            .unwrap_or_default(),

        // The input is what the agent decided to do. The response is kept out
        // of the body on purpose: it is usually the largest part of the payload
        // and the least informative once the outcome flag has been read from it.
        EventKind::ToolUse => object
            .get("tool_input")
            .or_else(|| object.get("toolInput"))
            .map(render_compact)
            .unwrap_or_default(),

        EventKind::PreCompact | EventKind::PostCompact => string_field(object, "trigger")
            .or_else(|| string_field(object, "summary"))
            .unwrap_or_default(),

        EventKind::SessionStart => string_field(object, "source").unwrap_or_default(),
        EventKind::SessionEnd => string_field(object, "reason").unwrap_or_default(),

        EventKind::Notification => string_field(object, "message")
            .unwrap_or_else(|| render_compact(&Value::Object(object.clone()))),
    }
}

/// Render a value as compact JSON, or as a plain string if it is one.
fn render_compact(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Read a string field, ignoring non-strings.
fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn claude() -> AgentKind {
        AgentKind::ClaudeCode
    }

    #[test]
    fn a_prompt_hook_becomes_a_user_prompt_observation() {
        let payload = json!({
            "session_id": "abc-123",
            "hook_event_name": "UserPromptSubmit",
            "cwd": "/repo",
            "prompt": "add the storage layer"
        });

        let parsed = parse(&claude(), &payload).unwrap();
        assert_eq!(parsed.kind, EventKind::UserPrompt);
        assert_eq!(parsed.body.as_str(), "add the storage layer");
        assert_eq!(parsed.cwd, Some(PathBuf::from("/repo")));
        assert!(parsed.tool.is_none());
    }

    #[test]
    fn a_tool_hook_records_the_input_and_the_outcome() {
        let payload = json!({
            "session_id": "abc-123",
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": {"file_path": "src/lib.rs"},
            "tool_response": {"success": true}
        });

        let parsed = parse(&claude(), &payload).unwrap();
        assert_eq!(parsed.kind, EventKind::ToolUse);
        assert_eq!(parsed.tool.as_ref().unwrap().name, "Edit");
        assert_eq!(parsed.tool.as_ref().unwrap().ok, Some(true));
        assert!(parsed.body.as_str().contains("src/lib.rs"));
    }

    #[test]
    fn failure_is_recognised_in_each_shape_harnesses_use() {
        let cases = [
            (json!({"success": false}), Some(false)),
            (json!({"is_error": true}), Some(false)),
            (json!({"error": "boom"}), Some(false)),
            (json!({"stdout": "fine"}), None),
        ];

        for (response, expected) in cases {
            let payload = json!({
                "session_id": "s",
                "hook_event_name": "PostToolUse",
                "tool_name": "Bash",
                "tool_response": response
            });
            let parsed = parse(&claude(), &payload).unwrap();
            assert_eq!(parsed.tool.unwrap().ok, expected);
        }
    }

    #[test]
    fn an_unreported_outcome_is_not_treated_as_success() {
        let payload = json!({
            "session_id": "s",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash"
        });
        assert_eq!(parse(&claude(), &payload).unwrap().tool.unwrap().ok, None);
    }

    #[test]
    fn secrets_are_redacted_before_the_payload_leaves_the_parser() {
        let payload = json!({
            "session_id": "s",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "deploy with AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIK7MDENGbPxRfiCY"
        });

        let parsed = parse(&claude(), &payload).unwrap();
        assert!(!parsed.body.as_str().contains("wJalrXUtnFEMIK7MDENGbPxRfiCY"));
        assert!(parsed.was_redacted());
    }

    #[test]
    fn unknown_events_are_captured_rather_than_rejected() {
        let payload = json!({
            "session_id": "s",
            "hook_event_name": "SomeFutureHook",
            "message": "something happened"
        });

        let parsed = parse(&claude(), &payload).unwrap();
        assert_eq!(parsed.kind, EventKind::Notification);
        assert_eq!(parsed.body.as_str(), "something happened");
    }

    #[test]
    fn unknown_fields_do_not_break_parsing() {
        let payload = json!({
            "session_id": "s",
            "hook_event_name": "SessionStart",
            "source": "startup",
            "a_field_added_next_release": {"nested": [1, 2, 3]}
        });
        assert_eq!(parse(&claude(), &payload).unwrap().kind, EventKind::SessionStart);
    }

    #[test]
    fn a_payload_without_a_session_id_is_refused() {
        let payload = json!({"hook_event_name": "SessionStart"});
        assert!(matches!(
            parse(&claude(), &payload),
            Err(HookError::MissingSessionId)
        ));
    }

    #[test]
    fn notifications_get_the_smaller_budget() {
        let payload = json!({
            "session_id": "s",
            "hook_event_name": "Notification",
            "message": "x".repeat(BoundedBody::DEFAULT_LIMIT)
        });
        let parsed = parse(&claude(), &payload).unwrap();
        assert!(parsed.body.len() <= BoundedBody::NOTIFICATION_LIMIT);
        assert!(parsed.body.is_truncated());
    }

    #[test]
    fn camel_case_payloads_are_accepted_too() {
        let payload = json!({
            "sessionId": "s",
            "hook_event_name": "PostToolUse",
            "toolName": "Read",
            "toolInput": {"path": "a.rs"}
        });
        let parsed = parse(&claude(), &payload).unwrap();
        assert_eq!(parsed.tool.unwrap().name, "Read");
    }
}
