//! What a tool returned has to survive the whole way to the prompt.
//!
//! Two crates that never call each other hold the two ends of this: the hook
//! parser writes a tool result into an observation body, and the consolidation
//! renders that body as one transcript line. They agree on one separator, and
//! nothing inside either crate's own tests would notice if one end changed it.
//! This is the seam, tested with the payload shape Claude Code actually sends
//! today — an object of `stdout`, `stderr`, `interrupted`, `isImage` and
//! `noOutputExpected`, with no success flag anywhere in it.

use anamnesis_consolidate::render_prompt;
use anamnesis_core::ids::{ObservationId, ProjectId, SessionId, WorkspaceId};
use anamnesis_core::observation::Observation;
use anamnesis_core::scope::{ProjectKey, WorkspaceName};
use anamnesis_core::session::{AgentKind, Session, SessionState};
use serde_json::json;

fn session() -> Session {
    Session {
        id: SessionId::new(),
        agent: AgentKind::ClaudeCode,
        workspace_id: WorkspaceId::derive(&WorkspaceName::parse("default").expect("workspace")),
        project_id: ProjectId::derive(
            &WorkspaceName::parse("default").expect("workspace"),
            &ProjectKey::from_path(std::path::Path::new("/repo")),
        ),
        workstream_id: None,
        checkout_path: "/repo".into(),
        started_at: "2026-09-09T09:00:00Z".parse().expect("timestamp"),
        ended_at: None,
        state: SessionState::Closed,
        operator: None,
    }
}

fn observed(payload: serde_json::Value) -> Observation {
    let parsed = anamnesis_hooks::parse(&AgentKind::ClaudeCode, &payload).expect("a hook payload");
    let sanitized = parsed.was_redacted();
    Observation {
        id: ObservationId::new(),
        session_id: SessionId::new(),
        kind: parsed.kind,
        tool: parsed.tool,
        at: "2026-09-09T09:30:00Z".parse().expect("timestamp"),
        sanitized,
        body: parsed.body,
    }
}

#[test]
fn a_verdict_a_tool_printed_is_in_the_prompt_the_model_reads() {
    let observations = vec![
        observed(json!({
            "session_id": "probe",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "testleri çalıştır"
        })),
        observed(json!({
            "session_id": "probe",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "cargo test --workspace"},
            "tool_response": {
                "stdout": "running 82 tests\ntest result: FAILED. 81 passed; 1 failed",
                "stderr": "   Compiling anamnesis-core v1.0.0",
                "interrupted": false,
                "isImage": false,
                "noOutputExpected": false
            }
        })),
    ];

    let prompt = render_prompt(
        &session(),
        &observations,
        anamnesis_consolidate::Surroundings::default(),
        4_000,
    );

    assert!(prompt.contains("cargo test --workspace"), "{prompt}");
    assert!(
        prompt.contains("test result: FAILED. 81 passed; 1 failed"),
        "the model was told what the command returned:\n{prompt}"
    );
}

/// And the honesty from the other direction: this harness reports no outcome
/// on any call, so the prompt says the absence of a failure marker proves
/// nothing. Without that the model reads a transcript with no `(FAILED)` on it
/// and reports a session that went well.
#[test]
fn a_harness_that_reports_no_outcome_says_so_in_the_prompt() {
    let observations = vec![observed(json!({
        "session_id": "probe",
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": "cargo build"},
        "tool_response": {"stdout": "", "stderr": "", "interrupted": false}
    }))];

    let prompt = render_prompt(
        &session(),
        &observations,
        anamnesis_consolidate::Surroundings::default(),
        4_000,
    );

    assert!(prompt.contains("Tool outcomes: not reported"), "{prompt}");
}
