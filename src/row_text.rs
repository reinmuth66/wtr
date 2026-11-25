//! Formatting and single-line rendering for session history rows.

use std::path::{Path, PathBuf};

use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::history::{CompactRow, Prompt, PromptRow, Row, Severity, Status, ToolRow};
use crate::icons;
use crate::text_doc::clean_line;
use crate::theme;

/// Context needed when building display text for a history row.
pub struct Ctx {
    pub home: Option<PathBuf>,
    pub icon_width: u16,
}

/// Formatted parts of one history list row.
pub struct RowText {
    pub status: Option<(&'static str, Color)>,
    pub kind: &'static str,
    pub summary: Vec<Span<'static>>,
    pub note: Vec<Span<'static>>,
}

/// Returns the Nerd Font kind icon for a tool name.
pub fn kind_mark(tool_name: &str) -> &'static str {
    if tool_name.starts_with("mcp__") {
        return icons::MCP;
    }
    match tool_name {
        "Bash" => icons::BASH,
        "Read" => icons::READ,
        "Edit" => icons::EDIT,
        "Write" => icons::WRITE,
        "Grep" => icons::GREP,
        "Glob" => icons::GLOB,
        "WebSearch" | "WebFetch" => icons::WEB,
        "AskUserQuestion" => icons::ASK,
        "Agent" => icons::AGENT,
        "Skill" => icons::SKILL,
        "ToolSearch" => icons::TOOL_SEARCH,
        "SendMessage" => icons::SEND_MESSAGE,
        "TaskStop" => icons::TASK_STOP,
        "Artifact" => icons::ARTIFACT,
        _ => icons::TOOL,
    }
}

/// Returns the Nerd Font status icon and color for a tool execution status.
pub fn status_mark(s: Status) -> (&'static str, Color) {
    match s {
        Status::Ok => (icons::OK, theme::GREEN),
        Status::Failed => (icons::FAILED, theme::RED),
        Status::Rejected => (icons::REJECTED, theme::PEACH),
        Status::Interrupted => (icons::INTERRUPTED, theme::YELLOW),
        Status::Running => (icons::RUNNING, theme::SKY),
    }
}

/// Shortens `p` relative to `cwd` if under it, otherwise `~/...` if under `home`.
pub fn short_path(p: &str, cwd: Option<&Path>, home: Option<&Path>) -> String {
    let path = Path::new(p);
    if let Some(c) = cwd
        && let Ok(rel) = path.strip_prefix(c)
    {
        return if rel.as_os_str().is_empty() {
            ".".to_string()
        } else {
            rel.display().to_string()
        };
    }
    if let Some(h) = home
        && let Ok(rel) = path.strip_prefix(h)
    {
        return if rel.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", rel.display())
        };
    }
    p.to_string()
}

/// Formats milliseconds as `33s`, `3m 5s`, or `1h 2m` (truncating sub-units).
pub fn fmt_duration(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Formats token counts as `999`, `113k`, or `1.2M` (truncating).
pub fn fmt_tokens(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{}k", n / 1_000)
    } else {
        let tenths = n / 100_000;
        format!("{}.{}M", tenths / 10, tenths % 10)
    }
}

/// Splits an `mcp__<server>__<tool>` name on the last `__` into `(server, tool)`.
pub fn mcp_name(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix("mcp__")?.rsplit_once("__")
}

/// Builds the summary and note spans for a history row.
pub fn row_text(row: &Row, ctx: &Ctx) -> RowText {
    match row {
        Row::Tool(t) => tool_row_text(t, ctx),
        Row::Prompt(p) => prompt_row_text(p),
        Row::Compact(c) => compact_row_text(c),
    }
}

fn first_line(s: &str) -> String {
    clean_line(s.split('\n').next().unwrap_or(""))
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn span_text(s: String) -> Vec<Span<'static>> {
    if s.is_empty() {
        Vec::new()
    } else {
        vec![Span::raw(s)]
    }
}

fn muted_span(s: String) -> Vec<Span<'static>> {
    if s.is_empty() {
        Vec::new()
    } else {
        vec![Span::styled(s, Style::default().fg(theme::OVERLAY1))]
    }
}

