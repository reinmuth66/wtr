use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::Value;

use crate::records::{Class, classify, content_kind, record_kind};

const REJECTION_PREFIX: &str = "The user doesn't want to proceed with this tool use.";
const INTERRUPT_TEXT: &str = "[Request interrupted by user]";
const INTERRUPT_TOOL_TEXT: &str = "[Request interrupted by user for tool use]";

/// Execution status of a tool invocation row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Failed,
    Rejected,
    Interrupted,
    Running,
}

/// Result attached to a tool invocation row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
    pub extra: Value,
}

/// Token and timing summary for a subagent tool call (populated in Task 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentUsage {
    pub tokens: u64,
    pub tool_uses: u64,
    pub duration_ms: u64,
}

/// Outcome kind of a hook execution (populated in Task 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    Success,
    NonBlockingError,
    BlockingError,
    Cancelled,
}

/// Hook execution attached to a tool invocation (populated in Task 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    pub kind: HookKind,
    pub event: String,
    pub name: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i64>,
}

/// Severity of an LSP/tool diagnostic (populated in Task 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

/// Diagnostic attached to a tool invocation (populated in Task 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub line: u64,
    pub message: String,
}

/// A tool invocation row in the session history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRow {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub cwd: Option<PathBuf>,
    pub result: Option<ToolResult>,
    pub status: Status,
    pub agent_id: Option<String>,
    pub usage: Option<AgentUsage>,
    pub report: Option<String>,
    pub hooks: Vec<Hook>,
    pub diagnostics: Vec<Diagnostic>,
}

/// User prompt content, either plain text or a slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    Text(String),
    Command { name: String, args: String },
}

/// A user prompt row in the session history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRow {
    pub prompt: Prompt,
    pub duration_ms: Option<u64>,
}

/// A compaction boundary row in the session history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactRow {
    pub manual: bool,
    pub pre_tokens: Option<u64>,
    pub post_tokens: Option<u64>,
    pub summary: Option<String>,
}

/// A single row in the session history.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum Row {
    Tool(ToolRow),
    Prompt(PromptRow),
    Compact(CompactRow),
}

/// Session history built incrementally from JSONL records.
#[derive(Debug, Default)]
pub struct History {
    pub rows: Vec<Row>,
    id_to_row: HashMap<String, usize>,
    agent_to_row: HashMap<String, usize>,
    pending_results: HashMap<String, (ToolResult, Status)>,
    pending_reports: HashMap<String, String>,
    revisions: Vec<u64>,
    rev_counter: u64,
    last_linked_results: Vec<usize>,
}

impl History {
    /// Creates an empty session history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Processes a single session JSONL record and updates history rows.
    pub fn push(&mut self, rec: &Value) {
        if rec.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let kind = record_kind(rec);
        if classify(&kind) != Class::Use {
            return;
        }

        match kind.as_str() {
            "assistant" => self.push_assistant(rec),
            "user" => self.push_user(rec),
            "system:compact_boundary" => self.push_compact_boundary(rec),
            "system:local_command" => self.push_local_command(rec),
            "system:turn_duration" => self.push_turn_duration(rec),
            "attachment:queued_command" => self.push_queued_command(rec),
            "attachment:hook_success" => self.push_hook(rec, HookKind::Success),
            "attachment:hook_non_blocking_error" => self.push_hook(rec, HookKind::NonBlockingError),
            "attachment:hook_blocking_error" => self.push_hook(rec, HookKind::BlockingError),
            "attachment:hook_cancelled" => self.push_hook(rec, HookKind::Cancelled),
            "attachment:diagnostics" => self.push_diagnostics(rec),
            _ => {}
        }
    }

    /// Returns the revision number when `row` was last modified.
    pub fn revision(&self, row: usize) -> u64 {
        self.revisions.get(row).copied().unwrap_or(0)
    }

    /// Counts `(ok, failed)` tool rows between prompt `row` and the next prompt row.
    pub fn tool_counts(&self, row: usize) -> (usize, usize) {
        let mut ok = 0;
        let mut failed = 0;
        for r in self.rows.iter().skip(row.saturating_add(1)) {
            match r {
                Row::Prompt(_) => break,
                Row::Tool(t) => match t.status {
                    Status::Ok => ok += 1,
                    Status::Failed | Status::Rejected | Status::Interrupted => failed += 1,
                    Status::Running => {}
                },
                Row::Compact(_) => {}
            }
        }
        (ok, failed)
    }

    fn push_row(&mut self, row: Row) -> usize {
        let idx = self.rows.len();
        self.rows.push(row);
        self.revisions.push(0);
        self.bump(idx);
        idx
    }

    fn bump(&mut self, row: usize) {
        self.rev_counter += 1;
        if let Some(rev) = self.revisions.get_mut(row) {
            *rev = self.rev_counter;
        }
    }

