//! Drawing of the header line and the document body.
//!
//! Every widget clips to its area. `wrap_spans` can produce a row wider than
//! the text area (a character wider than the width sits alone on its row), so
//! nothing here assumes a row fits.

use std::path::Path;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::highlight::{HlCache, plain_spans};
use crate::icons;
use crate::kind::{FileKind, format_size};
use crate::text_doc::TextDoc;
use crate::theme;
use crate::wrap::{Rows, Scroll, row_count, wrap_spans};

/// Columns taken by the line numbers: the digits (at least 2) and one space.
pub fn gutter_width(line_count: usize) -> usize {
    line_count.to_string().len().max(2) + 1
}

/// Columns left for the text; never less than 1.
pub fn text_width(area_width: u16, line_count: usize) -> usize {
    (area_width as usize)
        .saturating_sub(gutter_width(line_count))
        .max(1)
}

/// Row counts of a document wrapped at `width` text columns.
pub struct DocRows<'a> {
    pub doc: &'a TextDoc,
    pub width: usize,
}

impl Rows for DocRows<'_> {
    fn len(&self) -> usize {
        self.doc.lines.len()
    }

    fn rows(&self, line: usize) -> usize {
        row_count(&self.doc.lines[line], self.width)
    }
}

/// Writes `spans` from (`x`, `y`), never past `max_w` columns.
fn draw_spans(buf: &mut Buffer, x: u16, y: u16, max_w: u16, spans: &[Span<'_>]) {
    let (mut x, mut left) = (x, max_w);
    for span in spans {
        if left == 0 {
            break;
        }
        let (nx, _) = buf.set_stringn(x, y, span.content.as_ref(), left as usize, span.style);
        left -= nx - x;
        x = nx;
    }
}

/// The document with line numbers, wrapped to the area.
pub struct TextView<'a> {
    pub doc: &'a TextDoc,
    pub hl: &'a HlCache,
    pub scroll: Scroll,
}

impl Widget for TextView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let count = self.doc.lines.len();
        let gutter = gutter_width(count);
        let width = text_width(area.width, count);
        let number_style = Style::default().fg(theme::OVERLAY0);
        // Columns of the gutter that can hold digits (the last one is a space).
        let digits = gutter - 1;
        let text_x = area.x.saturating_add(gutter.min(u16::MAX as usize) as u16);
        let text_w = area.right().saturating_sub(text_x);

        let mut y = area.y;
        let mut skip = self.scroll.row;
        for i in self.scroll.line..count {
            let plain;
            let spans = match self.hl.line(i) {
                Some(s) => s,
                None => {
                    plain = plain_spans(&self.doc.lines[i]);
                    &plain
                }
            };
            for (n, row) in wrap_spans(spans, width).iter().enumerate() {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                if y >= area.bottom() {
                    return;
                }
                if n == 0 {
                    let num = format!("{:>digits$}", i + 1);
                    buf.set_stringn(area.x, y, &num, area.width as usize, number_style);
                }
                draw_spans(buf, text_x, y, text_w, &row.spans);
                y += 1;
            }
            skip = 0;
        }
    }
}

/// Ready-made lines (a table), drawn without wrapping or line numbers.
pub struct LinesView<'a> {
    pub lines: &'a [Line<'static>],
    pub scroll: Scroll,
}

impl Widget for LinesView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let shown = self.lines.iter().skip(self.scroll.line);
        for (y, line) in (area.y..area.bottom()).zip(shown) {
            buf.set_line(area.x, y, line, area.width);
        }
    }
}

/// The one-line description of a file wtr cannot show itself.
pub fn info_lines(label: &str, size: u64) -> Vec<Line<'static>> {
    vec![Line::styled(
        format!("{} {label} · {}", icons::FILE_OTHER, format_size(size)),
        Style::default().fg(theme::SUBTEXT0),
    )]
}

pub fn no_file_line() -> Line<'static> {
    Line::styled(
        format!("{} no file", icons::NO_FILE),
        Style::default().fg(theme::OVERLAY1),
    )
}