fn prompt_row_text(p: &PromptRow) -> RowText {
    let summary = match &p.prompt {
        Prompt::Text(text) => span_text(first_line(text)),
        Prompt::Command { name, args } => {
            let text = if args.is_empty() {
                clean_line(name)
            } else {
                clean_line(&format!("{name} {args}"))
            };
            vec![Span::styled(text, Style::default().fg(theme::BLUE))]
        }
    };
    let note = match p.duration_ms {
        Some(ms) => muted_span(format!("{} {}", icons::TIMER, fmt_duration(ms))),
        None => Vec::new(),
    };
    RowText {
        status: None,
        kind: icons::PROMPT,
        summary,
        note,
    }
}

fn compact_row_text(c: &CompactRow) -> RowText {
    let kind_str = if c.manual { "manual" } else { "auto" };
    let note = match (c.pre_tokens, c.post_tokens) {
        (Some(pre), Some(post)) => {
            muted_span(format!("{} → {}", fmt_tokens(pre), fmt_tokens(post)))
        }
        _ => Vec::new(),
    };
    RowText {
        status: None,
        kind: icons::COMPACT,
        summary: vec![Span::styled(
            format!("compacted · {kind_str}"),
            Style::default().fg(theme::OVERLAY1),
        )],
        note,
    }
}

fn tool_row_text(t: &ToolRow, ctx: &Ctx) -> RowText {
    RowText {
        status: Some(status_mark(t.status)),
        kind: kind_mark(&t.name),
        summary: tool_summary(t, ctx),
        note: tool_note(t),
    }
}

fn tool_summary(t: &ToolRow, ctx: &Ctx) -> Vec<Span<'static>> {
    let short = |p: &str| clean_line(&short_path(p, t.cwd.as_deref(), ctx.home.as_deref()));

    if let Some((server, tool)) = mcp_name(&t.name) {
        let first_str = t
            .input
            .as_object()
            .and_then(|m| m.values().find_map(Value::as_str))
            .map(first_line)
            .filter(|s| !s.is_empty());
        let text = match first_str {
            Some(arg) => format!("{} {} {arg}", clean_line(server), clean_line(tool)),
            None => format!("{} {}", clean_line(server), clean_line(tool)),
        };
        return span_text(text);
    }

    let text = match t.name.as_str() {
        "Bash" => str_field(&t.input, "command")
            .map(first_line)
            .unwrap_or_default(),
        "Read" | "Edit" | "Write" => str_field(&t.input, "file_path")
            .map(short)
            .unwrap_or_default(),
        "Grep" => {
            let pat = str_field(&t.input, "pattern")
                .map(first_line)
                .unwrap_or_default();
            match str_field(&t.input, "path") {
                Some(p) if !pat.is_empty() => format!("{pat} {}", short(p)),
                Some(p) => short(p),
                None => pat,
            }
        }
        "Glob" => str_field(&t.input, "pattern")
            .map(first_line)
            .unwrap_or_default(),
        "ToolSearch" | "WebSearch" => str_field(&t.input, "query")
            .map(first_line)
            .unwrap_or_default(),
        "WebFetch" => str_field(&t.input, "url")
            .map(first_line)
            .unwrap_or_default(),
        "AskUserQuestion" => t
            .input
            .get("questions")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|q| q.get("header"))
            .and_then(Value::as_str)
            .map(first_line)
            .unwrap_or_default(),
        "Agent" => str_field(&t.input, "description")
            .map(first_line)
            .unwrap_or_default(),
        "Skill" => str_field(&t.input, "skill")
            .map(first_line)
            .unwrap_or_default(),
        "SendMessage" => {
            let to = str_field(&t.input, "to")
                .map(clean_line)
                .unwrap_or_default();
            let msg = str_field(&t.input, "message")
                .map(first_line)
                .unwrap_or_default();
            match (to.is_empty(), msg.is_empty()) {
                (false, false) => format!("{to} {msg}"),
                (false, true) => to,
                (true, false) => msg,
                (true, true) => String::new(),
            }
        }
        "TaskStop" => str_field(&t.input, "task_id")
            .map(first_line)
            .unwrap_or_default(),
        _ => clean_line(&t.name),
    };
    span_text(text)
}

