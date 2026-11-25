//! Builds the expanded header and body blocks for each session history row.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::diff::{DiffRender, max_line_number, parse_delta, run_delta, unified_diff};
use crate::highlight::{Highlighter, HlCache, plain_spans};
use crate::history::{
    CompactRow, Diagnostic, Hook, HookKind, Prompt, PromptRow, Row, Severity, Status, ToolResult,
    ToolRow,
};
use crate::icons;
use crate::row_text::{Ctx, RowText, fmt_duration, row_text, short_path};
use crate::text_doc::{SyntaxHint, TextDoc, clean_line, pretty_json};
use crate::theme;
use crate::wrap;

/// Maximum byte length of expanded body text before syntax highlighting is skipped.
pub const DETAIL_HL_LIMIT: usize = 256 * 1024;

/// A block of content in an expanded history row body.
pub enum Block {
    Lines(Vec<Line<'static>>),
    Diff(DiffRender),
}

/// Expanded header and body for a single history row.
pub struct Detail {
    pub header: RowText,
    pub body: Vec<Block>,
}

/// Builds the expanded header and body for `row`.
pub fn detail(
    row: &Row,
    counts: (usize, usize),
    ctx: &Ctx,
    hl: &Highlighter,
    path_var: Option<&OsStr>,
) -> Detail {
    match row {
        Row::Tool(t) => Detail {
            header: tool_header(row, t, ctx),
            body: tool_body(t, ctx, hl, path_var),
        },
        Row::Prompt(p) => Detail {
            header: prompt_header(row, p, counts, ctx),
            body: prompt_body(p),
        },
        Row::Compact(c) => Detail {
            header: row_text(row, ctx),
            body: compact_body(c),
        },
    }
}

/// Wraps all blocks in `body` to `width` display columns.
pub fn wrap_body(body: &[Block], width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for block in body {
        match block {
            Block::Lines(lines) => {
                for line in lines {
                    out.extend(wrap::wrap_spans(&line.spans, width));
                }
            }
            Block::Diff(diff) => out.extend(diff.wrap(width)),
        }
    }
    out
}

fn tool_header(row: &Row, t: &ToolRow, ctx: &Ctx) -> RowText {
    let mut h = row_text(row, ctx);
    h.summary = vec![Span::styled(
        clean_line(&t.name),
        Style::default().fg(theme::TEXT),
    )];
    h
}

fn prompt_header(row: &Row, p: &PromptRow, counts: (usize, usize), ctx: &Ctx) -> RowText {
    let mut h = row_text(row, ctx);
    h.summary = match &p.prompt {
        Prompt::Text(_) => vec![Span::raw("user".to_string())],
        Prompt::Command { name, .. } => vec![Span::styled(
            clean_line(name),
            Style::default().fg(theme::BLUE),
        )],
    };
    let (ok, failed) = counts;
    let mut note = vec![Span::styled(
        format!("{} {ok}", icons::OK),
        Style::default().fg(theme::GREEN),
    )];
    if failed > 0 {
        note.push(Span::styled(
            format!(" {} {failed}", icons::FAILED),
            Style::default().fg(theme::RED),
        ));
    }
    if let Some(ms) = p.duration_ms {
        note.push(Span::styled(
            format!(" {} {}", icons::TIMER, fmt_duration(ms)),
            Style::default().fg(theme::OVERLAY1),
        ));
    }
    h.note = note;
    h
}

fn prompt_body(p: &PromptRow) -> Vec<Block> {
    let text = match &p.prompt {
        Prompt::Text(s) => s.as_str(),
        Prompt::Command { name, args } => {
            let full = if args.is_empty() {
                name.clone()
            } else {
                format!("{name} {args}")
            };
            return wrap_text_block(&full, theme::TEXT);
        }
    };
    wrap_text_block(text, theme::TEXT)
}

fn compact_body(c: &CompactRow) -> Vec<Block> {
    match &c.summary {
        Some(s) => wrap_text_block(s, theme::TEXT),
        None => Vec::new(),
    }
}

fn tool_body(t: &ToolRow, ctx: &Ctx, hl: &Highlighter, path_var: Option<&OsStr>) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut lines = Vec::new();

