use serde_json::json;

use super::{ActiveRequest, AgentEvent, PiIncomingRecord, decode_event, tool_activity};

#[test]
fn file_activity_shows_only_the_path() {
    let args = json!({
        "path": "src/日本語 file.rs",
        "offset": 10,
        "limit": 20,
        "content": "private file content",
        "edits": [{"oldText": "old text", "newText": "new text"}],
    });
    for name in ["read", "edit", "write"] {
        assert_eq!(
            tool_activity(name, &args),
            format!("{name} src/日本語 file.rs")
        );
    }
}

#[test]
fn bash_activity_is_one_line_and_preserves_quoted_spaces() {
    assert_eq!(
        tool_activity(
            "bash",
            &json!({
                "command": "  printf 'a  b'\r\n\tcargo check\u{2028}echo done\u{2029}  ",
                "timeout": 60,
            })
        ),
        "bash printf 'a  b'  cargo check echo done",
    );
    assert_eq!(
        tool_activity("read", &json!({"path": "a\nb\rc\td\u{0085}e.rs"})),
        "read a b c d e.rs",
    );
}

#[test]
fn unavailable_details_fall_back_to_the_tool_name() {
    for name in ["read", "edit", "write", "bash"] {
        for args in [
            json!(null),
            json!({}),
            json!({"path": null, "command": null}),
            json!({"path": 42, "command": []}),
            json!({"path": " \t\n", "command": " \t\n"}),
        ] {
            assert_eq!(tool_activity(name, &args), name);
        }
    }
    assert_eq!(
        tool_activity(
            "custom_tool",
            &json!({"path": "hidden", "command": "hidden"})
        ),
        "custom_tool",
    );
}

#[test]
fn tool_start_decodes_to_activity_including_nested_calls() {
    let record: PiIncomingRecord = serde_json::from_value(json!({
        "type": "tool_execution_start",
        "toolCallId": "call-2",
        "parentToolCallId": "call-1",
        "toolName": "read",
        "args": {"path": "src/main.rs"},
    }))
    .unwrap();
    assert!(matches!(
        decode_event(&record),
        Some(AgentEvent::ToolActivity(text)) if text == "read src/main.rs",
    ));

    let missing_args: PiIncomingRecord = serde_json::from_value(json!({
        "type": "tool_execution_start",
        "toolName": "read",
    }))
    .unwrap();
    assert!(matches!(
        decode_event(&missing_args),
        Some(AgentEvent::ToolActivity(text)) if text == "read",
    ));
}

#[test]
fn tool_output_and_tool_end_do_not_change_activity_or_add_messages() {
    for kind in ["tool_execution_update", "tool_execution_end"] {
        for is_error in [false, true] {
            let record: PiIncomingRecord = serde_json::from_value(json!({
                "type": kind,
                "toolCallId": "call-1",
                "toolName": "bash",
                "partialResult": {"content": [{"type": "text", "text": "partial output"}]},
                "result": {"content": [{"type": "text", "text": "full output"}]},
                "isError": is_error,
            }))
            .unwrap();
            assert!(decode_event(&record).is_none());
        }
    }
}

#[test]
fn a_response_finishes_only_after_the_agent_settles() {
    for aborted in [false, true] {
        let mut request = ActiveRequest::Prompt {
            id: "prompt-1".into(),
            accepted: true,
            settled: false,
            failure: None,
        };
        let message_end: PiIncomingRecord = serde_json::from_value(json!({
            "type": "message_end",
            "message": {"role": "assistant", "content": [], "stopReason": "toolUse"},
        }))
        .unwrap();
        assert!(request.handle_record(&message_end).is_none());
        let finished = request.handle_record(&PiIncomingRecord::AgentSettled { aborted });
        if aborted {
            assert!(matches!(finished, Some(AgentEvent::Error(_))));
        } else {
            assert!(matches!(finished, Some(AgentEvent::PromptFinished)));
        }
    }
}
