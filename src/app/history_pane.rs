//! Session history pane state: incremental reading, detail cache, navigation, and opening.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use ratatui::text::Line;
use serde_json::Value;

use crate::app::{Action, Viewport};
use crate::browser::{file_url, is_html};
use crate::detail::{Detail, detail, wrap_body};
use crate::highlight::Highlighter;
use crate::history::{History, Row};
use crate::nav::{self, HistoryView, Nav, NavKey};
use crate::row_text::Ctx;
use crate::tail::SessionRead;

struct Session {
    path: PathBuf,
    history: History,
    offset: u64,
    reading: bool,
    again: bool,
    missing: bool,
}

struct DetailCache {
    row: usize,
    revision: u64,
    detail: Detail,
    wrapped_width: usize,
    wrapped: Vec<Line<'static>>,
}

pub(super) struct HistoryPane {
    session: Option<Session>,
    generation: u64,
    detail: Option<DetailCache>,
    #[cfg(test)]
    detail_builds: usize,
}

impl HistoryPane {
    pub(super) fn new() -> Self {
        Self {
            session: None,
            generation: 0,
            detail: None,
            #[cfg(test)]
            detail_builds: 0,
        }
    }

    #[cfg(test)]
    pub(super) fn detail_builds(&self) -> usize {
        self.detail_builds
    }

    pub(super) fn watched(&self) -> Option<&Path> {
        self.session.as_ref().map(|s| s.path.as_path())
    }

    pub(super) fn set_session(&mut self, path: Option<PathBuf>) -> Option<Action> {
        if self.session.as_ref().map(|s| &s.path) == path.as_ref() {
            return None;
        }
        self.detail = None;
        self.generation += 1;
        match path {
            None => {
                self.session = None;
                None
            }
            Some(p) => {
                self.session = Some(Session {
                    path: p.clone(),
                    history: History::new(),
                    offset: 0,
                    reading: true,
                    again: false,
                    missing: false,
                });
                Some(Action::ReadSession {
                    path: p,
                    offset: 0,
                    generation: self.generation,
                })
            }
        }
    }

    pub(super) fn on_fs_change(&mut self, path: &Path) -> Option<Action> {
        let s = self.session.as_mut()?;
        if s.path != path {
            return None;
        }
        if s.reading {
            s.again = true;
            return None;
        }
        s.reading = true;
        self.generation += 1;
        Some(Action::ReadSession {
            path: s.path.clone(),
            offset: s.offset,
            generation: self.generation,
        })
    }

    pub(super) fn on_read(&mut self, r: SessionRead, nav: &mut Nav, vp: Viewport) -> Action {
        if r.generation != self.generation {
            return Action::None;
        }
        let Some(s) = &mut self.session else {
            return Action::None;
        };
        s.reading = false;
        match r.result {
            Err(_) => {
                s.missing = true;
                s.history = History::new();
                s.offset = 0;
                self.detail = None;
            }
            Ok(chunk) => {
                if chunk.reset {
                    s.history = History::new();
                    self.detail = None;
                }
                for rec in &chunk.records {
                    s.history.push(rec);
                }
                s.offset = chunk.end;
                s.missing = false;
                nav::settle(nav, s.history.rows.len(), vp.height as usize);
            }
        }
        if s.again {
            s.again = false;
            s.reading = true;
            self.generation += 1;
            Action::ReadSession {
                path: s.path.clone(),
                offset: s.offset,
                generation: self.generation,
            }
        } else {
            Action::None
        }
    }

    pub(super) fn apply_nav(
        &mut self,
        key: NavKey,
        nav: &mut Nav,
        vp: Viewport,
        ctx: &Ctx,
        hl: &Highlighter,
        path_var: Option<&OsStr>,
    ) {
        let Some(s) = &self.session else { return };
        if s.missing || s.history.rows.is_empty() {
            return;
        }
        let len = s.history.rows.len();
        let detail_rows = if nav.expanded {
            let selected = nav.selected.min(len - 1);
            let bw = body_width(vp.width, ctx.icon_width);
            self.ensure_detail(selected, bw, ctx, hl, path_var).len()
        } else {
            0
        };
        let s = self.session.as_ref().expect("session checked above");
        nav::apply(
            nav,
            key,
            len,
            |i| matches!(s.history.rows[i], Row::Prompt(_)),
            vp.height as usize,
            detail_rows,
        );
    }

