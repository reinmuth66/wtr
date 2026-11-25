//! Renders Edit tool `structuredPatch` diffs via `delta` and wraps them with a fixed gutter.

use std::ffi::OsStr;
use std::io::Write;
use std::process::{Command, Stdio};

use ansi_to_tui::IntoText;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

use crate::{external, wrap};

/// Builds a unified diff string with `a/` and `b/` headers from a `structuredPatch` JSON value.
pub fn unified_diff(file_name: &str, patch: &Value) -> Option<String> {
    let hunks = patch.as_array()?;
    if hunks.is_empty() {
        return None;
    }

    let mut out = format!("--- a/{file_name}\n+++ b/{file_name}\n");
    for hunk in hunks {
        let old_start = hunk.get("oldStart").and_then(Value::as_u64).unwrap_or(0);
        let old_lines = hunk.get("oldLines").and_then(Value::as_u64).unwrap_or(0);
        let new_start = hunk.get("newStart").and_then(Value::as_u64).unwrap_or(0);
        let new_lines = hunk.get("newLines").and_then(Value::as_u64).unwrap_or(0);
        out.push_str(&format!(
            "@@ -{old_start},{old_lines} +{new_start},{new_lines} @@\n"
        ));
        if let Some(lines) = hunk.get("lines").and_then(Value::as_array) {
            for line in lines {
                if let Some(s) = line.as_str() {
                    out.push_str(s);
                    out.push('\n');
                }
            }
        }
    }
    Some(out)
}

/// Returns the maximum line number referenced by any hunk in `patch`.
pub fn max_line_number(patch: &Value) -> u64 {
    let Some(hunks) = patch.as_array() else {
        return 0;
    };
    hunks
        .iter()
        .map(|h| {
            let old_end = h
                .get("oldStart")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .saturating_add(h.get("oldLines").and_then(Value::as_u64).unwrap_or(0));
            let new_end = h
                .get("newStart")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .saturating_add(h.get("newLines").and_then(Value::as_u64).unwrap_or(0));
            old_end.max(new_end)
        })
        .max()
        .unwrap_or(0)
}