fn tool_note(t: &ToolRow) -> Vec<Span<'static>> {
    match t.status {
        Status::Failed => {
            let msg = t
                .result
                .as_ref()
                .map(|r| first_line(&r.text))
                .unwrap_or_default();
            if msg.is_empty() {
                Vec::new()
            } else {
                vec![Span::styled(msg, Style::default().fg(theme::RED))]
            }
        }
        Status::Rejected => vec![Span::styled(
            "(rejected)",
            Style::default().fg(theme::PEACH),
        )],
        Status::Interrupted => vec![Span::styled(
            "(interrupted)",
            Style::default().fg(theme::YELLOW),
        )],
        Status::Running => vec![Span::styled("(running)", Style::default().fg(theme::SKY))],
        Status::Ok => ok_tool_note(t),
    }
}

fn diag_prefix(t: &ToolRow) -> Option<Span<'static>> {
    let errors = t
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    if errors > 0 {
        return Some(Span::styled(
            format!("{} {errors}", icons::DIAG_ERROR),
            Style::default().fg(theme::RED),
        ));
    }
    let warnings = t
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();
    if warnings > 0 {
        return Some(Span::styled(
            format!("{} {warnings}", icons::DIAG_WARNING),
            Style::default().fg(theme::YELLOW),
        ));
    }
    None
}