    fn push_assistant(&mut self, rec: &Value) {
        let Some(content) = rec
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            return;
        };
        let cwd = rec.get("cwd").and_then(Value::as_str).map(PathBuf::from);

        for item in content {
            if classify(&content_kind("assistant", item)) != Class::Use {
                continue;
            }
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                continue;
            };
            let input = item.get("input").cloned().unwrap_or(Value::Null);
            let mut row = ToolRow {
                id: id.to_string(),
                name: name.to_string(),
                input,
                cwd: cwd.clone(),
                result: None,
                status: Status::Running,
                agent_id: None,
                usage: None,
                report: None,
                hooks: Vec::new(),
                diagnostics: Vec::new(),
            };
            let mut agent_id = None;
            if let Some((result, status)) = self.pending_results.remove(id) {
                agent_id = result
                    .extra
                    .get("agentId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                row.result = Some(result);
                row.status = status;
                if let Some(ref aid) = agent_id {
                    row.agent_id = Some(aid.clone());
                    row.report = self.pending_reports.remove(aid);
                }
            }
            let idx = self.push_row(Row::Tool(row));
            self.id_to_row.insert(id.to_string(), idx);
            if let Some(aid) = agent_id {
                self.agent_to_row.insert(aid, idx);
            }
            self.last_linked_results.clear();
        }
    }

    fn push_user(&mut self, rec: &Value) {
        let Some(content) = rec.get("message").and_then(|m| m.get("content")) else {
            return;
        };

        if rec.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
            let summary = extract_text(Some(content));
            if let Some((idx, Row::Compact(c))) = self
                .rows
                .iter_mut()
                .enumerate()
                .rev()
                .find(|(_, r)| matches!(r, Row::Compact(_)))
            {
                c.summary = Some(summary);
                self.bump(idx);
            }
            return;
        }

        let extra = rec
            .get("toolUseResult")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(Value::Null);

        if let Some(arr) = content.as_array() {
            for item in arr {
                if classify(&content_kind("user", item)) != Class::Use {
                    continue;
                }
                if item.get("type").and_then(Value::as_str) == Some("tool_result") {
                    self.handle_tool_result(item, &extra);
                }
            }
        }

        let is_meta = rec.get("isMeta").and_then(Value::as_bool) == Some(true);
        let mut prompt_parts = Vec::new();
        let mut saw_interrupt_tool = false;

        match content {
            Value::String(s) => {
                self.handle_user_text(s, is_meta, &mut saw_interrupt_tool, &mut prompt_parts);
            }
            Value::Array(arr) => {
                for item in arr {
                    if classify(&content_kind("user", item)) != Class::Use {
                        continue;
                    }
                    match item.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(s) = item.get("text").and_then(Value::as_str) {
                                self.handle_user_text(
                                    s,
                                    is_meta,
                                    &mut saw_interrupt_tool,
                                    &mut prompt_parts,
                                );
                            }
                        }
                        Some("image") if !is_meta => {
                            prompt_parts.push("[image]".to_string());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }

        if saw_interrupt_tool {
            self.apply_interruption();
        }

        if !prompt_parts.is_empty() {
            let joined = prompt_parts.join("\n");
            let prompt = parse_prompt(&joined);
            self.push_row(Row::Prompt(PromptRow {
                prompt,
                duration_ms: None,
            }));
            self.last_linked_results.clear();
        }
    }

    fn handle_tool_result(&mut self, item: &Value, extra: &Value) {
        let Some(tool_use_id) = item.get("tool_use_id").and_then(Value::as_str) else {
            return;
        };
        let is_error = item
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = extract_text(item.get("content"));
        let status = if extra.get("status").and_then(Value::as_str) == Some("async_launched") {
            Status::Running
        } else if !is_error {
            Status::Ok
        } else if text.starts_with(REJECTION_PREFIX) {
            Status::Rejected
        } else {
            Status::Failed
        };
        let result = ToolResult {
            text,
            is_error,
            extra: extra.clone(),
        };

        if let Some(&idx) = self.id_to_row.get(tool_use_id) {
            let agent_id = extra
                .get("agentId")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(Row::Tool(row)) = self.rows.get_mut(idx) {
                row.result = Some(result);
                row.status = status;
                if let Some(ref aid) = agent_id {
                    row.agent_id = Some(aid.clone());
                    if let Some(report) = self.pending_reports.remove(aid) {
                        row.report = Some(report);
                    }
                }
                self.bump(idx);
                self.last_linked_results.push(idx);
            }
            if let Some(aid) = agent_id {
                self.agent_to_row.insert(aid, idx);
            }
        } else {
            self.pending_results
                .insert(tool_use_id.to_string(), (result, status));
        }
    }

    fn apply_interruption(&mut self) {
        let indices = std::mem::take(&mut self.last_linked_results);
        for idx in indices {
            if let Some(Row::Tool(row)) = self.rows.get_mut(idx)
                && row.status == Status::Rejected
            {
                row.status = Status::Interrupted;
                self.bump(idx);
            }
        }
    }

    fn push_compact_boundary(&mut self, rec: &Value) {
        let meta = rec.get("compactMetadata");
        let manual = meta.and_then(|m| m.get("trigger")).and_then(Value::as_str) == Some("manual");
        let pre_tokens = meta
            .and_then(|m| m.get("preTokens"))
            .and_then(Value::as_u64);
        let post_tokens = meta
            .and_then(|m| m.get("postTokens"))
            .and_then(Value::as_u64);
        self.push_row(Row::Compact(CompactRow {
            manual,
            pre_tokens,
            post_tokens,
            summary: None,
        }));
    }

    fn push_local_command(&mut self, rec: &Value) {
        let Some(content) = rec.get("content").and_then(Value::as_str) else {
            return;
        };
        if let Some(cmd) = parse_command(content) {
            self.push_row(Row::Prompt(PromptRow {
                prompt: cmd,
                duration_ms: None,
            }));
            self.last_linked_results.clear();
        }
    }

    fn push_turn_duration(&mut self, rec: &Value) {
        let Some(ms) = rec.get("durationMs").and_then(Value::as_u64) else {
            return;
        };
        if let Some((idx, Row::Prompt(p))) = self
            .rows
            .iter_mut()
            .enumerate()
            .rev()
            .find(|(_, r)| matches!(r, Row::Prompt(_)))
        {
            p.duration_ms = Some(ms);
            self.bump(idx);
        }
    }

    fn push_queued_command(&mut self, rec: &Value) {
        let Some(att) = rec.get("attachment") else {
            return;
        };
        let Some(mode) = att.get("commandMode").and_then(Value::as_str) else {
            return;
        };
        let Some(prompt) = att.get("prompt").and_then(Value::as_str) else {
            return;
        };
        match mode {
            "prompt" => {
                self.maybe_handle_handback(prompt);
                if filter_prompt_text(prompt).is_none() {
                    return;
                }
                self.push_row(Row::Prompt(PromptRow {
                    prompt: Prompt::Text(prompt.to_string()),
                    duration_ms: None,
                }));
                self.last_linked_results.clear();
            }
            "task-notification" => self.push_task_notification(prompt),
            _ => {}
        }
    }

    fn handle_user_text(
        &mut self,
        s: &str,
        is_meta: bool,
        saw_interrupt_tool: &mut bool,
        prompt_parts: &mut Vec<String>,
    ) {
        self.maybe_handle_handback(s);
        if s.trim().starts_with("<task-notification>") {
            self.push_task_notification(s);
        }
        if s.trim() == INTERRUPT_TOOL_TEXT {
            *saw_interrupt_tool = true;
        } else if !is_meta && let Some(part) = filter_prompt_text(s) {
            prompt_parts.push(part);
        }
    }

    fn push_task_notification(&mut self, prompt: &str) {
        let Some(tool_use_id) = extract_tag(prompt, "tool-use-id") else {
            return;
        };
        let Some(&idx) = self.id_to_row.get(&tool_use_id) else {
            return;
        };
        let status = if extract_tag(prompt, "status").as_deref() == Some("completed") {
            Status::Ok
        } else {
            Status::Failed
        };
        let usage = extract_tag(prompt, "usage").map(|u| parse_agent_usage(&u));
        if let Some(Row::Tool(row)) = self.rows.get_mut(idx) {
            row.status = status;
            row.usage = usage;
            self.bump(idx);
        }
    }

    fn maybe_handle_handback(&mut self, s: &str) {
        let Some((from, report)) = parse_handback(s) else {
            return;
        };
        if let Some(&idx) = self.agent_to_row.get(&from) {
            if let Some(Row::Tool(row)) = self.rows.get_mut(idx) {
                row.report = Some(report);
                self.bump(idx);
            }
        } else {
            self.pending_reports.insert(from, report);
        }
    }

    fn push_hook(&mut self, rec: &Value, kind: HookKind) {
        let Some(att) = rec.get("attachment") else {
            return;
        };
        let Some(event) = att.get("hookEvent").and_then(Value::as_str) else {
            return;
        };
        if event != "PostToolUse" {
            return;
        }
        let Some(tool_use_id) = att.get("toolUseID").and_then(Value::as_str) else {
            return;
        };
        let Some(&idx) = self.id_to_row.get(tool_use_id) else {
            return;
        };
        let hook = Hook {
            kind,
            event: event.to_string(),
            name: att
                .get("hookName")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            stdout: att
                .get("stdout")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            stderr: att
                .get("stderr")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            exit_code: att.get("exitCode").and_then(parse_exit_code),
        };
        if let Some(Row::Tool(row)) = self.rows.get_mut(idx) {
            row.hooks.push(hook);
            self.bump(idx);
        }
    }

    fn push_diagnostics(&mut self, rec: &Value) {
        let Some(files) = rec
            .get("attachment")
            .and_then(|a| a.get("files"))
            .and_then(Value::as_array)
        else {
            return;
        };
        for file in files {
            let Some(uri) = file.get("uri").and_then(Value::as_str) else {
                continue;
            };
            let path = uri.strip_prefix("file://").unwrap_or(uri);
            let Some(diags) = file.get("diagnostics").and_then(Value::as_array) else {
                continue;
            };
            if diags.is_empty() {
                continue;
            }
            let Some((idx, Row::Tool(row))) =
                self.rows.iter_mut().enumerate().rev().find(|(_, r)| {
                    matches!(
                        r,
                        Row::Tool(t)
                            if (t.name == "Edit" || t.name == "Write")
                                && t.input.get("file_path").and_then(Value::as_str) == Some(path)
                    )
                })
            else {
                continue;
            };
            for d in diags {
                let severity = match d.get("severity").and_then(Value::as_str) {
                    Some("Error") => Severity::Error,
                    Some("Warning") => Severity::Warning,
                    Some("Hint") => Severity::Hint,
                    _ => Severity::Information,
                };
                let line = d
                    .get("range")
                    .and_then(|r| r.get("start"))
                    .and_then(|s| s.get("line"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    + 1;
                let message = d
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                row.diagnostics.push(Diagnostic {
                    severity,
                    line,
                    message,
                });
            }
            self.bump(idx);
        }
    }
}

fn parse_agent_usage(s: &str) -> AgentUsage {
    let mut usage = AgentUsage {
        tokens: 0,
        tool_uses: 0,
        duration_ms: 0,
    };
    for line in s.lines() {
        let trimmed = line.trim();
        if let Some(val) = trimmed.strip_prefix("subagent_tokens:") {
            usage.tokens = val.trim().parse().unwrap_or(0);
        } else if let Some(val) = trimmed.strip_prefix("tool_uses:") {
            usage.tool_uses = val.trim().parse().unwrap_or(0);
        } else if let Some(val) = trimmed.strip_prefix("duration_ms:") {
            usage.duration_ms = val.trim().parse().unwrap_or(0);
        }
    }
    usage
}

fn parse_handback(s: &str) -> Option<(String, String)> {
    let trimmed = s.trim();
    if !trimmed.starts_with("Another Claude session sent a message:")
        && !trimmed.starts_with("<agent-message from=")
    {
        return None;
    }
    let from_prefix = "<agent-message from=\"";
    let from_start = s.find(from_prefix)? + from_prefix.len();
    let from_end = s[from_start..].find('"')? + from_start;
    let from = &s[from_start..from_end];
    if from.is_empty() {
        return None;
    }
    let marker = "[Subagent hand-back]";
    let marker_pos = s.find(marker)?;
    let line_end = s[marker_pos..].find('\n')? + marker_pos;
    let rest = &s[line_end + 1..];
    let body = match rest.find("</agent-message>") {
        Some(end) => &rest[..end],
        None => rest,
    };
    Some((from.to_string(), body.trim().to_string()))
}

fn parse_exit_code(val: &Value) -> Option<i64> {
    match val {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn extract_text(val: Option<&Value>) -> String {
    match val {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn filter_prompt_text(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("<system-reminder>")
        || trimmed.starts_with("<local-command-")
        || trimmed.starts_with("<bash-")
        || trimmed.starts_with("<task-notification>")
        || trimmed.starts_with("Another Claude session sent a message:")
        || trimmed.starts_with("<agent-message")
        || trimmed == INTERRUPT_TEXT
        || trimmed == INTERRUPT_TOOL_TEXT
    {
        return None;
    }
    Some(s.to_string())
}

fn parse_prompt(s: &str) -> Prompt {
    if let Some(cmd) = parse_command(s) {
        cmd
    } else {
        Prompt::Text(s.to_string())
    }
}

fn parse_command(s: &str) -> Option<Prompt> {
    if !s.contains("<command-name>") {
        return None;
    }
    let name = extract_tag(s, "command-name")?;
    let args = extract_tag(s, "command-args").unwrap_or_default();
    Some(Prompt::Command { name, args })
}

fn extract_tag(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&close)? + start;
    Some(s[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_use(id: &str, name: &str, input: Value) -> Value {
        json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [
                    { "type": "tool_use", "id": id, "name": name, "input": input }
                ]
            }
        })
    }

    fn tool_result(id: &str, text: &str, is_error: bool, extra: Value) -> Value {
        json!({
            "type": "user",
            "toolUseResult": extra,
            "message": {
                "role": "user",
                "content": [
                    { "type": "tool_result", "tool_use_id": id, "content": text, "is_error": is_error }
                ]
            }
        })
    }

    fn user_text(text: &str) -> Value {
        json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": text
            }
        })
    }

    fn system(subtype: &str, extra: Value) -> Value {
        let mut obj = json!({ "type": "system", "subtype": subtype });
        if let (Some(dst), Some(src)) = (obj.as_object_mut(), extra.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        obj
    }

    fn attachment(atype: &str, extra: Value) -> Value {
        let mut att = json!({ "type": atype });
        if let (Some(dst), Some(src)) = (att.as_object_mut(), extra.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        json!({ "type": "attachment", "attachment": att })
    }

    #[test]
    fn tool_then_result() {
        let mut h = History::new();
        h.push(&tool_use("t1", "Bash", json!({"command": "ls"})));
        assert_eq!(h.rows.len(), 1);
        match &h.rows[0] {
            Row::Tool(t) => {
                assert_eq!(t.id, "t1");
                assert_eq!(t.name, "Bash");
                assert_eq!(t.status, Status::Running);
                assert!(t.result.is_none());
            }
            _ => panic!("expected Tool row"),
        }

        h.push(&tool_result("t1", "ok", false, Value::Null));
        assert_eq!(h.rows.len(), 1);
        match &h.rows[0] {
            Row::Tool(t) => {
                assert_eq!(t.status, Status::Ok);
                assert_eq!(t.result.as_ref().unwrap().text, "ok");
                assert!(!t.result.as_ref().unwrap().is_error);
            }
            _ => panic!("expected Tool row"),
        }
    }

    #[test]
    fn result_before_use_is_linked() {
        let mut h = History::new();
        h.push(&tool_result("t1", "ok", false, json!({"exitCode": 0})));
        assert!(h.rows.is_empty());

        h.push(&tool_use("t1", "Bash", json!({"command": "ls"})));
        assert_eq!(h.rows.len(), 1);
        match &h.rows[0] {
            Row::Tool(t) => {
                assert_eq!(t.status, Status::Ok);
                let res = t.result.as_ref().unwrap();
                assert_eq!(res.text, "ok");
                assert_eq!(res.extra, json!({"exitCode": 0}));
            }
            _ => panic!("expected Tool row"),
        }
    }

    #[test]
    fn failure_and_rejection() {
        let mut h = History::new();
        h.push(&tool_use("t1", "Bash", json!({})));
        h.push(&tool_result("t1", "Exit code 1\nboom", true, Value::Null));
        h.push(&tool_use("t2", "Bash", json!({})));
        h.push(&tool_result(
            "t2",
            "The user doesn't want to proceed with this tool use. reason",
            true,
            Value::Null,
        ));

        match &h.rows[0] {
            Row::Tool(t) => assert_eq!(t.status, Status::Failed),
            _ => panic!("expected Tool row"),
        }
        match &h.rows[1] {
            Row::Tool(t) => assert_eq!(t.status, Status::Rejected),
            _ => panic!("expected Tool row"),
        }
    }

    #[test]
    fn interruption_follows_a_rejection() {
        let mut h = History::new();
        h.push(&tool_use("t1", "Bash", json!({})));
        h.push(&tool_result(
            "t1",
            "The user doesn't want to proceed with this tool use.",
            true,
            Value::Null,
        ));
        h.push(&user_text("[Request interrupted by user for tool use]"));

        assert_eq!(h.rows.len(), 1);
        match &h.rows[0] {
            Row::Tool(t) => assert_eq!(t.status, Status::Interrupted),
            _ => panic!("expected Tool row"),
        }
    }

    #[test]
    fn interruption_does_not_reach_back_past_a_new_tool() {
        let mut h = History::new();
        h.push(&tool_use("t1", "Bash", json!({})));
        h.push(&tool_result(
            "t1",
            "The user doesn't want to proceed with this tool use.",
            true,
            Value::Null,
        ));
        h.push(&tool_use("t2", "Read", json!({})));
        h.push(&user_text("[Request interrupted by user for tool use]"));

        match &h.rows[0] {
            Row::Tool(t) => assert_eq!(t.status, Status::Rejected),
            _ => panic!("expected Tool row"),
        }
    }

    #[test]
    fn prompts() {
        let mut h = History::new();
        h.push(&user_text("fix it"));
        h.push(&json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [
                    { "type": "text", "text": "<system-reminder>x</system-reminder>" },
                    { "type": "text", "text": "hi" },
                    { "type": "image", "source": {} }
                ]
            }
        }));
        h.push(&json!({
            "type": "user",
            "isMeta": true,
            "message": { "role": "user", "content": "meta" }
        }));
        h.push(&user_text("<local-command-stdout>x</local-command-stdout>"));
        h.push(&user_text("[Request interrupted by user]"));

        assert_eq!(h.rows.len(), 2);
        match &h.rows[0] {
            Row::Prompt(p) => assert_eq!(p.prompt, Prompt::Text("fix it".to_string())),
            _ => panic!("expected Prompt row"),
        }
        match &h.rows[1] {
            Row::Prompt(p) => assert_eq!(p.prompt, Prompt::Text("hi\n[image]".to_string())),
            _ => panic!("expected Prompt row"),
        }
    }

    #[test]
    fn slash_commands_from_user_and_local_command() {
        let mut h = History::new();
        h.push(&user_text(
            "<command-name>/effort</command-name>\n<command-args>high</command-args>",
        ));
        h.push(&system(
            "local_command",
            json!({ "content": "<command-name>/restart</command-name>" }),
        ));
        h.push(&system(
            "local_command",
            json!({ "content": "<local-command-stdout></local-command-stdout>" }),
        ));

        assert_eq!(h.rows.len(), 2);
        match &h.rows[0] {
            Row::Prompt(p) => assert_eq!(
                p.prompt,
                Prompt::Command {
                    name: "/effort".to_string(),
                    args: "high".to_string()
                }
            ),
            _ => panic!("expected Prompt row"),
        }
        match &h.rows[1] {
            Row::Prompt(p) => assert_eq!(
                p.prompt,
                Prompt::Command {
                    name: "/restart".to_string(),
                    args: String::new()
                }
            ),
            _ => panic!("expected Prompt row"),
        }
    }

    #[test]
    fn queued_prompt_only_in_prompt_mode() {
        let mut h = History::new();
        h.push(&attachment(
            "queued_command",
            json!({ "commandMode": "prompt", "prompt": "queued" }),
        ));
        h.push(&attachment(
            "queued_command",
            json!({ "commandMode": "task-notification", "prompt": "ignored" }),
        ));
        h.push(&attachment(
            "queued_command",
            json!({ "prompt": "ignored" }),
        ));

        assert_eq!(h.rows.len(), 1);
        match &h.rows[0] {
            Row::Prompt(p) => assert_eq!(p.prompt, Prompt::Text("queued".to_string())),
            _ => panic!("expected Prompt row"),
        }
    }

    #[test]
    fn compact_boundary_and_summary() {
        let mut h = History::new();
        h.push(&system(
            "compact_boundary",
            json!({
                "compactMetadata": {
                    "trigger": "manual",
                    "preTokens": 630000,
                    "postTokens": 10000
                }
            }),
        ));
        h.push(&json!({
            "type": "user",
            "isCompactSummary": true,
            "message": { "role": "user", "content": "要約" }
        }));

        assert_eq!(h.rows.len(), 1);
        match &h.rows[0] {
            Row::Compact(c) => {
                assert!(c.manual);
                assert_eq!(c.pre_tokens, Some(630000));
                assert_eq!(c.post_tokens, Some(10000));
                assert_eq!(c.summary.as_deref(), Some("要約"));
            }
            _ => panic!("expected Compact row"),
        }
    }

    #[test]
    fn turn_duration_goes_to_the_last_prompt() {
        let mut h = History::new();
        h.push(&user_text("do it"));
        h.push(&tool_use("t1", "Bash", json!({})));
        h.push(&system("turn_duration", json!({ "durationMs": 33000 })));

        match &h.rows[0] {
            Row::Prompt(p) => assert_eq!(p.duration_ms, Some(33000)),
            _ => panic!("expected Prompt row"),
        }
    }

    #[test]
    fn sidechain_and_unknown_are_skipped() {
        let mut h = History::new();
        let mut side = tool_use("t1", "Bash", json!({}));
        side.as_object_mut()
            .unwrap()
            .insert("isSidechain".to_string(), json!(true));
        h.push(&side);
        h.push(&json!({ "type": "brand-new" }));

        assert!(h.rows.is_empty());
    }

    #[test]
    fn odd_fields_do_not_panic() {
        let mut h = History::new();
        h.push(&json!({ "type": "user" }));
        h.push(&json!({ "type": "user", "message": { "content": 42 } }));
        h.push(&json!({
            "type": "assistant",
            "message": {
                "content": [
                    { "type": "tool_use", "name": "Bash", "input": {} }
                ]
            }
        }));
        assert!(h.rows.is_empty());
    }

    #[test]
    fn tool_counts_and_revision() {
        let mut h = History::new();
        h.push(&user_text("first"));
        h.push(&tool_use("t1", "Bash", json!({})));
        let rev_before = h.revision(1);
        h.push(&tool_result("t1", "ok", false, Value::Null));
        let rev_after = h.revision(1);
        assert!(rev_after > rev_before);

        h.push(&tool_use("t2", "Bash", json!({})));
        h.push(&tool_result("t2", "err", true, Value::Null));
        h.push(&user_text("second"));

        assert_eq!(h.tool_counts(0), (1, 1));
    }

    #[test]
    fn background_agent_runs_until_notified() {
        let mut h = History::new();
        h.push(&tool_use("t1", "Agent", json!({})));
        h.push(&tool_result(
            "t1",
            "launched",
            false,
            json!({ "status": "async_launched", "agentId": "a1" }),
        ));
        match &h.rows[0] {
            Row::Tool(t) => {
                assert_eq!(t.status, Status::Running);
                assert_eq!(t.agent_id.as_deref(), Some("a1"));
            }
            _ => panic!("expected Tool row"),
        }
        let rev_before = h.revision(0);

        h.push(&attachment(
            "queued_command",
            json!({
                "commandMode": "task-notification",
                "prompt": "<task-notification><tool-use-id>t1</tool-use-id><status>completed</status><usage>subagent_tokens: 113000\ntool_uses: 36\nduration_ms: 185000</usage></task-notification>"
            }),
        ));
        assert!(h.revision(0) > rev_before);
        match &h.rows[0] {
            Row::Tool(t) => {
                assert_eq!(t.status, Status::Ok);
                assert_eq!(
                    t.usage,
                    Some(AgentUsage {
                        tokens: 113000,
                        tool_uses: 36,
                        duration_ms: 185000,
                    })
                );
            }
            _ => panic!("expected Tool row"),
        }
    }

    #[test]
    fn last_notification_wins() {
        let mut h = History::new();
        h.push(&tool_use("t1", "Agent", json!({})));
        h.push(&tool_result(
            "t1",
            "launched",
            false,
            json!({ "status": "async_launched", "agentId": "a1" }),
        ));
        h.push(&attachment(
            "queued_command",
            json!({
                "commandMode": "task-notification",
                "prompt": "<task-notification><tool-use-id>t1</tool-use-id><status>completed</status></task-notification>"
            }),
        ));
        h.push(&attachment(
            "queued_command",
            json!({
                "commandMode": "task-notification",
                "prompt": "<task-notification><tool-use-id>t1</tool-use-id><status>killed</status></task-notification>"
            }),
        ));
        match &h.rows[0] {
            Row::Tool(t) => assert_eq!(t.status, Status::Failed),
            _ => panic!("expected Tool row"),
        }
    }

    #[test]
    fn handback_before_and_after_agent_id() {
        let mut h = History::new();
        h.push(&user_text(
            "Another Claude session sent a message:\n<agent-message from=\"a1\">\n[Subagent hand-back]\n報告\n</agent-message>",
        ));
        assert!(h.rows.is_empty());

        h.push(&tool_use("t1", "Agent", json!({})));
        h.push(&tool_result(
            "t1",
            "done",
            false,
            json!({ "agentId": "a1" }),
        ));
        assert_eq!(h.rows.len(), 1);
        match &h.rows[0] {
            Row::Tool(t) => assert_eq!(t.report.as_deref(), Some("報告")),
            _ => panic!("expected Tool row"),
        }

        h.push(&tool_use("t2", "Agent", json!({})));
        h.push(&tool_result(
            "t2",
            "done",
            false,
            json!({ "agentId": "a2" }),
        ));
        let rev_before = h.revision(1);
        h.push(&user_text(
            "Another Claude session sent a message:\n<agent-message from=\"a2\">\n[Subagent hand-back]\n後の報告\n</agent-message>",
        ));
        assert!(h.revision(1) > rev_before);
        match &h.rows[1] {
            Row::Tool(t) => assert_eq!(t.report.as_deref(), Some("後の報告")),
            _ => panic!("expected Tool row"),
        }

        h.push(&tool_use("t3", "Agent", json!({})));
        h.push(&tool_result(
            "t3",
            "done",
            false,
            json!({ "agentId": "a3" }),
        ));
        h.push(&user_text(
            "Another Claude session sent a message:\n<agent-message from=\"a3\">\nnot a hand-back\n</agent-message>",
        ));
        assert_eq!(h.rows.len(), 3);
        match &h.rows[2] {
            Row::Tool(t) => assert_eq!(t.report, None),
            _ => panic!("expected Tool row"),
        }
    }

    #[test]
    fn post_tool_use_hooks_only() {
        let mut h = History::new();
        h.push(&tool_use("t1", "Bash", json!({})));
        let rev_before = h.revision(0);
        h.push(&attachment(
            "hook_blocking_error",
            json!({
                "hookEvent": "PostToolUse",
                "toolUseID": "t1",
                "hookName": "lint",
                "stdout": "out",
                "stderr": "err",
                "exitCode": "2"
            }),
        ));
        assert!(h.revision(0) > rev_before);
        h.push(&attachment(
            "hook_success",
            json!({
                "hookEvent": "SessionStart",
                "toolUseID": "t1",
                "hookName": "init"
            }),
        ));
        match &h.rows[0] {
            Row::Tool(t) => {
                assert_eq!(t.hooks.len(), 1);
                assert_eq!(t.hooks[0].kind, HookKind::BlockingError);
                assert_eq!(t.hooks[0].event, "PostToolUse");
                assert_eq!(t.hooks[0].name, "lint");
                assert_eq!(t.hooks[0].stdout, "out");
                assert_eq!(t.hooks[0].stderr, "err");
                assert_eq!(t.hooks[0].exit_code, Some(2));
            }
            _ => panic!("expected Tool row"),
        }
    }

    #[test]
    fn diagnostics_join_the_last_edit_of_the_path() {
        let mut h = History::new();
        h.push(&tool_use("e1", "Edit", json!({ "file_path": "/p/a.rs" })));
        h.push(&tool_use("e2", "Edit", json!({ "file_path": "/p/b.rs" })));
        h.push(&tool_use("e3", "Edit", json!({ "file_path": "/p/a.rs" })));
        let rev_before = h.revision(2);

        h.push(&attachment(
            "diagnostics",
            json!({
                "files": [
                    {
                        "uri": "file:///p/a.rs",
                        "diagnostics": [
                            { "severity": "Error", "range": { "start": { "line": 0 } }, "message": "err" }
                        ]
                    },
                    {
                        "uri": "file:///p/unknown.rs",
                        "diagnostics": [
                            { "severity": "Warning", "range": { "start": { "line": 5 } }, "message": "warn" }
                        ]
                    }
                ]
            }),
        ));
        assert!(h.revision(2) > rev_before);
        match (&h.rows[0], &h.rows[1], &h.rows[2]) {
            (Row::Tool(t0), Row::Tool(t1), Row::Tool(t2)) => {
                assert!(t0.diagnostics.is_empty());
                assert!(t1.diagnostics.is_empty());
                assert_eq!(
                    t2.diagnostics,
                    vec![Diagnostic {
                        severity: Severity::Error,
                        line: 1,
                        message: "err".to_string(),
                    }]
                );
            }
            _ => panic!("expected Tool rows"),
        }
    }

    #[test]
    fn user_record_task_notification() {
        let mut h = History::new();
        h.push(&tool_use("t1", "Agent", json!({})));
        h.push(&tool_result(
            "t1",
            "launched",
            false,
            json!({ "status": "async_launched", "agentId": "a1" }),
        ));
        let rev_before = h.revision(0);

        h.push(&user_text(
            "<task-notification>\n<task-id>a1</task-id>\n<tool-use-id>t1</tool-use-id>\n<status>completed</status>\n<usage>subagent_tokens: 500\ntool_uses: 3\nduration_ms: 1200</usage>\n</task-notification>",
        ));
        assert_eq!(h.rows.len(), 1);
        assert!(h.revision(0) > rev_before);
        match &h.rows[0] {
            Row::Tool(t) => {
                assert_eq!(t.status, Status::Ok);
                assert_eq!(
                    t.usage,
                    Some(AgentUsage {
                        tokens: 500,
                        tool_uses: 3,
                        duration_ms: 1200,
                    })
                );
            }
            _ => panic!("expected Tool row"),
        }

        h.push(&json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "  <task-notification><tool-use-id>t1</tool-use-id><status>failed</status></task-notification>"
                    }
                ]
            }
        }));
        assert_eq!(h.rows.len(), 1);
        match &h.rows[0] {
            Row::Tool(t) => assert_eq!(t.status, Status::Failed),
            _ => panic!("expected Tool row"),
        }
    }

    #[test]
    fn queued_prompt_handback_and_agent_message() {
        let mut h = History::new();
        h.push(&tool_use("t1", "Agent", json!({})));
        h.push(&tool_result(
            "t1",
            "done",
            false,
            json!({ "agentId": "a1" }),
        ));
        let rev_before = h.revision(0);

        h.push(&attachment(
            "queued_command",
            json!({
                "commandMode": "prompt",
                "prompt": "<agent-message from=\"a1\">\n[Subagent hand-back]\nキュー経由の報告\n</agent-message>"
            }),
        ));
        assert_eq!(h.rows.len(), 1);
        assert!(h.revision(0) > rev_before);
        match &h.rows[0] {
            Row::Tool(t) => assert_eq!(t.report.as_deref(), Some("キュー経由の報告")),
            _ => panic!("expected Tool row"),
        }

        h.push(&attachment(
            "queued_command",
            json!({
                "commandMode": "prompt",
                "prompt": "<agent-message from=\"a2\">\nnot a hand-back\n</agent-message>"
            }),
        ));
        h.push(&user_text(
            "<agent-message from=\"a3\">\nnot a hand-back\n</agent-message>",
        ));
        assert_eq!(h.rows.len(), 1);
    }
}
