use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{icons, theme};

pub const MAX_ROWS: usize = 300;
pub const MAX_COL_WIDTH: usize = 40;

const SEPARATOR: &str = " \u{2502} ";
const NEWLINE_MARK: &str = "\u{21B5}";
const ELLIPSIS: &str = "\u{2026}";

#[derive(Debug, PartialEq)]
pub struct Table {
    pub rows: Vec<Vec<String>>,
    /// True when the input has more rows than were read.
    pub truncated: bool,
}

/// Reads at most `max_rows` rows. Reads one more record only to learn whether
/// the input was cut; nothing past that is read. Short rows are padded with
/// empty cells so every row has the same length.
pub fn read_table<R: std::io::Read>(
    r: R,
    delimiter: u8,
    max_rows: usize,
) -> Result<Table, csv::Error> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter)
        .from_reader(r);
    let mut rec = csv::ByteRecord::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    while rows.len() < max_rows && rdr.read_byte_record(&mut rec)? {
        rows.push(
            rec.iter()
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .collect(),
        );
    }
    let truncated = rdr.read_byte_record(&mut rec)?;
    let ncols = rows.iter().map(Vec::len).max().unwrap_or(0);
    for row in &mut rows {
        row.resize(ncols, String::new());
    }
    Ok(Table { rows, truncated })
}

/// Makes a cell safe to measure and print: newlines become a mark, a tab one
/// space, and other control characters caret notation (ESC -> `^[`).
fn flatten(cell: &str) -> String {
    let mut out = String::with_capacity(cell.len());
    let mut chars = cell.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str(NEWLINE_MARK);
            }
            '\n' => out.push_str(NEWLINE_MARK),
            '\t' => out.push(' '),
            '\0'..='\x1f' | '\x7f' => {
                out.push('^');
                out.push((c as u8 ^ 0x40) as char);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Cuts `s` to at most `max` columns, ending with an ellipsis when cut.
fn clip(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push_str(ELLIPSIS);
    out
}

pub fn render_table(t: &Table) -> Vec<Line<'static>> {
    let cells: Vec<Vec<String>> = t
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|c| clip(&flatten(c), MAX_COL_WIDTH))
                .collect()
        })
        .collect();
    let ncols = cells.first().map_or(0, Vec::len);
    let widths: Vec<usize> = (0..ncols)
        .map(|i| {
            cells
                .iter()
                .map(|row| row[i].width())
                .max()
                .unwrap_or(0)
                .min(MAX_COL_WIDTH)
        })
        .collect();

    let header = Style::default()
        .fg(theme::BLUE)
        .add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line<'static>> = cells
        .into_iter()
        .enumerate()
        .map(|(ri, row)| {
            let style = if ri == 0 { header } else { Style::default() };
            let last = row.len().saturating_sub(1);
            let mut spans = Vec::with_capacity(row.len() * 2);
            for (ci, cell) in row.into_iter().enumerate() {
                if ci > 0 {
                    spans.push(Span::styled(
                        SEPARATOR,
                        Style::default().fg(theme::OVERLAY0),
                    ));
                }
                let pad = if ci == last {
                    0
                } else {
                    widths[ci].saturating_sub(cell.width())
                };
                let mut text = cell;
                text.push_str(&" ".repeat(pad));
                spans.push(Span::styled(text, style));
            }
            Line::from(spans)
        })
        .collect();
    if t.truncated {
        lines.push(Line::styled(
            format!("{} first {MAX_ROWS} rows", icons::TRUNCATED),
            Style::default().fg(theme::OVERLAY1),
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(s: &str, d: u8) -> Table {
        read_table(s.as_bytes(), d, MAX_ROWS).unwrap()
    }
    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn quoted_comma_and_newline() {
        let t = read("a,b\n\"x,y\",\"1\n2\"\n", b',');
        assert_eq!(t.rows, vec![vec!["a", "b"], vec!["x,y", "1\n2"]]);
    }
    #[test]
    fn ragged_rows_are_padded() {
        assert_eq!(read("a,b,c\n1\n", b',').rows[1], vec!["1", "", ""]);
    }
    #[test]
    fn tsv() {
        assert_eq!(read("a\tb\n", b'\t').rows[0], vec!["a", "b"]);
    }
    #[test]
    fn truncation() {
        let many: String = (0..301).map(|i| format!("{i}\n")).collect();
        let t = read(&many, b',');
        assert_eq!((t.rows.len(), t.truncated), (300, true));
        let exact: String = (0..300).map(|i| format!("{i}\n")).collect();
        assert!(!read(&exact, b',').truncated);
    }
    #[test]
    fn japanese_columns_align() {
        let lines = render_table(&read("名前,x\nあ,1\n", b','));
        let col = |l: &Line| {
            let s = text(l);
            unicode_width::UnicodeWidthStr::width(&s[..s.find('│').unwrap()])
        };
        assert_eq!(col(&lines[0]), col(&lines[1]));
    }
    #[test]
    fn newline_in_cell_is_marked() {
        assert!(text(&render_table(&read("\"1\n2\"\n", b','))[0]).contains("1↵2"));
    }
    #[test]
    fn tab_and_control_chars_are_visible_and_aligned() {
        let t = read("a\tb,x\n\"e\u{1b}[31mf\u{7f}\",y\nlong,z\n", b',');
        let lines = render_table(&t);
        let all: String = lines.iter().map(text).collect::<Vec<_>>().join("\n");
        assert!(all.contains("a b"), "{all:?}");
        assert!(all.contains("e^[[31mf^?"), "{all:?}");
        assert!(!all.chars().any(|c| c.is_control() && c != '\n'), "{all:?}");
        let col = |l: &Line| {
            let s = text(l);
            unicode_width::UnicodeWidthStr::width(&s[..s.find('│').unwrap()])
        };
        assert_eq!(col(&lines[0]), col(&lines[1]));
        assert_eq!(col(&lines[1]), col(&lines[2]));
    }
    #[test]
    fn crlf_and_lone_cr_are_one_mark() {
        let t = read("\"1\r\n2\r3\"\n", b',');
        assert!(text(&render_table(&t)[0]).contains("1↵2↵3"));
    }
    #[test]
    fn truncated_footer() {
        let many: String = (0..301).map(|i| format!("{i}\n")).collect();
        assert!(text(render_table(&read(&many, b',')).last().unwrap()).ends_with("first 300 rows"));
    }
}
