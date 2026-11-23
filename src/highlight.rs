use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use syntect::highlighting::{
    FontStyle, HighlightIterator, HighlightState, Highlighter as ThemeHighlighter, Theme,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use two_face::theme::EmbeddedThemeName;

use crate::text_doc::{SyntaxHint, TextDoc};
use crate::theme;

/// Files larger than this are shown without colors.
pub const PLAIN_THRESHOLD: u64 = 4 * 1024 * 1024;

/// Lines longer than this (in bytes) are not parsed, so one huge line cannot
/// block the render loop.
pub const MAX_HL_LINE: usize = 10_000;

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            syntaxes: two_face::syntax::extra_newlines(),
            theme: two_face::theme::extra()
                .get(EmbeddedThemeName::CatppuccinMacchiato)
                .clone(),
        }
    }

    pub fn syntax_name(&self, doc: &TextDoc) -> String {
        self.syntax_for(doc).name.clone()
    }

    fn syntax_by_name(&self, name: &str) -> &SyntaxReference {
        self.syntaxes
            .find_syntax_by_name(name)
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }

    fn syntax_for(&self, doc: &TextDoc) -> &SyntaxReference {
        let path = match &doc.hint {
            SyntaxHint::Json => return self.syntax_by_name("JSON"),
            SyntaxHint::Path(p) => p,
        };
        if let Ok(Some(s)) = self.syntaxes.find_syntax_for_file(path) {
            return s;
        }
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("jsonl") || e.eq_ignore_ascii_case("ndjson"))
        {
            return self.syntax_by_name("JSON");
        }
        doc.lines
            .first()
            .and_then(|l| self.syntaxes.find_syntax_by_first_line(l))
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }
}

/// One span in the default text color.
pub fn plain_spans(text: &str) -> Vec<Span<'static>> {
    vec![Span::styled(
        text.to_string(),
        Style::default().fg(theme::TEXT),
    )]
}

/// Highlighted lines of one document, produced from the top in small steps.
pub struct HlCache {
    lines: Vec<Vec<Span<'static>>>,
    parse: ParseState,
    highlight: HighlightState,
    plain: bool,
    total: usize,
}

impl HlCache {
    pub fn new(hl: &Highlighter, doc: &TextDoc, plain: bool) -> Self {
        let syntax = hl.syntax_for(doc);
        let theme_hl = ThemeHighlighter::new(&hl.theme);
        Self {
            lines: Vec::new(),
            parse: ParseState::new(syntax),
            highlight: HighlightState::new(&theme_hl, ScopeStack::new()),
            plain,
            total: doc.lines.len(),
        }
    }

    pub fn line(&self, i: usize) -> Option<&[Span<'static>]> {
        self.lines.get(i).map(Vec::as_slice)
    }

    pub fn done(&self) -> bool {
        self.plain || self.lines.len() >= self.total
    }

    /// Highlights further lines until `budget` has passed (at least one line
    /// per call). Returns true while lines remain.
    pub fn advance(&mut self, hl: &Highlighter, doc: &TextDoc, budget: Duration) -> bool {
        if self.done() {
            return false;
        }
        let start = Instant::now();
        let theme_hl = ThemeHighlighter::new(&hl.theme);
        while self.lines.len() < doc.lines.len() {
            let spans = self.highlight_line(hl, &theme_hl, &doc.lines[self.lines.len()]);
            self.lines.push(spans);
            if start.elapsed() >= budget {
                break;
            }
        }
        self.total = doc.lines.len();
        !self.done()
    }

    /// Highlights until the first `upto` lines are done or `budget` has
    /// passed, whichever comes first.
    pub fn advance_to(&mut self, hl: &Highlighter, doc: &TextDoc, upto: usize, budget: Duration) {
        if self.plain {
            return;
        }
        let start = Instant::now();
        let theme_hl = ThemeHighlighter::new(&hl.theme);
        let upto = upto.min(doc.lines.len());
        while self.lines.len() < upto && start.elapsed() < budget {
            let spans = self.highlight_line(hl, &theme_hl, &doc.lines[self.lines.len()]);
            self.lines.push(spans);
        }
    }