    if matches!(t.status, Status::Rejected | Status::Interrupted) {
        build_aborted_body(t, hl, &mut lines);
    } else {
        match t.name.as_str() {
            "Bash" => build_bash_body(t, &mut lines),
            "Read" => build_read_body(t, hl, &mut lines),
            "Edit" => build_edit_body(t, ctx, hl, path_var, &mut lines, &mut blocks),
            "Write" => build_write_body(t, hl, &mut lines),
            "Grep" | "Glob" => build_search_body(t, ctx, &mut lines),
            "AskUserQuestion" => build_ask_body(t, &mut lines),
            "Agent" => build_agent_body(t, &mut lines),
            _ => build_generic_body(t, hl, &mut lines),
        }
    }

    append_hooks_and_diagnostics(t, &mut lines);

    if !lines.is_empty() {
        blocks.push(Block::Lines(lines));
    }
    blocks
}

fn build_aborted_body(t: &ToolRow, hl: &Highlighter, lines: &mut Vec<Line<'static>>) {
    if t.name == "Bash" {
        push_bash_command(&t.input, lines);
    } else {
        lines.push(subheading("input", theme::OVERLAY1));
        lines.extend(highlight_json(&t.input, hl));
    }
    if let Some(res) = &t.result {
        lines.push(subheading("result", result_heading_color(t.status)));
        push_cleaned_lines(&res.text, theme::TEXT, lines);
    }
}

fn build_bash_body(t: &ToolRow, lines: &mut Vec<Line<'static>>) {
    push_bash_command(&t.input, lines);
    if let Some(desc) = str_field(&t.input, "description") {
        push_cleaned_lines(desc, theme::OVERLAY1, lines);
    }
    if let Some(res) = &t.result {
        let heading = if t.status == Status::Failed {
            match parse_exit_code(&res.text) {
                Some(code) => subheading(&format!("output · exit {code}"), theme::RED),
                None => subheading("output", theme::RED),
            }
        } else {
            subheading("output", theme::OVERLAY1)
        };
        lines.push(heading);
        push_cleaned_lines(&res.text, theme::TEXT, lines);
    }
}

fn push_bash_command(input: &Value, lines: &mut Vec<Line<'static>>) {
    let Some(cmd) = str_field(input, "command") else {
        return;
    };
    let style = Style::default().fg(theme::TEXT);
    for (i, line) in split_raw_lines(cmd).enumerate() {
        let text = if i == 0 {
            format!("{} {}", icons::COMMAND, clean_line(line))
        } else {
            clean_line(line)
        };
        lines.push(Line::from(Span::styled(text, style)));
    }
}

fn parse_exit_code(text: &str) -> Option<i64> {
    let first = text.split('\n').next()?;
    let rest = first.strip_prefix("Exit code ")?;
    rest.trim().parse::<i64>().ok()
}

fn build_read_body(t: &ToolRow, hl: &Highlighter, lines: &mut Vec<Line<'static>>) {
    if let Some(file) = t.result.as_ref().and_then(|r| r.extra.get("file")) {
        let start = file.get("startLine").and_then(Value::as_u64).unwrap_or(1);
        let num = file.get("numLines").and_then(Value::as_u64).unwrap_or(0);
        let end = start.saturating_add(num.saturating_sub(1));
        lines.push(Line::from(Span::styled(
            format!("L{start}-{end}"),
            Style::default().fg(theme::OVERLAY1),
        )));
        let path = file
            .get("filePath")
            .and_then(Value::as_str)
            .or_else(|| str_field(&t.input, "file_path"))
            .unwrap_or("file.txt");
        if let Some(content) = file.get("content").and_then(Value::as_str) {
            lines.extend(highlight_text(
                content,
                SyntaxHint::Path(PathBuf::from(path)),
                hl,
            ));
        }
    } else if let Some(res) = &t.result {
        push_cleaned_lines(&res.text, theme::TEXT, lines);
    }
}