/// Cuts `s` to at most `width` columns, ending in `…` when something is cut.
fn truncate_to(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

fn kind_mark(kind: Option<FileKind>) -> &'static str {
    match kind {
        Some(FileKind::Text) => icons::FILE_TEXT,
        Some(FileKind::Json) => icons::FILE_JSON,
        Some(FileKind::Csv | FileKind::Tsv) => icons::FILE_TABLE,
        Some(FileKind::Image) => icons::FILE_IMAGE,
        Some(FileKind::Other) | None => icons::FILE_OTHER,
    }
}

/// Kind mark and file name on the left, `message` on the right. When they do
/// not both fit, the left side is cut and ends in `…`. The line is at most
/// `width` columns.
pub fn header_line(
    path: Option<&Path>,
    kind: Option<FileKind>,
    message: Option<&str>,
    width: u16,
) -> Line<'static> {
    let width = width as usize;
    let name = path
        .map(|p| match p.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => p.display().to_string(),
        })
        .unwrap_or_default();
    let left = format!("{} {name}", kind_mark(kind));
    let left_style = Style::default()
        .fg(theme::BLUE)
        .add_modifier(Modifier::BOLD);

    let Some(message) = message else {
        return Line::styled(truncate_to(&left, width), left_style);
    };
    let right = truncate_to(&format!("{} {message}", icons::ALERT), width);
    let right_w = right.width();
    // One column between the two sides, when there is room for both.
    let left = truncate_to(&left, width.saturating_sub(right_w + 1));
    let gap = width - left.width() - right_w;
    Line::from(vec![
        Span::styled(left, left_style),
        Span::raw(" ".repeat(gap)),
        Span::styled(right, Style::default().fg(theme::YELLOW)),
    ])
}

/// The mark and `message`, at most `width` columns and no wider than the
/// text, so that drawn at the right end it covers only its own cells.
pub fn message_line(message: &str, width: u16) -> Line<'static> {
    let text = truncate_to(&format!("{} {message}", icons::ALERT), width as usize);
    Line::styled(text, Style::default().fg(theme::YELLOW))
}

#[cfg(test)]
mod tests {
    #[test]
    fn message_line_is_only_the_text() {
        let s = text_of(&message_line("fx not found", 30));
        assert!(s.ends_with("fx not found") && !s.starts_with(' '));
        assert_eq!(s.width(), format!("{} fx not found", icons::ALERT).width());
        assert!(text_of(&message_line("nvim not found", 5)).width() <= 5);
    }

