//! Selection, follow mode, expanded layout, and rendering for session history.

use std::ops::Range;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Widget;

use crate::history::{History, Row};
use crate::row_text::{Ctx, RowText, render_compact, render_row, row_text};
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nav {
    pub selected: usize,
    pub follow: bool,
    pub expanded: bool,
    pub top: usize,
    pub detail_scroll: usize,
}

impl Default for Nav {
    fn default() -> Self {
        Self {
            selected: 0,
            follow: true,
            expanded: false,
            top: 0,
            detail_scroll: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKey {
    Down,
    Up,
    Expand,
    Collapse,
    NextPrompt,
    PrevPrompt,
    HalfDown,
    HalfUp,
    Top,
    Bottom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expanded {
    pub above: Option<usize>,
    pub body_rows: usize,
    pub below: Range<usize>,
}

/// Adjusts selection and scroll offset to fit `len` rows in `height` terminal lines.
/// When `follow` is active (and not expanded), selects the last row.
pub fn settle(nav: &mut Nav, len: usize, height: usize) {
    if len == 0 {
        nav.selected = 0;
        nav.top = 0;
        nav.detail_scroll = 0;
        return;
    }

    if nav.follow && !nav.expanded {
        nav.selected = len - 1;
    } else {
        nav.selected = nav.selected.min(len - 1);
    }

    if !nav.expanded {
        if height == 0 {
            nav.top = nav.selected;
        } else {
            if nav.selected < nav.top {
                nav.top = nav.selected;
            }
            if nav.selected >= nav.top + height {
                nav.top = nav.selected + 1 - height;
            }
            nav.top = nav.top.min(len.saturating_sub(height));
        }
    }
}

/// Applies a navigation key to `nav`, updating `follow` and settling the viewport.
pub fn apply(
    nav: &mut Nav,
    key: NavKey,
    len: usize,
    is_prompt: impl Fn(usize) -> bool,
    height: usize,
    detail_rows: usize,
) {
    if len == 0 {
        settle(nav, 0, height);
        return;
    }

    nav.selected = nav.selected.min(len - 1);

    match key {
        NavKey::Down => {
            if nav.selected + 1 < len {
                nav.selected += 1;
            }
            nav.detail_scroll = 0;
        }
        NavKey::Up => {
            nav.selected = nav.selected.saturating_sub(1);
            nav.detail_scroll = 0;
        }
        NavKey::Expand => {
            if !nav.expanded {
                nav.expanded = true;
                nav.detail_scroll = 0;
            }
        }
        NavKey::Collapse => {
            if nav.expanded {
                nav.expanded = false;
                nav.detail_scroll = 0;
            }
        }
        NavKey::NextPrompt => {
            if let Some(next) = ((nav.selected + 1)..len).find(|&i| is_prompt(i)) {
                nav.selected = next;
                nav.detail_scroll = 0;
            }
        }
        NavKey::PrevPrompt => {
            if let Some(prev) = (0..nav.selected).rev().find(|&i| is_prompt(i)) {
                nav.selected = prev;
                nav.detail_scroll = 0;
            }
        }
        NavKey::HalfDown => {
            if nav.expanded {
                let layout = expanded_layout(len, nav.selected, height, detail_rows);
                let step = (layout.body_rows / 2).max(1);
                let max_scroll = detail_rows.saturating_sub(layout.body_rows);
                nav.detail_scroll = (nav.detail_scroll + step).min(max_scroll);
            } else {
                let step = (height / 2).max(1);
                nav.selected = (nav.selected + step).min(len - 1);
            }
        }
        NavKey::HalfUp => {
            if nav.expanded {
                let layout = expanded_layout(len, nav.selected, height, detail_rows);
                let step = (layout.body_rows / 2).max(1);
                nav.detail_scroll = nav.detail_scroll.saturating_sub(step);
            } else {
                let step = (height / 2).max(1);
                nav.selected = nav.selected.saturating_sub(step);
            }
        }
        NavKey::Top => {
            nav.selected = 0;
            nav.detail_scroll = 0;
        }
        NavKey::Bottom => {
            nav.selected = len - 1;
            nav.detail_scroll = 0;
        }
    }

    nav.follow = !nav.expanded && nav.selected + 1 == len;
    settle(nav, len, height);
}

/// Computes row allocation for the expanded view: optional above row, top separator,
/// header, body, bottom separator, and trailing below rows (filling leftover height).
pub fn expanded_layout(len: usize, selected: usize, height: usize, detail_rows: usize) -> Expanded {
    if len == 0 || selected >= len || height == 0 {
        return Expanded {
            above: None,
            body_rows: 0,
            below: 0..0,
        };
    }

    let above = (selected > 0).then(|| selected - 1);
    let has_below = selected + 1 < len;
    // Fixed rows: optional above (1) + top sep (1) + header (1) + bottom sep (1) + at least 1 below (if any).
    let fixed = usize::from(above.is_some()) + 3 + usize::from(has_below);
    let body_rows = detail_rows.min(height.saturating_sub(fixed));
    let leftover = height.saturating_sub(fixed + body_rows);

    let below = if has_below {
        let count = (1 + leftover).min(len - (selected + 1));
        (selected + 1)..(selected + 1 + count)
    } else {
        len..len
    };

    Expanded {
        above,
        body_rows,
        below,
    }
}

pub struct HistoryView<'a> {
    pub history: &'a History,
    pub nav: &'a Nav,
    pub ctx: &'a Ctx,
    pub detail: Option<(&'a RowText, &'a [Line<'static>])>,
}

impl Widget for HistoryView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() || self.history.rows.is_empty() {
            return;
        }

        let len = self.history.rows.len();

        if !self.nav.expanded {
            let start = self.nav.top.min(len);
            for (y, i) in (area.y..area.bottom()).zip(start..len) {
                render_list_row(buf, area.x, y, area.width, &self.history.rows[i], self.ctx);
                if i == self.nav.selected {
                    buf.set_style(
                        Rect::new(area.x, y, area.width, 1),
                        Style::default().bg(theme::SURFACE1),
                    );
                }
            }
            return;
        }

        let selected = self.nav.selected.min(len - 1);
        let fallback_header;
        let (header, body_lines): (&RowText, &[Line<'static>]) = match self.detail {
            Some((h, b)) => (h, b),
            None => {
                fallback_header = row_text(&self.history.rows[selected], self.ctx);
                (&fallback_header, &[])
            }
        };

        let layout = expanded_layout(len, selected, area.height as usize, body_lines.len());
        let mut y = area.y;

        if let Some(above) = layout.above
            && y < area.bottom()
        {
            render_list_row(
                buf,
                area.x,
                y,
                area.width,
                &self.history.rows[above],
                self.ctx,
            );
            y += 1;
        }

        if y < area.bottom() {
            draw_separator(buf, area.x, y, area.width);
            y += 1;
        }

        if y < area.bottom() {
            render_row(buf, area.x, y, area.width, header, self.ctx.icon_width);
            y += 1;
        }

        let indent = 1 + 2 * (self.ctx.icon_width.max(1) + 1);
        let body_x = area.x.saturating_add(indent);
        let body_w = area.right().saturating_sub(body_x);
        for line in body_lines
            .iter()
            .skip(self.nav.detail_scroll)
            .take(layout.body_rows)
        {
            if y >= area.bottom() {
                break;
            }
            if body_w > 0 {
                buf.set_line(body_x, y, line, body_w);
            }
            y += 1;
        }

        if y < area.bottom() {
            draw_separator(buf, area.x, y, area.width);
            y += 1;
        }

        for below in layout.below {
            if y >= area.bottom() {
                break;
            }
            render_list_row(
                buf,
                area.x,
                y,
                area.width,
                &self.history.rows[below],
                self.ctx,
            );
            y += 1;
        }
    }
}

fn render_list_row(buf: &mut Buffer, x: u16, y: u16, width: u16, row: &Row, ctx: &Ctx) {
    match row {
        Row::Compact(c) => render_compact(buf, x, y, width, c, ctx.icon_width),
        other => {
            let rt = row_text(other, ctx);
            render_row(buf, x, y, width, &rt, ctx.icon_width);
        }
    }
}

fn draw_separator(buf: &mut Buffer, x: u16, y: u16, width: u16) {
    let sep = "─".repeat(width as usize);
    buf.set_stringn(
        x,
        y,
        &sep,
        width as usize,
        Style::default().fg(theme::OVERLAY0),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{CompactRow, Prompt, PromptRow, Row, Status, ToolRow};
    use serde_json::json;

    fn make_history(n: usize) -> History {
        let mut h = History::new();
        for i in 0..n {
            h.rows.push(Row::Tool(ToolRow {
                id: format!("id_{i}"),
                name: "Bash".to_string(),
                input: json!({ "command": format!("echo {i}") }),
                cwd: None,
                result: None,
                status: Status::Ok,
                agent_id: None,
                usage: None,
                report: None,
                hooks: Vec::new(),
                diagnostics: Vec::new(),
            }));
        }
        h
    }

    #[test]
    fn follows_the_bottom_until_moved() {
        let mut nav = Nav::default();
        settle(&mut nav, 5, 3);
        assert_eq!(nav.selected, 4);
        assert_eq!(nav.top, 2);
        assert!(nav.follow);

        apply(&mut nav, NavKey::Up, 5, |_| false, 3, 0);
        assert_eq!(nav.selected, 3);
        assert!(!nav.follow);

        settle(&mut nav, 6, 3);
        assert_eq!(nav.selected, 3);

        apply(&mut nav, NavKey::Bottom, 6, |_| false, 3, 0);
        assert_eq!(nav.selected, 5);
        assert_eq!(nav.top, 3);
        assert!(nav.follow);
    }

    #[test]
    fn expanded_does_not_follow() {
        let mut nav = Nav::default();
        settle(&mut nav, 5, 10);
        assert_eq!(nav.selected, 4);
        assert!(nav.follow);

        apply(&mut nav, NavKey::Expand, 5, |_| false, 10, 20);
        assert!(nav.expanded);
        assert!(!nav.follow);

        settle(&mut nav, 6, 10);
        assert_eq!(nav.selected, 4);
    }

    #[test]
    fn prompt_jumps_skip_compact_rows() {
        let prompts = [true, false, false, true];
        let is_prompt = |i: usize| prompts[i];
        let mut nav = Nav {
            selected: 0,
            follow: false,
            expanded: false,
            top: 0,
            detail_scroll: 0,
        };

        apply(&mut nav, NavKey::NextPrompt, 4, is_prompt, 10, 0);
        assert_eq!(nav.selected, 3);

        apply(&mut nav, NavKey::NextPrompt, 4, is_prompt, 10, 0);
        assert_eq!(nav.selected, 3);

        apply(&mut nav, NavKey::PrevPrompt, 4, is_prompt, 10, 0);
        assert_eq!(nav.selected, 0);

        apply(&mut nav, NavKey::PrevPrompt, 4, is_prompt, 10, 0);
        assert_eq!(nav.selected, 0);
    }

    #[test]
    fn half_page_scrolls_the_body_when_expanded() {
        let mut nav = Nav {
            selected: 5,
            follow: false,
            expanded: true,
            top: 0,
            detail_scroll: 0,
        };
        let layout = expanded_layout(10, 5, 20, 100);
        let max_scroll = 100 - layout.body_rows;

        apply(&mut nav, NavKey::HalfDown, 10, |_| false, 20, 100);
        assert!(nav.detail_scroll > 0);
        assert!(nav.detail_scroll <= max_scroll);

        for _ in 0..50 {
            apply(&mut nav, NavKey::HalfDown, 10, |_| false, 20, 100);
        }
        assert_eq!(nav.detail_scroll, max_scroll);

        apply(&mut nav, NavKey::HalfUp, 10, |_| false, 20, 100);
        assert!(nav.detail_scroll < max_scroll);
    }

    #[test]
    fn layout_edges() {
        let top = expanded_layout(10, 0, 20, 3);
        assert_eq!(top.above, None);
        assert_eq!(top.body_rows, 3);
        assert_eq!(top.below.start, 1);
        assert!(top.below.len() > 1);

        let bottom = expanded_layout(10, 9, 20, 3);
        assert_eq!(bottom.above, Some(8));
        assert_eq!(bottom.body_rows, 3);
        assert!(bottom.below.is_empty());

        let mid = expanded_layout(10, 5, 20, 100);
        assert_eq!(mid.above, Some(4));
        assert_eq!(mid.body_rows, 20 - 5);
        assert_eq!(mid.below, 6..7);
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let mut h = make_history(3);
        h.rows.push(Row::Prompt(PromptRow {
            prompt: Prompt::Text("hello".to_string()),
            duration_ms: None,
        }));
        h.rows.push(Row::Compact(CompactRow {
            manual: true,
            pre_tokens: Some(1000),
            post_tokens: Some(500),
            summary: None,
        }));
        let ctx = Ctx {
            home: None,
            icon_width: 1,
        };

        let empty = History::new();
        let mut nav0 = Nav::default();
        settle(&mut nav0, 0, 0);
        apply(&mut nav0, NavKey::Down, 0, |_| false, 0, 0);
        apply(&mut nav0, NavKey::Bottom, 0, |_| false, 0, 0);

        for height in 0..=3 {
            for width in 0..=6 {
                let area = Rect::new(0, 0, width, height);
                let mut buf = Buffer::empty(area);
                HistoryView {
                    history: &empty,
                    nav: &nav0,
                    ctx: &ctx,
                    detail: None,
                }
                .render(area, &mut buf);

                for expanded in [false, true] {
                    let nav = Nav {
                        selected: 2,
                        follow: false,
                        expanded,
                        top: 0,
                        detail_scroll: 0,
                    };
                    let mut buf = Buffer::empty(area);
                    let rt = row_text(&h.rows[2], &ctx);
                    let body = vec![Line::from("body line 1"), Line::from("body line 2")];
                    HistoryView {
                        history: &h,
                        nav: &nav,
                        ctx: &ctx,
                        detail: Some((&rt, &body)),
                    }
                    .render(area, &mut buf);
                }
            }
        }
    }

    #[test]
    fn renders_list_selection_bg_and_expanded_body_indent() {
        let h = make_history(4);
        let ctx = Ctx {
            home: None,
            icon_width: 1,
        };
        let area = Rect::new(0, 0, 40, 8);

        // List view: selected row gets SURFACE1 background, others keep default Reset.
        let nav_list = Nav {
            selected: 1,
            follow: false,
            expanded: false,
            top: 0,
            detail_scroll: 0,
        };
        let mut buf = Buffer::empty(area);
        HistoryView {
            history: &h,
            nav: &nav_list,
            ctx: &ctx,
            detail: None,
        }
        .render(area, &mut buf);
        assert_eq!(buf[(0, 1)].bg, theme::SURFACE1);
        assert_eq!(buf[(39, 1)].bg, theme::SURFACE1);
        assert_ne!(buf[(0, 0)].bg, theme::SURFACE1);

        // Expanded view: separator uses OVERLAY0, body is indented by 1 + 2*(1+1) = 5,
        // and starts from detail_scroll.
        let nav_exp = Nav {
            selected: 1,
            follow: false,
            expanded: true,
            top: 0,
            detail_scroll: 1,
        };
        let rt = row_text(&h.rows[1], &ctx);
        let body = vec![Line::from("first"), Line::from("second")];
        let mut buf = Buffer::empty(area);
        HistoryView {
            history: &h,
            nav: &nav_exp,
            ctx: &ctx,
            detail: Some((&rt, &body)),
        }
        .render(area, &mut buf);

        // y=0: above row (0)
        // y=1: top separator
        assert_eq!(buf[(0, 1)].symbol(), "─");
        assert_eq!(buf[(0, 1)].fg, theme::OVERLAY0);
        // y=2: header
        // y=3: body starting at x=5, scrolled to "second"
        assert_eq!(buf[(5, 3)].symbol(), "s");
        assert_eq!(buf[(6, 3)].symbol(), "e");
    }
}