fn build_edit_body(
    t: &ToolRow,
    ctx: &Ctx,
    hl: &Highlighter,
    path_var: Option<&OsStr>,
    lines: &mut Vec<Line<'static>>,
    blocks: &mut Vec<Block>,
) {
    let raw_path = str_field(&t.input, "file_path").unwrap_or("");
    if !raw_path.is_empty() {
        let short = clean_line(&short_path(raw_path, t.cwd.as_deref(), ctx.home.as_deref()));
        lines.push(Line::from(Span::styled(
            short,
            Style::default().fg(theme::TEXT),
        )));
    }

    let Some(res) = &t.result else {
        return;
    };

    if let Some(diff_block) = try_edit_diff(raw_path, res, path_var) {
        if !lines.is_empty() {
            blocks.push(Block::Lines(std::mem::take(lines)));
        }
        blocks.push(diff_block);
    } else {
        lines.push(subheading("input", theme::OVERLAY1));
        lines.extend(highlight_json(&t.input, hl));
        lines.push(subheading("output", result_heading_color(t.status)));
        push_cleaned_lines(&res.text, theme::TEXT, lines);
    }
}

fn try_edit_diff(raw_path: &str, res: &ToolResult, path_var: Option<&OsStr>) -> Option<Block> {
    let patch = res.extra.get("structuredPatch")?;
    let file_name = Path::new(raw_path)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("file");
    let diff = unified_diff(file_name, patch)?;
    let max_ln = max_line_number(patch);
    let digits = if max_ln == 0 {
        1
    } else {
        max_ln.to_string().len()
    };
    let ansi = run_delta(&diff, digits, path_var).ok()?;
    Some(Block::Diff(parse_delta(&ansi, digits)))
}

fn build_write_body(t: &ToolRow, hl: &Highlighter, lines: &mut Vec<Line<'static>>) {
    let is_create = t
        .result
        .as_ref()
        .and_then(|r| r.extra.get("type"))
        .and_then(Value::as_str)
        == Some("create");
    let label = if is_create { "new file" } else { "overwrite" };
    lines.push(Line::from(Span::styled(
        label.to_string(),
        Style::default().fg(theme::OVERLAY1),
    )));
    if let Some(content) = str_field(&t.input, "content") {
        let path = str_field(&t.input, "file_path").unwrap_or("file.txt");
        lines.extend(highlight_text(
            content,
            SyntaxHint::Path(PathBuf::from(path)),
            hl,
        ));
    }
}

fn build_search_body(t: &ToolRow, ctx: &Ctx, lines: &mut Vec<Line<'static>>) {
    let style = Style::default().fg(theme::TEXT);
    if let Some(pat) = str_field(&t.input, "pattern") {
        lines.push(Line::from(Span::styled(
            format!("pattern: {}", clean_line(pat)),
            style,
        )));
    }
    if let Some(path) = str_field(&t.input, "path") {
        let short = clean_line(&short_path(path, t.cwd.as_deref(), ctx.home.as_deref()));
        lines.push(Line::from(Span::styled(format!("path: {short}"), style)));
    }
    if let Some(glob) = str_field(&t.input, "glob") {
        lines.push(Line::from(Span::styled(
            format!("glob: {}", clean_line(glob)),
            style,
        )));
    }
    if let Some(res) = &t.result {
        lines.push(subheading("results", result_heading_color(t.status)));
        push_cleaned_lines(&res.text, theme::TEXT, lines);
    }
}

fn build_ask_body(t: &ToolRow, lines: &mut Vec<Line<'static>>) {
    let Some(questions) = t.input.get("questions").and_then(Value::as_array) else {
        return;
    };
    let answers = t.result.as_ref().and_then(|r| r.extra.get("answers"));
    for q in questions {
        let header = str_field(q, "header")
            .or_else(|| str_field(q, "question"))
            .unwrap_or("");
        if !header.is_empty() {
            lines.push(Line::from(Span::styled(
                clean_line(header),
                Style::default().fg(theme::BLUE),
            )));
        }
        if let Some(q_text) = str_field(q, "question")
            && let Some(ans) = answers.and_then(|a| a.get(q_text)).and_then(Value::as_str)
        {
            push_cleaned_lines(ans, theme::TEXT, lines);
        }
    }
}