/// Fixed `delta` CLI arguments with Catppuccin Macchiato colors and `digits`-wide line numbers.
pub fn delta_args(digits: usize) -> Vec<String> {
    [
        "--no-gitconfig",
        "--line-numbers",
        "--syntax-theme",
        "Catppuccin Macchiato",
        "--file-style",
        "omit",
        "--hunk-header-style",
        "omit",
        "--line-numbers-left-format",
        &format!("{{nm:>{digits}}} "),
        "--line-numbers-right-format",
        &format!("{{np:>{digits}}} │"),
        "--minus-style",
        "syntax #493142",
        "--minus-emph-style",
        "syntax #6e3f53",
        "--plus-style",
        "syntax #344637",
        "--plus-emph-style",
        "syntax #4a6649",
        "--line-numbers-minus-style",
        "#ed8796",
        "--line-numbers-plus-style",
        "#a6da95",
        "--line-numbers-zero-style",
        "#6e738d",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Pipes `diff` through `delta` found in `path_var` and returns its ANSI output.
pub fn run_delta(diff: &str, digits: usize, path_var: Option<&OsStr>) -> Result<String, String> {
    let exe =
        external::find_in_path("delta", path_var).ok_or_else(|| String::from("delta not found"))?;
    let mut child = Command::new(exe)
        .args(delta_args(digits))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn delta: {e}"))?;

    let writer = child.stdin.take().map(|mut stdin| {
        let bytes = diff.as_bytes().to_vec();
        std::thread::spawn(move || stdin.write_all(&bytes))
    });

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait for delta: {e}"))?;

    if let Some(handle) = writer {
        handle
            .join()
            .map_err(|_| String::from("delta stdin writer thread panicked"))?
            .map_err(|e| format!("failed to write to delta stdin: {e}"))?;
    }
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parsed `delta` output ready to be wrapped to any viewport width without re-running `delta`.
#[derive(Debug, Clone)]
pub struct DiffRender {
    lines: Vec<Line<'static>>,
    gutter: usize,
}

/// Parses `delta` ANSI output into ratatui lines, dropping the leading blank line.
pub fn parse_delta(ansi: &str, digits: usize) -> DiffRender {
    let mut lines: Vec<Line<'static>> = match ansi.into_text() {
        Ok(text) => text
            .lines
            .into_iter()
            .map(|l| {
                let spans: Vec<Span<'static>> = l
                    .spans
                    .into_iter()
                    .map(|s| Span::styled(s.content.into_owned(), s.style))
                    .collect();
                Line::from(spans).style(l.style)
            })
            .collect(),
        Err(_) => ansi.lines().map(|l| Line::raw(l.to_string())).collect(),
    };

    if lines
        .first()
        .is_some_and(|l| l.spans.iter().all(|s| s.content.is_empty()))
    {
        lines.remove(0);
    }

    DiffRender {
        lines,
        gutter: 2 * digits + 3,
    }
}

impl DiffRender {
    /// Wraps the content column to `width - gutter`, keeping the line-number gutter intact.
    pub fn wrap(&self, width: usize) -> Vec<Line<'static>> {
        if width <= self.gutter {
            return self.lines.clone();
        }
        let content_width = width - self.gutter;
        let mut out = Vec::new();

        for line in &self.lines {
            let bg = line.spans.last().and_then(|s| s.style.bg);
            let (gutter_spans, content_spans) = split_at_width(&line.spans, self.gutter);
            let wrapped = wrap::wrap_spans(&content_spans, content_width);

            for (idx, mut row) in wrapped.into_iter().enumerate() {
                let mut spans = if idx == 0 {
                    gutter_spans.clone()
                } else {
                    vec![Span::raw(" ".repeat(self.gutter))]
                };
                spans.append(&mut row.spans);
                let mut combined = Line::from(spans).style(line.style);
                if let Some(bg_color) = bg {
                    let row_width = combined.width();
                    if row_width < width {
                        combined.spans.push(Span::styled(
                            " ".repeat(width - row_width),
                            Style::new().bg(bg_color),
                        ));
                    }
                }
                out.push(combined);
            }
        }
        out
    }
}

fn split_at_width(spans: &[Span<'_>], target: usize) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut used = 0;
    let mut in_right = false;

    for span in spans {
        if in_right {
            right.push(Span::styled(span.content.to_string(), span.style));
            continue;
        }
        let mut left_buf = String::new();
        let mut right_buf = String::new();
        for c in span.content.chars() {
            if in_right {
                right_buf.push(c);
                continue;
            }
            let w = c.width().unwrap_or(0);
            if w > 0 && used + w > target {
                in_right = true;
                right_buf.push(c);
            } else {
                used += w;
                left_buf.push(c);
                if used >= target {
                    in_right = true;
                }
            }
        }
        if !left_buf.is_empty() {
            left.push(Span::styled(left_buf, span.style));
        }
        if !right_buf.is_empty() {
            right.push(Span::styled(right_buf, span.style));
        }
    }

    (left, right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use serde_json::{Value, json};
    use std::ffi::OsStr;

    fn patch() -> Value {
        json!([{"oldStart":72,"oldLines":3,"newStart":72,"newLines":4,"lines":[" a","-b","+c","+d"," e"]}])
    }

    #[test]
    fn builds_unified_diff() {
        assert_eq!(
            unified_diff("x.rs", &patch()).unwrap(),
            "--- a/x.rs\n+++ b/x.rs\n@@ -72,3 +72,4 @@\n a\n-b\n+c\n+d\n e\n"
        );
        assert_eq!(unified_diff("x.rs", &json!([])), None);
        assert_eq!(unified_diff("x.rs", &Value::Null), None);
    }

    #[test]
    fn digits() {
        assert_eq!(max_line_number(&patch()), 76);
    }

    #[test]
    fn args_fix_the_line_number_columns() {
        let a = delta_args(2);
        assert!(a.contains(&"--no-gitconfig".to_string()));
        assert!(
            a.windows(2)
                .any(|w| w[0] == "--line-numbers-left-format" && w[1] == "{nm:>2} ")
        );
        assert!(!a.iter().any(|s| s.starts_with("--width")));
    }

    #[test]
    fn wrap_keeps_the_gutter_and_fills_the_background() {
        let rendered = parse_delta(
            "\n\x1b[48;2;52;70;55m 72 ⋮ 73 │long line text\x1b[0K\x1b[0m\n",
            2,
        );
        let wrapped = rendered.wrap(14);
        assert!(wrapped.len() >= 2);
        let second_text: String = wrapped[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(second_text.starts_with(&" ".repeat(7)));
        for line in &wrapped {
            assert_eq!(line.width(), 14);
            let last_span = line.spans.last().unwrap();
            assert_eq!(last_span.style.bg, Some(Color::Rgb(52, 70, 55)));
        }
    }

    #[test]
    fn delta_renders_colors() {
        let path_var = std::env::var_os("PATH");
        if crate::external::find_in_path("delta", path_var.as_deref()).is_none() {
            return;
        }
        let diff = unified_diff("x.rs", &patch()).unwrap();
        let out = run_delta(&diff, 2, path_var.as_deref()).unwrap();
        assert!(out.contains("\x1b["));
        assert!(!out.contains("a/x.rs"));
    }

    #[test]
    fn delta_handles_large_diff() {
        let path_var = std::env::var_os("PATH");
        if crate::external::find_in_path("delta", path_var.as_deref()).is_none() {
            return;
        }
        let lines: Vec<String> = (0..20_000)
            .map(|i| format!("+let line_{i:05} = \"padding_to_exceed_one_megabyte_easily\";"))
            .collect();
        let patch = json!([{
            "oldStart": 1,
            "oldLines": 0,
            "newStart": 1,
            "newLines": lines.len(),
            "lines": lines,
        }]);
        let diff = unified_diff("large.rs", &patch).unwrap();
        assert!(diff.len() >= 1_048_576);
        let out = run_delta(&diff, 5, path_var.as_deref()).unwrap();
        assert!(!out.is_empty());
    }

    #[test]
    fn missing_delta() {
        assert_eq!(
            run_delta("", 2, Some(OsStr::new(""))).unwrap_err(),
            "delta not found"
        );
    }
}