    pub(super) fn open(&self, nav: &Nav, alt: bool) -> Action {
        let Some(s) = &self.session else {
            return Action::None;
        };
        if s.missing || s.history.rows.is_empty() {
            return Action::None;
        }
        let idx = nav.selected.min(s.history.rows.len() - 1);
        let Row::Tool(t) = &s.history.rows[idx] else {
            return Action::None;
        };
        match t.name.as_str() {
            "Read" | "Edit" | "Write" => {
                let Some(fp) = t
                    .input
                    .get("file_path")
                    .and_then(Value::as_str)
                    .filter(|p| !p.is_empty())
                else {
                    return Action::None;
                };
                let path = PathBuf::from(fp);
                if alt && is_html(&path) {
                    Action::Browse {
                        url: file_url(&path),
                    }
                } else {
                    Action::Open {
                        path,
                        kind: None,
                        alt,
                    }
                }
            }
            "WebFetch" => {
                if !alt {
                    return Action::None;
                }
                let Some(url) = t
                    .input
                    .get("url")
                    .and_then(Value::as_str)
                    .filter(|u| !u.is_empty())
                else {
                    return Action::None;
                };
                Action::Browse {
                    url: url.to_string(),
                }
            }
            "Artifact" => {
                if !alt {
                    return Action::None;
                }
                let Some(url) = t
                    .result
                    .as_ref()
                    .and_then(|r| r.extra.get("url"))
                    .and_then(Value::as_str)
                    .filter(|u| !u.is_empty())
                else {
                    return Action::None;
                };
                Action::Browse {
                    url: url.to_string(),
                }
            }
            _ => Action::None,
        }
    }

    pub(super) fn view<'a>(
        &'a mut self,
        nav: &'a mut Nav,
        vp: Viewport,
        ctx: &'a Ctx,
        hl: &Highlighter,
        path_var: Option<&OsStr>,
    ) -> Option<HistoryView<'a>> {
        let s = self.session.as_ref()?;
        if s.missing {
            return None;
        }
        let len = s.history.rows.len();
        if len > 0 {
            nav::settle(nav, len, vp.height as usize);
            if nav.expanded {
                let selected = nav.selected.min(len - 1);
                let bw = body_width(vp.width, ctx.icon_width);
                let detail_rows = self.ensure_detail(selected, bw, ctx, hl, path_var).len();
                let layout = nav::expanded_layout(len, selected, vp.height as usize, detail_rows);
                nav.detail_scroll = nav
                    .detail_scroll
                    .min(detail_rows.saturating_sub(layout.body_rows));
            }
        }
        let s = self.session.as_ref()?;
        let detail = if nav.expanded && !s.history.rows.is_empty() {
            self.detail
                .as_ref()
                .map(|c| (&c.detail.header, c.wrapped.as_slice()))
        } else {
            None
        };
        Some(HistoryView {
            history: &s.history,
            nav,
            ctx,
            detail,
        })
    }

    fn ensure_detail(
        &mut self,
        row: usize,
        width: usize,
        ctx: &Ctx,
        hl: &Highlighter,
        path_var: Option<&OsStr>,
    ) -> &[Line<'static>] {
        let s = self.session.as_ref().expect("session present");
        let rev = s.history.revision(row);
        let hit = self
            .detail
            .as_ref()
            .is_some_and(|c| c.row == row && c.revision == rev);
        if !hit {
            let d = detail(
                &s.history.rows[row],
                s.history.tool_counts(row),
                ctx,
                hl,
                path_var,
            );
            let wrapped = wrap_body(&d.body, width);
            self.detail = Some(DetailCache {
                row,
                revision: rev,
                detail: d,
                wrapped_width: width,
                wrapped,
            });
            #[cfg(test)]
            {
                self.detail_builds += 1;
            }
        } else if let Some(c) = &mut self.detail
            && c.wrapped_width != width
        {
            c.wrapped = wrap_body(&c.detail.body, width);
            c.wrapped_width = width;
        }
        &self.detail.as_ref().expect("detail populated").wrapped
    }
}

fn body_width(vp_width: u16, icon_width: u16) -> usize {
    let indent = 1 + 2 * (icon_width.max(1) + 1);
    (vp_width.saturating_sub(indent) as usize).max(1)
}