    use super::*;
    use crate::highlight::Highlighter;
    use crate::text_doc::SyntaxHint;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(doc: &TextDoc, w: u16, h: u16, scroll: Scroll) -> Vec<String> {
        let hl = Highlighter::new();
        let cache = HlCache::new(&hl, doc, true);
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| {
            f.render_widget(
                TextView {
                    doc,
                    hl: &cache,
                    scroll,
                },
                f.area(),
            )
        })
        .unwrap();
        let b = t.backend().buffer().clone();
        // In ratatui 0.30 the second cell of a wide character holds " ", so
        // the cell after a width-2 symbol is skipped.
        (0..h)
            .map(|y| {
                let (mut s, mut x) = (String::new(), 0);
                while x < w {
                    let sym = b[(x, y)].symbol();
                    s.push_str(sym);
                    x += UnicodeWidthStr::width(sym).max(1) as u16;
                }
                s
            })
            .collect()
    }

    fn d(lines: &[&str]) -> TextDoc {
        TextDoc {
            lines: lines.iter().map(|s| s.to_string()).collect(),
            hint: SyntaxHint::Path("a.txt".into()),
        }
    }

    fn text_of(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn numbers_and_wrap() {
        // The gutter is 3 columns (" 1 ") and the text 6.
        let rows = render(&d(&["あいうえお", "x"]), 9, 3, Scroll::default());
        assert!(
            rows[0].starts_with(" 1 あ")
                && rows[1].starts_with("   え")
                && rows[2].starts_with(" 2 x")
        );
    }

    #[test]
    fn scroll_starts_mid_line() {
        let rows = render(&d(&["あいうえお", "x"]), 9, 2, Scroll { line: 0, row: 1 });
        assert!(rows[0].starts_with("   え") && rows[1].starts_with(" 2 x"));
    }

    #[test]
    fn narrower_than_gutter_does_not_panic() {
        render(&d(&["abc"; 120]), 2, 3, Scroll::default());
        render(&d(&["abc"; 120]), 1, 3, Scroll::default());
    }

    #[test]
    fn zero_sized_areas_do_not_panic() {
        render(&d(&["abc"]), 0, 3, Scroll::default());
        render(&d(&["abc"]), 5, 0, Scroll::default());
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 4));
        let lines = [Line::raw("abc")];
        LinesView {
            lines: &lines,
            scroll: Scroll::default(),
        }
        .render(Rect::new(0, 0, 0, 0), &mut buf);
    }

    #[test]
    fn gutter_grows_with_digits() {
        assert_eq!(gutter_width(9), 3);
        assert_eq!(gutter_width(1000), 5);
        assert_eq!(text_width(2, 120), 1);
    }

    #[test]
    fn wide_char_in_narrow_text_area_stays_inside() {
        // The text area is 1 column; "あ" is 2 wide and must be clipped.
        let doc = d(&["あい"]);
        let hl = Highlighter::new();
        let cache = HlCache::new(&hl, &doc, true);
        let outer = Rect::new(0, 0, 10, 4);
        let area = Rect::new(2, 1, 4, 2);
        let mut buf = Buffer::empty(outer);
        TextView {
            doc: &doc,
            hl: &cache,
            scroll: Scroll::default(),
        }
        .render(area, &mut buf);
        for y in 0..4 {
            for x in 0..10 {
                if !area.contains((x, y).into()) {
                    assert_eq!(buf[(x, y)].symbol(), " ", "({x},{y}) was written");
                }
            }
        }
    }

    #[test]
    fn doc_rows_use_the_width() {
        let doc = d(&["あいうえお", ""]);
        let r = DocRows {
            doc: &doc,
            width: 6,
        };
        assert_eq!((r.len(), r.rows(0), r.rows(1)), (2, 2, 1));
    }

    #[test]
    fn lines_view_starts_at_scroll_line() {
        let lines = [Line::raw("one"), Line::raw("two"), Line::raw("three")];
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        LinesView {
            lines: &lines,
            scroll: Scroll { line: 1, row: 0 },
        }
        .render(Rect::new(0, 0, 4, 2), &mut buf);
        let row = |y| (0..4).map(|x| buf[(x, y)].symbol()).collect::<String>();
        assert_eq!((row(0).as_str(), row(1).as_str()), ("two ", "thre"));
    }

    #[test]
    fn info_and_no_file_lines() {
        let info = info_lines("PDF", 2048);
        assert_eq!(info.len(), 1);
        assert_eq!(
            text_of(&info[0]),
            format!("{} PDF · {}", icons::FILE_OTHER, format_size(2048))
        );
        assert_eq!(
            text_of(&no_file_line()),
            format!("{} no file", icons::NO_FILE)
        );
    }

    #[test]
    fn header_truncates_name() {
        let l = header_line(
            Some(Path::new("/x/averyveryverylongname.rs")),
            Some(FileKind::Text),
            Some("nvim not found"),
            30,
        );
        let s = text_of(&l);
        assert!(s.contains('…') && s.ends_with("nvim not found") && s.width() <= 30);
    }

    #[test]
    fn header_fits_without_cutting() {
        let l = header_line(
            Some(Path::new("/x/a.json")),
            Some(FileKind::Json),
            Some("hi"),
            30,
        );
        let s = text_of(&l);
        assert!(s.starts_with(&format!("{} a.json", icons::FILE_JSON)));
        assert!(s.ends_with(&format!("{} hi", icons::ALERT)));
        assert_eq!(s.width(), 30);
        assert!(!s.contains('…'));
    }

    #[test]
    fn header_without_message_or_path() {
        let s = text_of(&header_line(None, None, None, 10));
        assert_eq!(s, format!("{} ", icons::FILE_OTHER));
        let s = text_of(&header_line(
            Some(Path::new("/x/abc.txt")),
            Some(FileKind::Text),
            None,
            4,
        ));
        assert_eq!(s.width(), 4);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn header_never_exceeds_width() {
        for w in 0..24u16 {
            let l = header_line(
                Some(Path::new("/x/abcdefgh.rs")),
                Some(FileKind::Text),
                Some("nvim not found"),
                w,
            );
            assert!(text_of(&l).width() <= w as usize, "width {w}");
        }
    }
}