fn build_agent_body(t: &ToolRow, lines: &mut Vec<Line<'static>>) {
    let subagent = str_field(&t.input, "subagent_type").map(clean_line);
    let model = str_field(&t.input, "model").map(clean_line);
    let meta = match (subagent, model) {
        (Some(s), Some(m)) => Some(format!("{s} · {m}")),
        (Some(s), None) => Some(s),
        (None, Some(m)) => Some(m),
        (None, None) => None,
    };
    if let Some(meta_line) = meta {
        lines.push(Line::from(Span::styled(
            meta_line,
            Style::default().fg(theme::OVERLAY1),
        )));
    }
    if let Some(prompt) = str_field(&t.input, "prompt") {
        lines.push(subheading("prompt", theme::OVERLAY1));
        push_cleaned_lines(prompt, theme::TEXT, lines);
    }
    if let Some(report) = &t.report {
        lines.push(subheading("result", result_heading_color(t.status)));
        push_cleaned_lines(report, theme::TEXT, lines);
    } else if let Some(res) = &t.result {
        lines.push(subheading("result", result_heading_color(t.status)));
        push_cleaned_lines(&res.text, theme::TEXT, lines);
    }
}

fn build_generic_body(t: &ToolRow, hl: &Highlighter, lines: &mut Vec<Line<'static>>) {
    lines.push(subheading("input", theme::OVERLAY1));
    lines.extend(highlight_json(&t.input, hl));
    if let Some(res) = &t.result {
        lines.push(subheading("output", result_heading_color(t.status)));
        push_cleaned_lines(&res.text, theme::TEXT, lines);
    }
}

fn append_hooks_and_diagnostics(t: &ToolRow, lines: &mut Vec<Line<'static>>) {
    for hook in &t.hooks {
        append_hook(hook, lines);
    }
    if !t.diagnostics.is_empty() {
        lines.push(subheading("diagnostics", theme::OVERLAY1));
        for diag in &t.diagnostics {
            lines.push(format_diagnostic(diag));
        }
    }
}

fn append_hook(hook: &Hook, lines: &mut Vec<Line<'static>>) {
    let heading = match hook.kind {
        HookKind::Cancelled => subheading("hook · cancelled", theme::OVERLAY1),
        HookKind::Success => subheading(
            &format!("hook · {}", clean_line(&hook.name)),
            theme::OVERLAY1,
        ),
        HookKind::NonBlockingError | HookKind::BlockingError => {
            subheading(&format!("hook · {}", clean_line(&hook.name)), theme::RED)
        }
    };
    lines.push(heading);
    if !hook.stdout.is_empty() {
        push_cleaned_lines(&hook.stdout, theme::TEXT, lines);
    }
    if !hook.stderr.is_empty() {
        push_cleaned_lines(&hook.stderr, theme::TEXT, lines);
    }
}

fn format_diagnostic(diag: &Diagnostic) -> Line<'static> {
    let (label, color) = match diag.severity {
        Severity::Error => ("Error", theme::RED),
        Severity::Warning => ("Warning", theme::YELLOW),
        Severity::Information => ("Info", theme::OVERLAY1),
        Severity::Hint => ("Hint", theme::OVERLAY1),
    };
    let text = format!("{label} L{} {}", diag.line, clean_line(&diag.message));
    Line::from(Span::styled(text, Style::default().fg(color)))
}

fn subheading(label: &str, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        format!("─ {label} ─"),
        Style::default().fg(color),
    ))
}

