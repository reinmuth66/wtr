use serde_json::Value;

/// Classification of a record or content block kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Use,
    Drop,
    Unknown,
}

/// Record and content kinds that wtr reads and displays.
pub const USED: &[&str] = &[
    "assistant",
    "assistant/tool_use",
    "user",
    "user/text",
    "user/image",
    "user/tool_result",
    "system:compact_boundary",
    "system:local_command",
    "system:turn_duration",
    "attachment:queued_command",
    "attachment:hook_success",
    "attachment:hook_non_blocking_error",
    "attachment:hook_blocking_error",
    "attachment:hook_cancelled",
    "attachment:diagnostics",
];

/// Known record and content kinds that wtr intentionally ignores.
pub const DROPPED: &[&str] = &[
    // assistant content
    "assistant/text",
    "assistant/thinking",
    // top-level types
    "api-request",
    "api-request-blob",
    "api-request-shape",
    "last-prompt",
    "atis-latch",
    "mode",
    "ai-title",
    "permission-mode",
    "session-owner",
    "file-history-snapshot",
    "file-history-delta",
    "queue-operation",
    "pr-link",
    "frame-link",
    "agent-name",
    "tools-used",
    "artifact-autoreact-ledger",
    "artifact-comment-monitor",
    "cost-state",
    // system subtypes
    "system:away_summary",
    "system:informational",
    "system:stop_hook_summary",
    // attachment types
    "attachment:agent_listing_delta",
    "attachment:async_hook_response",
    "attachment:at_mention_reference",
    "attachment:auto_mode",
    "attachment:command_permissions",
    "attachment:compact_file_reference",
    "attachment:credential_org",
    "attachment:critical_system_reminder",
    "attachment:date",
    "attachment:deferred_tools_delta",
    "attachment:deferred_tools_record",
    "attachment:directory",
    "attachment:edited_text_file",
    "attachment:environment",
    "attachment:file",
    "attachment:hook_additional_context",
    "attachment:hook_system_message",
    "attachment:instructions",
    "attachment:invoked_skills",
    "attachment:mcp_instructions_delta",
    "attachment:model",
    "attachment:nested_memory",
    "attachment:plan_mode",
    "attachment:plan_mode_exit",
    "attachment:prompt_snapshot",
    "attachment:read_truncation_notice",
    "attachment:remote_session_change",
    "attachment:session_context",
    "attachment:silent_turn_reminder",
    "attachment:skill_listing",
    "attachment:task_reminder",
    "attachment:task_status",
    "attachment:thinking_drop",
    "attachment:total_tokens_reminder",
];

/// Fields (record/tool kind, JSON Pointer) read by wtr; used by `scan-records` to detect schema drift.
pub const FIELDS: &[(&str, &str)] = &[
    ("system:compact_boundary", "/compactMetadata/trigger"),
    ("system:compact_boundary", "/compactMetadata/preTokens"),
    ("system:compact_boundary", "/compactMetadata/postTokens"),
    ("system:turn_duration", "/durationMs"),
    ("attachment:queued_command", "/attachment/prompt"),
    ("attachment:queued_command", "/attachment/commandMode"),
    ("attachment:hook_success", "/attachment/toolUseID"),
    ("attachment:diagnostics", "/attachment/files"),
    ("tool:Edit", "/toolUseResult/structuredPatch"),
    ("tool:Read", "/toolUseResult/file"),
    ("tool:Agent", "/toolUseResult/agentId"),
    ("tool:Write", "/toolUseResult/type"),
];

/// Returns the canonical kind string for a top-level session JSONL record.
pub fn record_kind(rec: &Value) -> String {
    let Some(ty) = rec.get("type").and_then(Value::as_str) else {
        return "?".to_string();
    };
    match ty {
        "system" => {
            let sub = rec.get("subtype").and_then(Value::as_str).unwrap_or("?");
            format!("system:{sub}")
        }
        "attachment" => {
            let sub = rec
                .get("attachment")
                .and_then(|a| a.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            format!("attachment:{sub}")
        }
        other => other.to_string(),
    }
}

/// Returns the canonical kind string for an item inside `message.content`.
pub fn content_kind(role: &str, item: &Value) -> String {
    let ty = item.get("type").and_then(Value::as_str).unwrap_or("?");
    format!("{role}/{ty}")
}

/// Classifies a record or content kind against `USED` and `DROPPED`.
pub fn classify(kind: &str) -> Class {
    if USED.contains(&kind) {
        Class::Use
    } else if DROPPED.contains(&kind) {
        Class::Drop
    } else {
        Class::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kinds() {
        assert_eq!(
            record_kind(&json!({"type":"system","subtype":"compact_boundary"})),
            "system:compact_boundary"
        );
        assert_eq!(record_kind(&json!({"type":"system"})), "system:?");
        assert_eq!(
            record_kind(&json!({"type":"attachment","attachment":{"type":"diagnostics"}})),
            "attachment:diagnostics"
        );
        assert_eq!(record_kind(&json!({"type":"attachment"})), "attachment:?");
        assert_eq!(record_kind(&json!({"type":7})), "?");
        assert_eq!(
            content_kind("assistant", &json!({"type":"tool_use"})),
            "assistant/tool_use"
        );
        assert_eq!(content_kind("user", &json!({})), "user/?");
    }

    #[test]
    fn table() {
        assert!(matches!(classify("attachment:queued_command"), Class::Use));
        assert!(matches!(classify("attachment:diagnostics"), Class::Use));
        assert!(matches!(classify("system:turn_duration"), Class::Use));
        assert!(matches!(classify("cost-state"), Class::Drop));
        assert!(matches!(classify("system:away_summary"), Class::Drop));
        assert!(matches!(classify("assistant/thinking"), Class::Drop));
        assert!(matches!(classify("attachment:brand_new"), Class::Unknown));
    }

    #[test]
    fn no_kind_is_in_both_lists() {
        for kind in USED {
            assert!(
                !DROPPED.contains(kind),
                "kind {kind} appears in both USED and DROPPED"
            );
        }
    }

    #[test]
    fn every_field_kind_is_used() {
        for (kind, _ptr) in FIELDS {
            assert!(
                kind.starts_with("tool:") || USED.contains(kind),
                "FIELDS kind {kind} is neither tool: nor in USED"
            );
        }
    }
}
