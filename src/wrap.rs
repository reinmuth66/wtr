//! Wrapping by display width and scroll position math.
//!
//! A document is a list of logical lines. Each logical line takes one or more
//! screen rows, depending on the width. `Scroll` points at a screen row by
//! (logical line, row inside that line).

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Decides where rows break. Shared by `wrap_spans` and `row_count` so they
/// always agree.
struct Breaker {
    width: usize,
    used: usize,
}

impl Breaker {
    fn new(width: usize) -> Self {
        Self { width: width.max(1), used: 0 }
    }

    /// Feeds one character; returns true when it starts a new row.
    fn step(&mut self, c: char) -> bool {
        let w = c.width().unwrap_or(0);
        // Zero-width characters stay attached to the preceding character.
        if w == 0 {
            return false;
        }
        let breaks = self.used > 0 && self.used + w > self.width;
        self.used = if breaks { w } else { self.used + w };
        breaks
    }
}

/// Splits one logical line into screen rows. An empty line is one row.
pub fn wrap_spans(spans: &[Span<'_>], width: usize) -> Vec<Line<'static>> {
    let mut breaker = Breaker::new(width);
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();

    for span in spans {
        let style: Style = span.style;
        let mut buf = String::new();
        for c in span.content.chars() {
            if breaker.step(c) {
                if !buf.is_empty() {
                    current.push(Span::styled(std::mem::take(&mut buf), style));
                }
                rows.push(Line::from(std::mem::take(&mut current)));
            }
            buf.push(c);
        }
        if !buf.is_empty() {
            current.push(Span::styled(buf, style));
        }
    }
    rows.push(Line::from(current));
    rows
}

/// Number of screen rows `text` takes; equals `wrap_spans(..).len()`.
pub fn row_count(text: &str, width: usize) -> usize {
    let mut breaker = Breaker::new(width);
    1 + text.chars().filter(|&c| breaker.step(c)).count()
}

/// Top of the screen: a logical line and the row inside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scroll {
    pub line: usize,
    pub row: usize,
}

impl Scroll {
    fn key(self) -> (usize, usize) {
        (self.line, self.row)
    }
}

/// Row counts of the logical lines.
#[allow(clippy::len_without_is_empty)] // the interface has no is_empty by design
pub trait Rows {
    fn len(&self) -> usize;
    /// Screen rows of `line`; 1 or more.
    fn rows(&self, line: usize) -> usize;
}

impl Rows for Vec<usize> {
    fn len(&self) -> usize {
        <[usize]>::len(self)
    }

    fn rows(&self, line: usize) -> usize {
        self[line].max(1)
    }
}

/// Moves forward by `n` screen rows, stopping where the last row of the last
/// line is at the bottom of the screen.
pub fn scroll_down(s: Scroll, n: usize, r: &impl Rows, height: usize) -> Scroll {
    let bottom = scroll_bottom(r, height);
    if s.key() >= bottom.key() {
        return bottom;
    }
    let mut line = s.line;
    let mut row = s.row.saturating_add(n);
    loop {
        if (line, row) >= bottom.key() {
            return bottom;
        }
        let rows = r.rows(line);
        if row < rows {
            return Scroll { line, row };
        }
        row -= rows;
        line += 1;
    }
}

/// Moves back by `n` screen rows, stopping at the top of the document.
pub fn scroll_up(s: Scroll, n: usize, r: &impl Rows) -> Scroll {
    let len = r.len();
    if len == 0 {
        return Scroll::default();
    }
    let (mut line, mut row) = if s.line >= len { (len, 0) } else { (s.line, s.row) };
    if n <= row {
        return Scroll { line, row: row - n };
    }
    let mut left = n - row;
    row = 0;
    loop {
        if line == 0 {
            return Scroll { line: 0, row };
        }
        line -= 1;
        let rows = r.rows(line);
        if left <= rows {
            return Scroll { line, row: rows - left };
        }
        left -= rows;
    }
}

/// The scroll position that puts the last row of the document at the bottom
/// of the screen. Walks back from the end, so it never counts the whole
/// document.
pub fn scroll_bottom(r: &impl Rows, height: usize) -> Scroll {
    let len = r.len();
    if len == 0 {
        return Scroll::default();
    }
    let mut remaining = height.max(1);
    let mut line = len - 1;
    loop {
        let rows = r.rows(line);
        if rows >= remaining {
            return Scroll { line, row: rows - remaining };
        }
        remaining -= rows;
        if line == 0 {
            return Scroll::default();
        }
        line -= 1;
    }
}

/// Puts `line` and `row` in range, and not below `scroll_bottom`.
pub fn clamp(s: Scroll, r: &impl Rows, height: usize) -> Scroll {
    let len = r.len();
    if len == 0 {
        return Scroll::default();
    }
    let bottom = scroll_bottom(r, height);
    if s.line >= len {
        return bottom;
    }
    let s = Scroll { line: s.line, row: s.row.min(r.rows(s.line) - 1) };
    if s.key() > bottom.key() { bottom } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn texts(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn ascii() {
        assert_eq!(texts(&wrap_spans(&[Span::raw("abcdef")], 4)), ["abcd", "ef"]);
    }

    #[test]
    fn wide_char_moves_to_next_row() {
        assert_eq!(texts(&wrap_spans(&[Span::raw("aあい")], 4)), ["aあ", "い"]);
    }

    #[test]
    fn too_wide_char_alone() {
        assert_eq!(texts(&wrap_spans(&[Span::raw("あい")], 1)), ["あ", "い"]);
    }

    #[test]
    fn styles_are_kept() {
        let red = Style::new().fg(Color::Red);
        let rows = wrap_spans(&[Span::styled("ab", red), Span::raw("cd")], 3);
        assert_eq!(texts(&rows), ["abc", "d"]);
        assert_eq!(rows[0].spans[0].style, red);
    }

    #[test]
    fn empty_and_zero_width() {
        assert_eq!(wrap_spans(&[Span::raw("")], 5).len(), 1);
        assert_eq!(row_count("abc", 0), 3);
    }

    #[test]
    fn row_count_matches_wrap() {
        for (t, w) in [("aあいうえおb", 3), ("", 2), ("abcdef", 4), ("あ", 1), ("e\u{301}e\u{301}e\u{301}", 2)] {
            assert_eq!(row_count(t, w), wrap_spans(&[Span::raw(t)], w).len());
        }
    }

    #[test]
    fn combining_mark_stays_with_previous_char() {
        let rows = wrap_spans(&[Span::raw("ae\u{301}b")], 2);
        assert_eq!(texts(&rows), ["ae\u{301}", "b"]);
        // A mark at the start of a row's worth of text never makes an empty row.
        assert_eq!(row_count("\u{301}", 1), 1);
    }

    #[test]
    fn scrolling() {
        let r = vec![1, 3, 1]; // screen rows 0 | 1 2 3 | 4, five in total
        let s0 = Scroll::default();
        assert_eq!(scroll_down(s0, 1, &r, 2), Scroll { line: 1, row: 0 });
        assert_eq!(scroll_down(s0, 2, &r, 2), Scroll { line: 1, row: 1 });
        assert_eq!(scroll_down(s0, 99, &r, 2), Scroll { line: 1, row: 2 });
        assert_eq!(scroll_bottom(&r, 2), Scroll { line: 1, row: 2 });
        assert_eq!(scroll_bottom(&r, 10), s0);
        assert_eq!(scroll_up(Scroll { line: 1, row: 1 }, 2, &r), s0);
        assert_eq!(scroll_up(Scroll { line: 2, row: 0 }, 1, &r), Scroll { line: 1, row: 2 });
    }

    #[test]
    fn clamping() {
        let r = vec![1, 3, 1];
        assert_eq!(clamp(Scroll { line: 9, row: 0 }, &r, 2), Scroll { line: 1, row: 2 });
        assert_eq!(clamp(Scroll { line: 0, row: 7 }, &r, 2), Scroll { line: 0, row: 0 });
        assert_eq!(clamp(Scroll { line: 3, row: 0 }, &Vec::<usize>::new(), 2), Scroll::default());
    }

    #[test]
    fn zero_height_does_not_panic() {
        let r = vec![1, 3, 1];
        let s0 = Scroll::default();
        assert_eq!(scroll_bottom(&r, 0), scroll_bottom(&r, 1));
        let _ = scroll_down(s0, 3, &r, 0);
        let _ = clamp(Scroll { line: 1, row: 1 }, &r, 0);
        let _ = scroll_up(Scroll { line: 9, row: 9 }, 1, &r);
    }

    #[test]
    fn scroll_bottom_does_not_walk_whole_document() {
        struct Huge;
        impl Rows for Huge {
            fn len(&self) -> usize {
                usize::MAX / 2
            }
            fn rows(&self, line: usize) -> usize {
                assert!(line > usize::MAX / 2 - 100, "walked too far back");
                1
            }
        }
        assert_eq!(scroll_bottom(&Huge, 5).line, usize::MAX / 2 - 5);
    }
}