fn ok_tool_note(t: &ToolRow) -> Vec<Span<'static>> {
    let extra = t.result.as_ref().map(|r| &r.extra);
    match t.name.as_str() {
        "Bash" => {
            let Some(res) = &t.result else {
                return Vec::new();
            };
            muted_span(format!("{} {}", icons::LINES, res.text.lines().count()))
        }
        "Read" => {
            let Some(n) = extra
                .and_then(|e| e.pointer("/file/numLines"))
                .and_then(Value::as_u64)
            else {
                return Vec::new();
            };
            muted_span(format!("{} {n}", icons::LINES))
        }
        "Edit" => {
            let mut spans = Vec::new();
            if let Some(diag) = diag_prefix(t) {
                spans.push(diag);
            }
            if let Some(patch) = extra
                .and_then(|e| e.get("structuredPatch"))
                .and_then(Value::as_array)
            {
                let (mut adds, mut dels) = (0usize, 0usize);
                for hunk in patch {
                    if let Some(lines) = hunk.get("lines").and_then(Value::as_array) {
                        for line in lines.iter().filter_map(Value::as_str) {
                            if line.starts_with('+') {
                                adds += 1;
                            } else if line.starts_with('-') {
                                dels += 1;
                            }
                        }
                    }
                }
                if !spans.is_empty() {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    format!("+{adds} -{dels}"),
                    Style::default().fg(theme::OVERLAY1),
                ));
            }
            spans
        }
        "Write" => {
            let mut spans = Vec::new();
            if let Some(diag) = diag_prefix(t) {
                spans.push(diag);
                spans.push(Span::raw(" "));
            }
            let lines = str_field(&t.input, "content")
                .map(|s| s.lines().count())
                .unwrap_or(0);
            let is_create = extra.and_then(|e| str_field(e, "type")) == Some("create");
            let icon = if is_create { icons::NEW } else { icons::LINES };
            spans.push(Span::styled(
                format!("{icon} {lines}"),
                Style::default().fg(theme::OVERLAY1),
            ));
            spans
        }
        "Grep" => {
            let mode = extra.and_then(|e| str_field(e, "mode"));
            let count = if mode == Some("content") {
                extra
                    .and_then(|e| e.get("numLines"))
                    .and_then(Value::as_u64)
            } else {
                extra
                    .and_then(|e| e.get("numFiles"))
                    .and_then(Value::as_u64)
            }
            .or_else(|| {
                t.result
                    .as_ref()
                    .map(|r| r.text.lines().filter(|l| !l.trim().is_empty()).count() as u64)
            });
            match count {
                Some(n) => muted_span(format!("{} {n}", icons::COUNT)),
                None => Vec::new(),
            }
        }
        "Glob" => {
            let count = extra
                .and_then(|e| e.get("numFiles"))
                .and_then(Value::as_u64)
                .or_else(|| {
                    t.result
                        .as_ref()
                        .map(|r| r.text.lines().filter(|l| !l.trim().is_empty()).count() as u64)
                });
            match count {
                Some(n) => muted_span(format!("{} {n}", icons::COUNT)),
                None => Vec::new(),
            }
        }
        "ToolSearch" => {
            let count = extra
                .and_then(|e| e.get("matches"))
                .and_then(|m| m.as_array().map(|a| a.len() as u64).or_else(|| m.as_u64()));
            match count {
                Some(n) => muted_span(format!("{} {n}", icons::COUNT)),
                None => Vec::new(),
            }
        }
        "WebSearch" => {
            let count = extra
                .and_then(|e| e.get("searchCount"))
                .and_then(Value::as_u64);
            let dur_ms = extra
                .and_then(|e| e.get("durationSeconds"))
                .and_then(Value::as_f64)
                .map(|s| (s * 1000.0) as u64);
            match (count, dur_ms) {
                (Some(c), Some(ms)) => muted_span(format!(
                    "{} {c} · {} {}",
                    icons::COUNT,
                    icons::TIMER,
                    fmt_duration(ms)
                )),
                (Some(c), None) => muted_span(format!("{} {c}", icons::COUNT)),
                (None, Some(ms)) => muted_span(format!("{} {}", icons::TIMER, fmt_duration(ms))),
                (None, None) => Vec::new(),
            }
        }
        "WebFetch" => match extra.and_then(|e| e.get("code")) {
            Some(v) => {
                if let Some(n) = v.as_u64() {
                    muted_span(n.to_string())
                } else if let Some(s) = v.as_str() {
                    muted_span(clean_line(s))
                } else {
                    Vec::new()
                }
            }
            None => Vec::new(),
        },
        "AskUserQuestion" => {
            let q = t
                .input
                .get("questions")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|item| str_field(item, "question"));
            let ans = q
                .and_then(|q| extra?.get("answers")?.get(q)?.as_str())
                .map(first_line)
                .unwrap_or_default();
            muted_span(ans)
        }
        "Agent" => {
            let tool_uses = extra
                .and_then(|e| e.get("totalToolUseCount"))
                .and_then(Value::as_u64)
                .or_else(|| t.usage.map(|u| u.tool_uses));
            let tokens = extra
                .and_then(|e| e.get("totalTokens"))
                .and_then(Value::as_u64)
                .or_else(|| t.usage.map(|u| u.tokens));
            let dur = extra
                .and_then(|e| e.get("totalDurationMs"))
                .and_then(Value::as_u64)
                .or_else(|| t.usage.map(|u| u.duration_ms));
            match (tool_uses, tokens, dur) {
                (Some(u), Some(tok), Some(ms)) => muted_span(format!(
                    "{} {u} · {} · {}",
                    icons::TOOL,
                    fmt_tokens(tok),
                    fmt_duration(ms)
                )),
                _ => Vec::new(),
            }
        }
        "Artifact" => match extra.and_then(|e| str_field(e, "url")) {
            Some(url) => muted_span(clean_line(url)),
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

fn truncate_spans(spans: &[Span<'static>], max_w: usize) -> Vec<Span<'static>> {
    if spans_width(spans) <= max_w {
        return spans.to_vec();
    }
    if max_w == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut used = 0usize;
    let mut last_style = Style::default();
    'outer: for span in spans {
        last_style = span.style;
        let mut buf = String::new();
        for c in span.content.chars() {
            let cw = c.width().unwrap_or(0);
            if used + cw > max_w - 1 {
                if !buf.is_empty() {
                    out.push(Span::styled(buf, span.style));
                }
                break 'outer;
            }
            buf.push(c);
            used += cw;
        }
        if !buf.is_empty() {
            out.push(Span::styled(buf, span.style));
        }
    }
    if let Some(last) = out.last_mut() {
        let mut s = last.content.to_string();
        s.push('…');
        last.content = s.into();
    } else {
        out.push(Span::styled("…", last_style));
    }
    out
}

fn draw_spans(buf: &mut Buffer, x: u16, y: u16, max_w: u16, spans: &[Span<'_>]) {
    let (mut cur_x, mut left) = (x, max_w);
    for span in spans {
        if left == 0 {
            break;
        }
        let (nx, _) = buf.set_stringn(cur_x, y, span.content.as_ref(), left as usize, span.style);
        left = left.saturating_sub(nx.saturating_sub(cur_x));
        cur_x = nx;
    }
}

fn draw_icon(buf: &mut Buffer, x: u16, y: u16, mark: &str, style: Style, icon_width: u16) {
    buf[(x, y)].set_symbol(mark).set_style(style);
    if icon_width >= 2 {
        buf[(x + 1, y)].set_diff_option(CellDiffOption::Skip);
    }
}

/// Renders one history row into `buf` at `(x, y)` within `width` columns.
pub fn render_row(buf: &mut Buffer, x: u16, y: u16, width: u16, t: &RowText, icon_width: u16) {
    let area = buf.area();
    if y >= area.bottom() || x >= area.right() {
        return;
    }
    let width = width.min(area.right() - x);
    let iw = icon_width.max(1);
    if width < 1 + iw {
        return;
    }

    if let Some((mark, color)) = t.status {
        draw_icon(buf, x + 1, y, mark, Style::default().fg(color), iw);
    }

    if width < 2 + 2 * iw {
        return;
    }
    draw_icon(
        buf,
        x + 2 + iw,
        y,
        t.kind,
        Style::default().fg(theme::SUBTEXT0),
        iw,
    );

    let prefix_w = 3 + 2 * iw;
    if width <= prefix_w {
        return;
    }

    let sum_x = x + prefix_w;
    let rem = (width - prefix_w) as usize;
    let max_note_w = (width as usize * 4) / 10;

    let note = if !t.note.is_empty() && max_note_w > 0 {
        truncate_spans(&t.note, max_note_w)
    } else {
        Vec::new()
    };
    let note_w = spans_width(&note);

    let (draw_note, max_sum_w) = if note_w > 0 && rem >= note_w + 2 {
        (true, rem - note_w - 1)
    } else {
        (false, rem)
    };

    let summary = truncate_spans(&t.summary, max_sum_w);
    draw_spans(buf, sum_x, y, max_sum_w as u16, &summary);

    if draw_note {
        let note_x = x + width - note_w as u16;
        draw_spans(buf, note_x, y, note_w as u16, &note);
    }
}

/// Renders a compaction boundary row into `buf` at `(x, y)` within `width` columns.
pub fn render_compact(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    c: &CompactRow,
    icon_width: u16,
) {
    let area = buf.area();
    if y >= area.bottom() || x >= area.right() {
        return;
    }
    let width = width.min(area.right() - x);
    if width == 0 {
        return;
    }
    let iw = icon_width.max(1);
    let style = Style::default().fg(theme::OVERLAY1);

    let right = match (c.pre_tokens, c.post_tokens) {
        (Some(pre), Some(post)) => format!(" {} → {}", fmt_tokens(pre), fmt_tokens(post)),
        _ => String::new(),
    };
    let right_w = right.width();

    let kind_str = if c.manual { "manual" } else { "auto" };
    let label = format!(" compacted · {kind_str} ");

    let mut cur_x = x;
    let end_x = x + width;

    // Leading " ── "
    let (nx, _) = buf.set_stringn(cur_x, y, " ── ", (end_x - cur_x) as usize, style);
    cur_x = nx;
    if cur_x + iw <= end_x {
        draw_icon(buf, cur_x, y, icons::COMPACT, style, iw);
        cur_x += iw;
    }
    if cur_x < end_x {
        let (nx, _) = buf.set_stringn(cur_x, y, &label, (end_x - cur_x) as usize, style);
        cur_x = nx;
    }

    let (fill_end, draw_right) = if right_w > 0 && (end_x - x) as usize > right_w + 6 {
        (end_x - right_w as u16, true)
    } else {
        (end_x, false)
    };

    if cur_x < fill_end {
        let dashes = "─".repeat((fill_end - cur_x) as usize);
        buf.set_stringn(cur_x, y, &dashes, (fill_end - cur_x) as usize, style);
    }

    if draw_right {
        buf.set_stringn(fill_end, y, &right, right_w, style);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use ratatui::buffer::{Buffer, CellDiffOption};
    use ratatui::layout::Rect;
    use serde_json::json;

    use super::*;
    use crate::history::{
        AgentUsage, CompactRow, Diagnostic, Prompt, PromptRow, Row, Severity, Status, ToolResult,
        ToolRow,
    };
    use crate::icons;

    fn buf_line(buf: &Buffer, y: u16, width: u16) -> String {
        let mut s = String::new();
        let mut x = 0;
        while x < width {
            let cell = &buf[(x, y)];
            if cell.diff_option == CellDiffOption::Skip {
                x += 1;
                continue;
            }
            let sym = cell.symbol();
            s.push_str(sym);
            x += sym.width().max(1) as u16;
        }
        s
    }

    fn spans_text(spans: &[ratatui::text::Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn tool_row(name: &str, input: serde_json::Value, status: Status) -> ToolRow {
        ToolRow {
            id: "1".into(),
            name: name.into(),
            input,
            cwd: None,
            result: None,
            status,
            agent_id: None,
            usage: None,
            report: None,
            hooks: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn paths() {
        assert_eq!(
            short_path("/r/src/a.rs", Some(Path::new("/r")), Some(Path::new("/h"))),
            "src/a.rs"
        );
        assert_eq!(
            short_path("/h/x/b.md", Some(Path::new("/r")), Some(Path::new("/h"))),
            "~/x/b.md"
        );
        assert_eq!(
            short_path("/etc/hosts", Some(Path::new("/r")), Some(Path::new("/h"))),
            "/etc/hosts"
        );
    }

    #[test]
    fn durations_and_tokens() {
        assert_eq!(fmt_duration(33_000), "33s");
        assert_eq!(fmt_duration(185_000), "3m 5s");
        assert_eq!(fmt_duration(3_720_000), "1h 2m");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(113_400), "113k");
        assert_eq!(fmt_tokens(1_250_000), "1.2M");
    }

    #[test]
    fn mcp_names() {
        assert_eq!(
            mcp_name("mcp__plugin_hm_playwright__browser_click"),
            Some(("plugin_hm_playwright", "browser_click"))
        );
    }

    #[test]
    fn notes() {
        let ctx = Ctx {
            home: None,
            icon_width: 1,
        };

        let mut bash = tool_row("Bash", json!({"command": "ls"}), Status::Ok);
        bash.result = Some(ToolResult {
            text: "a\nb\n".into(),
            is_error: false,
            extra: json!({}),
        });
        assert_eq!(
            spans_text(&row_text(&Row::Tool(bash), &ctx).note),
            format!("{} 2", icons::LINES)
        );

        let mut edit = tool_row("Edit", json!({"file_path": "/a.rs"}), Status::Ok);
        edit.result = Some(ToolResult {
            text: String::new(),
            is_error: false,
            extra: json!({
                "structuredPatch": [{"lines": [" ctx", "+one", "+two", "-old"]}]
            }),
        });
        edit.diagnostics = vec![
            Diagnostic {
                severity: Severity::Error,
                line: 1,
                message: "e1".into(),
            },
            Diagnostic {
                severity: Severity::Error,
                line: 2,
                message: "e2".into(),
            },
        ];
        assert_eq!(
            spans_text(&row_text(&Row::Tool(edit), &ctx).note),
            format!("{} 2 +2 -1", icons::DIAG_ERROR)
        );

        let mut agent = tool_row("Agent", json!({"description": "work"}), Status::Ok);
        agent.usage = Some(AgentUsage {
            tool_uses: 36,
            tokens: 113_000,
            duration_ms: 185_000,
        });
        assert_eq!(
            spans_text(&row_text(&Row::Tool(agent), &ctx).note),
            format!("{} 36 · 113k · 3m 5s", icons::TOOL)
        );

        let mut failed = tool_row("Bash", json!({"command": "false"}), Status::Failed);
        failed.result = Some(ToolResult {
            text: "Exit code 1\nx".into(),
            is_error: true,
            extra: json!({}),
        });
        assert_eq!(
            spans_text(&row_text(&Row::Tool(failed), &ctx).note),
            "Exit code 1"
        );

        let running = tool_row("Bash", json!({"command": "sleep 1"}), Status::Running);
        assert_eq!(
            spans_text(&row_text(&Row::Tool(running), &ctx).note),
            "(running)"
        );
    }

    #[test]
    fn row_is_one_line_with_note_at_the_right() {
        let ctx = Ctx {
            home: None,
            icon_width: 1,
        };
        let mut bash = tool_row(
            "Bash",
            json!({"command": "echo a_very_long_command_that_definitely_overflows_forty_columns"}),
            Status::Ok,
        );
        bash.result = Some(ToolResult {
            text: "1\n2\n3\n".into(),
            is_error: false,
            extra: json!({}),
        });
        let rt = row_text(&Row::Tool(bash), &ctx);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 1));
        render_row(&mut buf, 0, 0, 40, &rt, 1);
        let line = buf_line(&buf, 0, 40);
        assert_eq!(buf[(39, 0)].symbol(), "3");
        assert!(line.contains('…'));
    }

    #[test]
    fn japanese_summary_truncates_by_width() {
        let ctx = Ctx {
            home: None,
            icon_width: 1,
        };
        let prompt = PromptRow {
            prompt: Prompt::Text("あいうえおかきくけこさしすせそ".into()),
            duration_ms: None,
        };
        let rt = row_text(&Row::Prompt(prompt), &ctx);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        render_row(&mut buf, 0, 0, 20, &rt, 1);
        let line = buf_line(&buf, 0, 20);
        let trimmed = line.trim_end();
        assert!(unicode_width::UnicodeWidthStr::width(trimmed) <= 20);
        assert!(trimmed.ends_with('…'));
    }

    #[test]
    fn narrow_widths_do_not_panic() {
        let ctx = Ctx {
            home: None,
            icon_width: 1,
        };
        let bash = tool_row("Bash", json!({"command": "ls"}), Status::Ok);
        let rt = row_text(&Row::Tool(bash), &ctx);
        let compact = CompactRow {
            manual: true,
            pre_tokens: Some(630_000),
            post_tokens: Some(10_000),
            summary: None,
        };
        for w in 0..=6 {
            let mut buf = Buffer::empty(Rect::new(0, 0, w.max(1), 1));
            render_row(&mut buf, 0, 0, w, &rt, 1);
            render_compact(&mut buf, 0, 0, w, &compact, 1);
        }
    }

    #[test]
    fn wide_icons_skip_the_next_cell() {
        let ctx = Ctx {
            home: None,
            icon_width: 2,
        };
        let bash = tool_row("Bash", json!({"command": "echo"}), Status::Ok);
        let rt = row_text(&Row::Tool(bash), &ctx);

        let mut buf1 = Buffer::empty(Rect::new(0, 0, 30, 1));
        render_row(&mut buf1, 0, 0, 30, &rt, 1);
        let start1 = (0..30).find(|&x| buf1[(x, 0)].symbol() == "e").unwrap();

        let mut buf2 = Buffer::empty(Rect::new(0, 0, 30, 1));
        render_row(&mut buf2, 0, 0, 30, &rt, 2);
        assert_eq!(buf2[(2, 0)].diff_option, CellDiffOption::Skip);
        assert_eq!(buf2[(5, 0)].diff_option, CellDiffOption::Skip);
        let start2 = (0..30).find(|&x| buf2[(x, 0)].symbol() == "e").unwrap();
        assert!(start2 > start1);
    }

    #[test]
    fn compact_line() {
        let compact = CompactRow {
            manual: true,
            pre_tokens: Some(630_000),
            post_tokens: Some(10_000),
            summary: None,
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 1));
        render_compact(&mut buf, 0, 0, 60, &compact, 1);
        let line = buf_line(&buf, 0, 60);
        assert!(line.contains("compacted · manual"));
        assert!(line.contains("630k → 10k"));
        assert_eq!(buf[(59, 0)].symbol(), "k");
    }
}