fn result_heading_color(status: Status) -> Color {
    if status == Status::Failed {
        theme::RED
    } else {
        theme::OVERLAY1
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn split_raw_lines(text: &str) -> impl Iterator<Item = &str> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    body.split('\n')
}

fn push_cleaned_lines(text: &str, color: Color, out: &mut Vec<Line<'static>>) {
    if text.is_empty() {
        return;
    }
    let style = Style::default().fg(color);
    for line in split_raw_lines(text) {
        out.push(Line::from(Span::styled(clean_line(line), style)));
    }
}

fn wrap_text_block(text: &str, color: Color) -> Vec<Block> {
    let mut lines = Vec::new();
    push_cleaned_lines(text, color, &mut lines);
    if lines.is_empty() {
        Vec::new()
    } else {
        vec![Block::Lines(lines)]
    }
}

fn highlight_json(val: &Value, hl: &Highlighter) -> Vec<Line<'static>> {
    let raw = serde_json::to_string_pretty(val).unwrap_or_else(|_| val.to_string());
    let pretty = pretty_json(&raw).unwrap_or(raw);
    highlight_text(&pretty, SyntaxHint::Json, hl)
}

fn highlight_text(text: &str, hint: SyntaxHint, hl: &Highlighter) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let plain = text.len() > DETAIL_HL_LIMIT;
    let lines: Vec<String> = split_raw_lines(text).map(clean_line).collect();
    let doc = TextDoc { lines, hint };
    let mut cache = HlCache::new(hl, &doc, plain);
    cache.advance(hl, &doc, Duration::MAX);
    (0..doc.lines.len())
        .map(|i| {
            let spans = cache
                .line(i)
                .map(|s| s.to_vec())
                .unwrap_or_else(|| plain_spans(&doc.lines[i]));
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use ratatui::text::Line;
    use serde_json::json;

    use crate::history::{
        CompactRow, Diagnostic, Hook, HookKind, Prompt, PromptRow, Row, Severity, Status,
        ToolResult, ToolRow,
    };
    use crate::icons::{COMMAND, COMPACT, FAILED, OK, TIMER};
    use crate::row_text::Ctx;

    fn ctx() -> Ctx {
        Ctx {
            home: Some(PathBuf::from("/home/u")),
            icon_width: 2,
        }
    }

    fn base_tool(name: &str, input: Value) -> ToolRow {
        ToolRow {
            id: "id1".to_string(),
            name: name.to_string(),
            input,
            cwd: Some(PathBuf::from("/r")),
            result: None,
            status: Status::Ok,
            agent_id: None,
            usage: None,
            report: None,
            hooks: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn texts(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn body_lines(d: &Detail) -> Vec<Line<'static>> {
        wrap_body(&d.body, 200)
    }

    #[test]
    fn bash_failure_shows_exit_code() {
        let hl = Highlighter::new();
        let mut t = base_tool("Bash", json!({"command": "ls -la"}));
        t.status = Status::Failed;
        t.result = Some(ToolResult {
            text: "Exit code 2\nboom".to_string(),
            is_error: true,
            extra: json!({}),
        });
        let d = detail(&Row::Tool(t), (0, 0), &ctx(), &hl, None);
        let lines = texts(&body_lines(&d));
        assert!(lines.contains(&format!("{COMMAND} ls -la")));
        assert!(lines.contains(&"─ output · exit 2 ─".to_string()));
        assert!(lines.contains(&"boom".to_string()));
    }

    #[test]
    fn read_range_and_highlight() {
        let hl = Highlighter::new();
        let mut t = base_tool("Read", json!({"file_path": "/r/a.rs"}));
        t.result = Some(ToolResult {
            text: String::new(),
            is_error: false,
            extra: json!({
                "file": {
                    "startLine": 10,
                    "numLines": 3,
                    "content": "fn a() {}\nlet x = 1;\nlet y = 2;",
                    "filePath": "/r/a.rs"
                }
            }),
        });
        let d = detail(&Row::Tool(t), (0, 0), &ctx(), &hl, None);
        let lines = body_lines(&d);
        let str_lines = texts(&lines);
        assert!(str_lines.contains(&"L10-12".to_string()));
        assert!(lines.len() >= 2);
        assert!(lines[1].spans.len() >= 2);
    }

    #[test]
    fn edit_falls_back_without_delta() {
        let hl = Highlighter::new();
        let mut t = base_tool(
            "Edit",
            json!({"file_path": "/r/a.rs", "old_string": "a", "new_string": "b"}),
        );
        t.result = Some(ToolResult {
            text: "updated".to_string(),
            is_error: false,
            extra: json!({
                "structuredPatch": [{
                    "oldStart": 1,
                    "oldLines": 1,
                    "newStart": 1,
                    "newLines": 1,
                    "lines": ["-a", "+b"]
                }]
            }),
        });
        let d = detail(&Row::Tool(t), (0, 0), &ctx(), &hl, Some(OsStr::new("")));
        assert!(!d.body.iter().any(|b| matches!(b, Block::Diff(_))));
        let str_lines = texts(&body_lines(&d));
        assert!(str_lines.contains(&"─ input ─".to_string()));
        assert!(str_lines.contains(&"─ output ─".to_string()));
    }

    #[test]
    fn agent_prefers_the_report() {
        let hl = Highlighter::new();
        let mut t = base_tool(
            "Agent",
            json!({"subagent_type": "Explore", "model": "haiku", "prompt": "find"}),
        );
        t.report = Some("final".to_string());
        t.result = Some(ToolResult {
            text: "raw".to_string(),
            is_error: false,
            extra: json!({}),
        });
        let d = detail(&Row::Tool(t), (0, 0), &ctx(), &hl, None);
        let str_lines = texts(&body_lines(&d));
        let pos = str_lines.iter().position(|l| l == "─ result ─").unwrap();
        assert_eq!(str_lines[pos + 1], "final");
    }

    #[test]
    fn hooks_and_diagnostics_follow_the_body() {
        let hl = Highlighter::new();
        let mut t = base_tool("Edit", json!({"file_path": "/r/a.rs"}));
        t.hooks.push(Hook {
            kind: HookKind::BlockingError,
            event: "PostToolUse".to_string(),
            name: "fmt".to_string(),
            stdout: String::new(),
            stderr: "bad".to_string(),
            exit_code: Some(1),
        });
        t.diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            line: 5,
            message: "unused".to_string(),
        });
        let d = detail(&Row::Tool(t), (0, 0), &ctx(), &hl, None);
        let str_lines = texts(&body_lines(&d));
        assert!(str_lines.contains(&"─ hook · fmt ─".to_string()));
        assert!(str_lines.contains(&"Warning L5 unused".to_string()));
    }

    #[test]
    fn prompt_header_counts_tools() {
        let hl = Highlighter::new();
        let p = PromptRow {
            prompt: Prompt::Text("hello".to_string()),
            duration_ms: Some(33_000),
        };
        let d = detail(&Row::Prompt(p), (3, 1), &ctx(), &hl, None);
        let note: String = d.header.note.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(note, format!("{OK} 3 {FAILED} 1 {TIMER} 33s"));
    }

    #[test]
    fn control_chars_are_cleaned() {
        let hl = Highlighter::new();
        let mut t = base_tool("Bash", json!({"command": "echo"}));
        t.result = Some(ToolResult {
            text: "a\x1b[31mb\tc".to_string(),
            is_error: false,
            extra: json!({}),
        });
        let d = detail(&Row::Tool(t), (0, 0), &ctx(), &hl, None);
        let str_lines = texts(&body_lines(&d));
        assert!(str_lines.contains(&"a^[[31mb    c".to_string()));
    }

    #[test]
    fn wrap_body_wraps_to_width() {
        let hl = Highlighter::new();
        let p = PromptRow {
            prompt: Prompt::Text("abcdefghijklmno".to_string()),
            duration_ms: None,
        };
        let d = detail(&Row::Prompt(p), (0, 0), &ctx(), &hl, None);
        let wrapped = wrap_body(&d.body, 10);
        assert!(wrapped.len() >= 2);
        assert!(wrapped.iter().all(|l| l.width() <= 10));
    }

    #[test]
    fn compact_header_and_body() {
        let hl = Highlighter::new();
        let c = CompactRow {
            manual: false,
            pre_tokens: Some(630_000),
            post_tokens: Some(10_000),
            summary: Some("summary line".to_string()),
        };
        let d = detail(&Row::Compact(c), (0, 0), &ctx(), &hl, None);
        let summary: String = d
            .header
            .summary
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        let note: String = d.header.note.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(d.header.kind, COMPACT);
        assert!(d.header.status.is_none());
        assert_eq!(summary, "compacted · auto");
        assert_eq!(note, "630k → 10k");
        assert_eq!(texts(&body_lines(&d)), vec!["summary line"]);
    }

    #[test]
    fn write_grep_ask_and_rejected_tools() {
        let hl = Highlighter::new();

        let mut w = base_tool(
            "Write",
            json!({"file_path": "/r/a.rs", "content": "fn f() {}"}),
        );
        w.result = Some(ToolResult {
            text: "ok".to_string(),
            is_error: false,
            extra: json!({"type": "create"}),
        });
        let wd = texts(&body_lines(&detail(
            &Row::Tool(w),
            (0, 0),
            &ctx(),
            &hl,
            None,
        )));
        assert_eq!(wd[0], "new file");
        assert_eq!(wd[1], "fn f() {}");

        let mut g = base_tool(
            "Grep",
            json!({"pattern": "foo", "path": "/r/src", "glob": "*.rs"}),
        );
        g.result = Some(ToolResult {
            text: "match".to_string(),
            is_error: false,
            extra: json!({}),
        });
        let gd = texts(&body_lines(&detail(
            &Row::Tool(g),
            (0, 0),
            &ctx(),
            &hl,
            None,
        )));
        assert!(gd.contains(&"pattern: foo".to_string()));
        assert!(gd.contains(&"path: src".to_string()));
        assert!(gd.contains(&"glob: *.rs".to_string()));
        assert!(gd.contains(&"─ results ─".to_string()));
        assert!(gd.contains(&"match".to_string()));

        let mut ask = base_tool(
            "AskUserQuestion",
            json!({"questions": [{"header": "Choice", "question": "Which?"}]}),
        );
        ask.result = Some(ToolResult {
            text: String::new(),
            is_error: false,
            extra: json!({"answers": {"Which?": "Option A"}}),
        });
        let askd = texts(&body_lines(&detail(
            &Row::Tool(ask),
            (0, 0),
            &ctx(),
            &hl,
            None,
        )));
        assert_eq!(askd, vec!["Choice", "Option A"]);

        let mut rej = base_tool("Bash", json!({"command": "rm -rf /"}));
        rej.status = Status::Rejected;
        rej.result = Some(ToolResult {
            text: "denied".to_string(),
            is_error: true,
            extra: json!({}),
        });
        let rejd = texts(&body_lines(&detail(
            &Row::Tool(rej),
            (0, 0),
            &ctx(),
            &hl,
            None,
        )));
        assert_eq!(
            rejd,
            vec![
                format!("{COMMAND} rm -rf /"),
                "─ result ─".to_string(),
                "denied".to_string()
            ]
        );
    }

    #[test]
    fn pending_tool_omits_result_section_and_large_body_is_plain() {
        let hl = Highlighter::new();
        let pending = base_tool("mcp__srv__tool", json!({"k": "v"}));
        let pd = texts(&body_lines(&detail(
            &Row::Tool(pending),
            (0, 0),
            &ctx(),
            &hl,
            None,
        )));
        assert!(pd.contains(&"─ input ─".to_string()));
        assert!(!pd.contains(&"─ output ─".to_string()));

        let big = "fn x() {}\n".repeat((DETAIL_HL_LIMIT / 10) + 100);
        let mut r = base_tool("Read", json!({"file_path": "/r/a.rs"}));
        r.result = Some(ToolResult {
            text: String::new(),
            is_error: false,
            extra: json!({
                "file": {
                    "startLine": 1,
                    "numLines": 1,
                    "content": big,
                    "filePath": "/r/a.rs"
                }
            }),
        });
        let rd = body_lines(&detail(&Row::Tool(r), (0, 0), &ctx(), &hl, None));
        assert_eq!(rd[1].spans.len(), 1);
    }
}
