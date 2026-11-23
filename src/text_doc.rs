use std::path::{Path, PathBuf};

use unicode_width::UnicodeWidthChar;

use crate::kind::FileKind;

pub const TAB_WIDTH: usize = 4;

#[derive(Clone, Debug, PartialEq)]
pub enum SyntaxHint {
    Path(PathBuf),
    Json,
}

#[derive(Clone, Debug)]
pub struct TextDoc {
    pub lines: Vec<String>,
    pub hint: SyntaxHint,
}

/// Splits `bytes` into display-safe lines. A `Json` file that parses is
/// pretty-printed and hinted as JSON; anything else keeps its own text.
pub fn text_doc_from_bytes(path: &Path, bytes: &[u8], kind: FileKind) -> TextDoc {
    let text = decode_text(bytes);
    if kind == FileKind::Json
        && let Some(pretty) = pretty_json(&text)
    {
        return TextDoc {
            lines: split_lines(&pretty),
            hint: SyntaxHint::Json,
        };
    }
    TextDoc {
        lines: split_lines(&text),
        hint: SyntaxHint::Path(path.to_path_buf()),
    }
}

/// Re-indents with two spaces. Key order and number text are preserved.
pub fn pretty_json(s: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(s).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

/// True when the first non-blank character is `{` or `[` and the whole input
/// reads as JSON. Lets a file without an extension be recognized as JSON.
pub fn is_json_document(bytes: &[u8]) -> bool {
    let text = decode_text(bytes);
    let trimmed = text.trim();
    trimmed.starts_with(['{', '[']) && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
}

/// Decodes UTF-16 (by BOM) and UTF-8 (BOM removed). Anything else is read as
/// UTF-8 with invalid bytes shown as U+FFFD; no other encoding is converted.
pub fn decode_text(bytes: &[u8]) -> String {
    match bytes {
        [0xff, 0xfe, rest @ ..] => decode_utf16(rest, u16::from_le_bytes),
        [0xfe, 0xff, rest @ ..] => decode_utf16(rest, u16::from_be_bytes),
        [0xef, 0xbb, 0xbf, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| unit(*c)).collect();
    String::from_utf16_lossy(&units)
}

/// Makes one line safe to measure and print: drops a trailing `\r`, expands
/// tabs to the next multiple of `TAB_WIDTH` display columns, and shows other
/// control characters in caret notation (ESC -> `^[`, DEL -> `^?`).
pub fn clean_line(raw: &str) -> String {
    let line = raw.strip_suffix('\r').unwrap_or(raw);
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    for c in line.chars() {
        if c == '\t' {
            let pad = TAB_WIDTH - col % TAB_WIDTH;
            out.extend(std::iter::repeat_n(' ', pad));
            col += pad;
        } else if c.is_ascii_control() {
            out.push('^');
            out.push((c as u8 ^ 0x40) as char);
            col += 2;
        } else {
            out.push(c);
            col += c.width().unwrap_or(0);
        }
    }
    out
}

fn split_lines(text: &str) -> Vec<String> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    if body.is_empty() && text.is_empty() {
        return Vec::new();
    }
    body.split('\n').map(clean_line).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_drops_trailing_newline() {
        let d = text_doc_from_bytes(Path::new("a.txt"), b"a\r\nb\n", FileKind::Text);
        assert_eq!(d.lines, vec!["a", "b"]);
        assert!(
            text_doc_from_bytes(Path::new("a.txt"), b"", FileKind::Text)
                .lines
                .is_empty()
        );
    }

    #[test]
    fn tabs_expand_by_display_width() {
        assert_eq!(clean_line("a\tb"), "a   b");
        assert_eq!(clean_line("あ\tb"), "あ  b");
    }

    #[test]
    fn control_chars_are_visible() {
        assert_eq!(clean_line("x\x1b[31my\x7f"), "x^[[31my^?");
    }

    #[test]
    fn json_is_pretty_and_keeps_order_and_numbers() {
        let d = text_doc_from_bytes(
            Path::new("a.json"),
            br#"{"b":1,"a":12345678901234567890}"#,
            FileKind::Json,
        );
        assert_eq!(
            d.lines,
            vec!["{", "  \"b\": 1,", "  \"a\": 12345678901234567890", "}"]
        );
        assert_eq!(d.hint, SyntaxHint::Json);
    }

    #[test]
    fn decodes_utf16_and_strips_boms() {
        assert_eq!(decode_text(&[0xff, 0xfe, b'a', 0, 0x42, 0x30]), "aあ");
        assert_eq!(decode_text(&[0xfe, 0xff, 0, b'a']), "a");
        assert_eq!(decode_text(b"\xef\xbb\xbfx"), "x");
        assert_eq!(decode_text(&[0x61, 0xff]), "a\u{fffd}");
    }

    #[test]
    fn json_document_detection() {
        assert!(is_json_document(b"  {\"a\": [1]}\n"));
        assert!(is_json_document(b"[1,2]"));
        assert!(!is_json_document(b"{\"a\":"));
        assert!(!is_json_document(b"\"just a string\""));
        assert!(!is_json_document(b"key = 1"));
    }

    #[test]
    fn broken_json_is_kept() {
        let d = text_doc_from_bytes(Path::new("a.json"), b"{\"a\":", FileKind::Json);
        assert_eq!(d.lines, vec!["{\"a\":"]);
        assert_eq!(d.hint, SyntaxHint::Path("a.json".into()));
    }
}