    fn highlight_line(
        &mut self,
        hl: &Highlighter,
        theme_hl: &ThemeHighlighter,
        text: &str,
    ) -> Vec<Span<'static>> {
        if text.len() > MAX_HL_LINE {
            return plain_spans(text);
        }
        let with_newline = format!("{text}\n");
        let Ok(ops) = self.parse.parse_line(&with_newline, &hl.syntaxes) else {
            // A parse failure leaves the state unusable for this line only.
            return plain_spans(text);
        };
        let mut spans = Vec::new();
        for (style, piece) in
            HighlightIterator::new(&mut self.highlight, &ops, &with_newline, theme_hl)
        {
            let piece = piece.strip_suffix('\n').unwrap_or(piece);
            if piece.is_empty() {
                continue;
            }
            spans.push(Span::styled(piece.to_string(), to_ratatui(style)));
        }
        if spans.is_empty() {
            return plain_spans(text);
        }
        spans
    }
}

fn to_ratatui(style: syntect::highlighting::Style) -> Style {
    let c = style.foreground;
    let mut out = Style::default().fg(Color::Rgb(c.r, c.g, c.b));
    if style.font_style.contains(FontStyle::BOLD) {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(name: &str, lines: &[&str]) -> TextDoc {
        TextDoc {
            lines: lines.iter().map(|s| s.to_string()).collect(),
            hint: SyntaxHint::Path(name.into()),
        }
    }

    #[test]
    fn picks_syntaxes() {
        let hl = Highlighter::new();
        assert_eq!(hl.syntax_name(&doc("flake.nix", &["{ }"])), "Nix");
        assert_eq!(hl.syntax_name(&doc("a.yaml", &["a: 1"])), "YAML");
        assert_eq!(
            hl.syntax_name(&doc("noext", &["#!/bin/bash"])),
            "Bourne Again Shell (bash)"
        );
        assert_eq!(hl.syntax_name(&doc("s.jsonl", &["{\"a\":1}"])), "JSON");
        assert_eq!(hl.syntax_name(&doc("notes", &["hello"])), "Plain Text");
        assert_eq!(
            hl.syntax_name(&TextDoc {
                lines: vec![],
                hint: SyntaxHint::Json
            }),
            "JSON"
        );
    }

    #[test]
    fn keyword_differs_from_plain_and_text_is_preserved() {
        let hl = Highlighter::new();
        let d = doc("main.rs", &["fn main() {}"]);
        let mut c = HlCache::new(&hl, &d, false);
        while c.advance(&hl, &d, Duration::from_secs(1)) {}
        let spans = c.line(0).unwrap();
        assert_ne!(spans[0].style.fg, Some(crate::theme::TEXT));
        assert_eq!(
            spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
            "fn main() {}"
        );
        assert!(spans.iter().all(|s| s.style.bg.is_none()));
    }

    #[test]
    fn zero_budget_advances_one_line() {
        let hl = Highlighter::new();
        let d = doc("a.rs", &["let a = 1;", "let b = 2;"]);
        let mut c = HlCache::new(&hl, &d, false);
        assert!(c.advance(&hl, &d, Duration::ZERO));
        assert!(c.line(0).is_some() && c.line(1).is_none());
    }

    #[test]
    fn very_long_line_is_not_parsed() {
        let hl = Highlighter::new();
        let long = format!("let s = \"{}\";", "a".repeat(1_000_000));
        let d = doc("a.rs", &[&long, "let b = 2;"]);
        let mut c = HlCache::new(&hl, &d, false);
        let start = Instant::now();
        c.advance(&hl, &d, Duration::from_secs(10));
        assert!(start.elapsed() < Duration::from_secs(1));
        let text: String = c
            .line(0)
            .unwrap()
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(text, long);
        assert_eq!(c.line(0).unwrap()[0].style.fg, Some(crate::theme::TEXT));
        assert!(c.line(1).is_some());
    }

    #[test]
    fn advance_to_stops_at_the_line_or_the_budget() {
        let hl = Highlighter::new();
        let d = doc("a.rs", &["let a = 1;"; 50]);
        let mut c = HlCache::new(&hl, &d, false);
        c.advance_to(&hl, &d, 20, Duration::from_secs(10));
        assert!(c.line(19).is_some() && c.line(20).is_none());
        // Past the end stops at the end; a zero budget does nothing.
        let mut z = HlCache::new(&hl, &d, false);
        z.advance_to(&hl, &d, 20, Duration::ZERO);
        assert!(z.line(0).is_none());
        c.advance_to(&hl, &d, 1000, Duration::from_secs(10));
        assert!(c.done() && c.line(49).is_some());
    }

    #[test]
    fn plain_mode() {
        let hl = Highlighter::new();
        let d = doc("a.rs", &["fn a() {}"]);
        let c = HlCache::new(&hl, &d, true);
        assert!(c.done() && c.line(0).is_none());
    }
}
