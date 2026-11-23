//! Display state: the shown file, per-tab positions and keys.
//!
//! `App` never touches the terminal. It is driven by events and answers with
//! `Action`s for the main loop to carry out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use image::DynamicImage;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::text::Line;

use crate::highlight::{Highlighter, HlCache};
use crate::kind::FileKind;
use crate::loader::{Content, Loaded};
use crate::state::{Target, parse_state, resolve_target};
use crate::text_doc::TextDoc;
use crate::view::{DocRows, text_width};
use crate::wrap::{Rows, Scroll, clamp, scroll_bottom, scroll_down, scroll_up};

/// The body area (without the header line).
#[derive(Clone, Copy)]
pub struct Viewport {
    pub width: u16,
    pub height: u16,
}

pub enum Body {
    Text { doc: TextDoc, hl: HlCache },
    Table(Vec<Line<'static>>),
    Image(DynamicImage),
    Info { label: String, size: u64 },
    Missing,
}

pub struct Shown {
    pub path: PathBuf,
    pub kind: FileKind,
    pub body: Body,
    pub scroll: Scroll,
}

#[derive(Debug, PartialEq)]
pub enum Action {
    None,
    Load {
        path: PathBuf,
        generation: u64,
    },
    Open {
        path: PathBuf,
        kind: FileKind,
        alt: bool,
    },
    Quit,
}

/// What one tab remembers.
#[derive(Default)]
struct TabState {
    /// The file this tab showed last.
    #[allow(dead_code)] // recorded for the tab history of stage 2; nothing reads it yet
    file: Option<PathBuf>,
    /// The position of every file seen in this tab.
    scrolls: HashMap<PathBuf, Scroll>,
}

/// A load that has been requested and not yet arrived.
struct Pending {
    path: PathBuf,
    tab: Option<String>,
    /// Where to start once loaded; `None` for a reload (keep the position).
    pos: Option<Scroll>,
}

enum Move {
    Down(usize),
    Up(usize),
    Top,
    Bottom,
    Clamp,
}

fn run_move(r: &impl Rows, height: usize, s: Scroll, mv: Move) -> Scroll {
    match mv {
        Move::Down(n) => scroll_down(s, n, r, height),
        Move::Up(n) => scroll_up(s, n, r),
        Move::Top => Scroll::default(),
        Move::Bottom => scroll_bottom(r, height),
        Move::Clamp => clamp(s, r, height),
    }
}

/// Applies `mv` using the row counts of `body`. Bodies that do not scroll
/// keep the position they have.
fn move_scroll(body: &Body, vp: Viewport, s: Scroll, mv: Move) -> Scroll {
    let height = vp.height as usize;
    match body {
        Body::Text { doc, .. } => {
            let width = text_width(vp.width, doc.lines.len());
            run_move(&DocRows { doc, width }, height, s, mv)
        }
        Body::Table(lines) => run_move(&vec![1usize; lines.len()], height, s, mv),
        _ => s,
    }
}

pub struct App {
    hl: Highlighter,
    shown: Option<Shown>,
    /// The tab `shown` belongs to.
    shown_tab: Option<String>,
    /// The tab of the latest state file.
    tab: Option<String>,
    tabs: HashMap<Option<String>, TabState>,
    pending: Option<Pending>,
    generation: u64,
    message: Option<String>,
    /// The previous key was `g`.
    g_pressed: bool,
}

impl App {
    pub fn new(hl: Highlighter) -> Self {
        Self {
            hl,
            shown: None,
            shown_tab: None,
            tab: None,
            tabs: HashMap::new(),
            pending: None,
            generation: 0,
            message: None,
            g_pressed: false,
        }
    }

    pub fn shown(&self) -> Option<&Shown> {
        self.shown.as_ref()
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn set_message(&mut self, m: Option<String>) {
        self.message = m;
    }

    /// The file to watch: the one being loaded, else the one shown.
    pub fn watched_file(&self) -> Option<&Path> {
        match &self.pending {
            Some(p) => Some(&p.path),
            None => self.shown.as_ref().map(|s| s.path.as_path()),
        }
    }

    /// Writes the shown file and its position into the tab it belongs to.
    fn remember(&mut self) {
        let Some(s) = &self.shown else { return };
        let tab = self.tabs.entry(self.shown_tab.clone()).or_default();
        tab.file = Some(s.path.clone());
        tab.scrolls.insert(s.path.clone(), s.scroll);
    }

    fn request_load(&mut self, path: PathBuf, tab: Option<String>, pos: Option<Scroll>) -> Action {
        self.generation += 1;
        self.pending = Some(Pending {
            path: path.clone(),
            tab,
            pos,
        });
        Action::Load {
            path,
            generation: self.generation,
        }
    }

    pub fn on_state_bytes(&mut self, bytes: &[u8]) -> Action {
        let Some(state) = parse_state(bytes) else {
            return Action::None;
        };
        let tab_changed = state.tab != self.tab;
        if tab_changed {
            self.remember();
            self.tab = state.tab;
        }
        match resolve_target(state.file.as_deref()) {
            Target::Keep => Action::None,
            Target::Missing => {
                self.remember();
                self.shown = None;
                self.pending = None;
                // Drops a load that is still on its way.
                self.generation += 1;
                Action::None
            }
            Target::Show(p) => {
                let current = match &self.pending {
                    Some(pending) => Some(pending.path.as_path()),
                    None => self.shown.as_ref().map(|s| s.path.as_path()),
                };
                if !tab_changed && current == Some(p.as_path()) {
                    return Action::None;
                }
                self.remember();
                let pos = self
                    .tabs
                    .get(&self.tab)
                    .and_then(|t| t.scrolls.get(&p))
                    .copied()
                    .unwrap_or_default();
                let tab = self.tab.clone();
                self.request_load(p, tab, Some(pos))
            }
        }
    }

    /// Returns true when the display changed.
    pub fn on_loaded(&mut self, l: Loaded, vp: Viewport) -> bool {
        if l.generation != self.generation {
            return false;
        }
        let Some(pending) = self.pending.take() else {
            return false;
        };
        let start = pending.pos.unwrap_or_else(|| match &self.shown {
            Some(s) if s.path == l.path => s.scroll,
            _ => Scroll::default(),
        });
        let body = match l.content {
            Content::Text { doc, plain } => {
                let hl = HlCache::new(&self.hl, &doc, plain);
                Body::Text { doc, hl }
            }
            Content::Table(lines) => Body::Table(lines),
            Content::Image(img) => Body::Image(img),
            Content::Info { label, size } => Body::Info { label, size },
            Content::Missing => Body::Missing,
        };
        let scroll = move_scroll(&body, vp, start, Move::Clamp);
        self.shown = Some(Shown {
            path: l.path,
            kind: l.kind,
            body,
            scroll,
        });
        self.shown_tab = pending.tab;
        true
    }

    /// The file `path` changed on disk.
    pub fn on_fs_change(&mut self, path: &Path) -> Action {
        if let Some(p) = &self.pending {
            // A load is on its way; reload only that file, keeping where it
            // is meant to start.
            if p.path != path {
                return Action::None;
            }
            let (path, tab, pos) = (p.path.clone(), p.tab.clone(), p.pos);
            return self.request_load(path, tab, pos);
        }
        match &self.shown {
            Some(s) if s.path == path => {
                let (path, tab) = (s.path.clone(), self.shown_tab.clone());
                self.request_load(path, tab, None)
            }
            _ => Action::None,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent, vp: Viewport) -> Action {
        if key.kind != KeyEventKind::Press {
            return Action::None;
        }
        self.message = None;
        let g_before = std::mem::take(&mut self.g_pressed);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = ((vp.height / 2) as usize).max(1);
        if ctrl {
            return match key.code {
                KeyCode::Char('d') => self.scroll(vp, Move::Down(half)),
                KeyCode::Char('u') => self.scroll(vp, Move::Up(half)),
                KeyCode::Char('c') => Action::Quit,
                _ => Action::None,
            };
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return Action::None;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.scroll(vp, Move::Down(1)),
            KeyCode::Char('k') | KeyCode::Up => self.scroll(vp, Move::Up(1)),
            KeyCode::Char('g') => {
                if g_before {
                    self.scroll(vp, Move::Top)
                } else {
                    self.g_pressed = true;
                    Action::None
                }
            }
            KeyCode::Char('G') => self.scroll(vp, Move::Bottom),
            KeyCode::Char('o') | KeyCode::Enter => self.open(false),
            KeyCode::Char('O') => self.open(true),
            _ => Action::None,
        }
    }

    fn scroll(&mut self, vp: Viewport, mv: Move) -> Action {
        if let Some(s) = &mut self.shown {
            s.scroll = move_scroll(&s.body, vp, s.scroll, mv);
        }
        Action::None
    }

    fn open(&self, alt: bool) -> Action {
        match &self.shown {
            Some(s) => Action::Open {
                path: s.path.clone(),
                kind: s.kind,
                alt,
            },
            None => Action::None,
        }
    }

    /// Highlights a little more of the shown text. Returns true while lines
    /// remain.
    pub fn tick_highlight(&mut self, budget: Duration) -> bool {
        match &mut self.shown {
            Some(Shown {
                body: Body::Text { doc, hl },
                ..
            }) => hl.advance(&self.hl, doc, budget),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_doc::SyntaxHint;
    use ratatui::crossterm::event::KeyEventState;
    use tempfile::TempDir;

    const VP: Viewport = Viewport {
        width: 80,
        height: 10,
    };

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn text_loaded(path: &Path, generation: u64, n_lines: usize) -> Loaded {
        let doc = TextDoc {
            lines: (0..n_lines).map(|i| format!("line {i}")).collect(),
            hint: SyntaxHint::Path(path.to_path_buf()),
        };
        Loaded {
            generation,
            path: path.to_path_buf(),
            kind: FileKind::Text,
            content: Content::Text { doc, plain: true },
        }
    }

    fn state(tab: Option<&str>, file: &Path) -> Vec<u8> {
        match tab {
            Some(t) => serde_json::to_vec(&serde_json::json!({ "tab": t, "file": file })),
            None => serde_json::to_vec(&serde_json::json!({ "file": file })),
        }
        .unwrap()
    }

    fn files(n: usize) -> (TempDir, Vec<PathBuf>) {
        let d = tempfile::tempdir().unwrap();
        let paths = (0..n)
            .map(|i| {
                let p = d.path().join(format!("f{}.txt", i + 1));
                std::fs::write(&p, "x").unwrap();
                p
            })
            .collect();
        (d, paths)
    }

    fn new_app() -> App {
        App::new(Highlighter::new())
    }

    /// Sends the state, expects a `Load`, and answers it with `n` lines.
    fn show(app: &mut App, tab: Option<&str>, file: &Path, n: usize) {
        let Action::Load { path, generation } = app.on_state_bytes(&state(tab, file)) else {
            panic!("expected Load for {file:?}");
        };
        assert_eq!(path, file);
        assert!(app.on_loaded(text_loaded(&path, generation, n), VP));
    }

    fn line(app: &App) -> usize {
        app.shown().unwrap().scroll.line
    }

    fn press(app: &mut App, keys: &[char]) {
        for &c in keys {
            app.on_key(key(c), VP);
        }
    }

    #[test]
    fn broken_state_changes_nothing() {
        let mut a = new_app();
        assert_eq!(a.on_state_bytes(b"{\"fi"), Action::None);
        assert!(a.shown().is_none());
        assert!(a.watched_file().is_none());
    }

    #[test]
    fn load_then_scroll_and_gg() {
        let (_d, f) = files(1);
        let mut a = new_app();
        show(&mut a, None, &f[0], 100);
        assert_eq!(line(&a), 0);
        press(&mut a, &['j', 'j', 'j']);
        assert_eq!(line(&a), 3);
        // g followed by another key is cancelled and the key counts as usual.
        press(&mut a, &['g', 'j']);
        assert_eq!(line(&a), 4);
        press(&mut a, &['g', 'g']);
        assert_eq!(line(&a), 0);
        press(&mut a, &['G']);
        assert_eq!(line(&a), 90);
        // ctrl-u / ctrl-d move half the height (5), arrows move one line.
        a.on_key(ctrl('u'), VP);
        assert_eq!(line(&a), 85);
        a.on_key(ctrl('d'), VP);
        assert_eq!(line(&a), 90);
        a.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), VP);
        assert_eq!(line(&a), 89);
        a.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), VP);
        assert_eq!(line(&a), 90);
        // Key releases are ignored.
        let release = KeyEvent {
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
            ..key('k')
        };
        a.on_key(release, VP);
        assert_eq!(line(&a), 90);
    }

    #[test]
    fn q_does_not_quit_and_ctrl_c_does() {
        let mut a = new_app();
        a.set_message(Some("hello".into()));
        assert_eq!(a.on_key(key('q'), VP), Action::None);
        assert_eq!(a.message(), None);
        assert_eq!(a.on_key(ctrl('c'), VP), Action::Quit);
    }

    #[test]
    fn tab_memory() {
        let (_d, f) = files(2);
        let mut a = new_app();
        show(&mut a, Some("1"), &f[0], 100);
        a.on_key(ctrl('d'), VP);
        press(&mut a, &['j'; 15]);
        assert_eq!(line(&a), 20);
        show(&mut a, Some("2"), &f[1], 100);
        assert_eq!(line(&a), 0);
        show(&mut a, Some("1"), &f[0], 100);
        assert_eq!(line(&a), 20);
    }

    #[test]
    fn same_tab_new_file_starts_at_top() {
        let (_d, f) = files(2);
        let mut a = new_app();
        show(&mut a, Some("1"), &f[0], 100);
        press(&mut a, &['j'; 20]);
        assert_eq!(line(&a), 20);
        show(&mut a, Some("1"), &f[1], 100);
        assert_eq!(line(&a), 0);
    }

    #[test]
    fn same_tab_returning_file_restores_position() {
        let (_d, f) = files(2);
        let mut a = new_app();
        show(&mut a, Some("1"), &f[0], 100);
        press(&mut a, &['j'; 20]);
        show(&mut a, Some("1"), &f[1], 100);
        press(&mut a, &['j'; 5]);
        assert_eq!(line(&a), 5);
        show(&mut a, Some("1"), &f[0], 100);
        assert_eq!(line(&a), 20);
        show(&mut a, Some("1"), &f[1], 100);
        assert_eq!(line(&a), 5);
    }

    #[test]
    fn positions_are_per_tab() {
        let (_d, f) = files(1);
        let mut a = new_app();
        show(&mut a, Some("1"), &f[0], 100);
        press(&mut a, &['j'; 20]);
        assert_eq!(line(&a), 20);
        // The same file in another tab starts at the top.
        show(&mut a, Some("2"), &f[0], 100);
        assert_eq!(line(&a), 0);
        // Same tab, same file: nothing to load.
        assert_eq!(a.on_state_bytes(&state(Some("2"), &f[0])), Action::None);
    }

    #[test]
    fn stale_generation_is_dropped() {
        let (_d, f) = files(2);
        let mut a = new_app();
        let Action::Load { generation: g1, .. } = a.on_state_bytes(&state(Some("1"), &f[0])) else {
            panic!("expected Load");
        };
        let Action::Load { generation: g2, .. } = a.on_state_bytes(&state(Some("1"), &f[1])) else {
            panic!("expected Load");
        };
        assert_eq!(g2, g1 + 1);
        assert_eq!(a.watched_file(), Some(f[1].as_path()));
        assert!(!a.on_loaded(text_loaded(&f[0], g1, 10), VP));
        assert!(a.shown().is_none());
        assert!(a.on_loaded(text_loaded(&f[1], g2, 10), VP));
        assert_eq!(a.shown().unwrap().path, f[1]);
    }

    #[test]
    fn reload_keeps_and_clamps_scroll() {
        let (_d, f) = files(1);
        let mut a = new_app();
        show(&mut a, Some("1"), &f[0], 100);
        press(&mut a, &['j'; 50]);
        assert_eq!(line(&a), 50);
        let Action::Load { path, generation } = a.on_fs_change(&f[0]) else {
            panic!("expected Load");
        };
        assert_eq!(path, f[0]);
        // The old display stays while loading.
        assert_eq!(line(&a), 50);
        assert!(a.on_loaded(text_loaded(&path, generation, 20), VP));
        assert_eq!(line(&a), 10);

        let (_d, f) = files(1);
        let mut a = new_app();
        show(&mut a, Some("1"), &f[0], 100);
        press(&mut a, &['j'; 50]);
        let Action::Load { path, generation } = a.on_fs_change(&f[0]) else {
            panic!("expected Load");
        };
        assert!(a.on_loaded(text_loaded(&path, generation, 100), VP));
        assert_eq!(line(&a), 50);
    }

    #[test]
    fn vanish_and_return() {
        let (_d, f) = files(1);
        let mut a = new_app();
        show(&mut a, Some("1"), &f[0], 10);
        let Action::Load { generation, .. } = a.on_fs_change(&f[0]) else {
            panic!("expected Load");
        };
        let missing = Loaded {
            generation,
            path: f[0].clone(),
            kind: FileKind::Other,
            content: Content::Missing,
        };
        assert!(a.on_loaded(missing, VP));
        assert!(matches!(a.shown().unwrap().body, Body::Missing));
        assert_eq!(a.watched_file(), Some(f[0].as_path()));
        // It comes back: a change event loads it again.
        let Action::Load { path, generation } = a.on_fs_change(&f[0]) else {
            panic!("expected Load");
        };
        assert!(a.on_loaded(text_loaded(&path, generation, 10), VP));
        assert!(matches!(a.shown().unwrap().body, Body::Text { .. }));
        // Other paths are none of its business.
        assert_eq!(a.on_fs_change(Path::new("/other")), Action::None);
    }

    #[test]
    fn missing_file_in_state_shows_nothing() {
        let (_d, f) = files(1);
        let mut a = new_app();
        show(&mut a, Some("1"), &f[0], 10);
        let bytes = serde_json::to_vec(&serde_json::json!({"file": "/no/such"})).unwrap();
        assert_eq!(a.on_state_bytes(&bytes), Action::None);
        assert!(a.shown().is_none());
        assert!(a.watched_file().is_none());
    }

    #[test]
    fn dir_without_readme_keeps_current() {
        let (_d, f) = files(1);
        let empty = tempfile::tempdir().unwrap();
        let mut a = new_app();
        show(&mut a, Some("1"), &f[0], 10);
        assert_eq!(
            a.on_state_bytes(&state(Some("1"), empty.path())),
            Action::None
        );
        assert_eq!(a.shown().unwrap().path, f[0]);
    }

    #[test]
    fn open_actions() {
        let (_d, f) = files(1);
        let mut a = new_app();
        assert_eq!(a.on_key(key('o'), VP), Action::None);
        show(&mut a, Some("1"), &f[0], 10);
        let open = |alt| Action::Open {
            path: f[0].clone(),
            kind: FileKind::Text,
            alt,
        };
        assert_eq!(a.on_key(key('o'), VP), open(false));
        assert_eq!(
            a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), VP),
            open(false)
        );
        assert_eq!(a.on_key(key('O'), VP), open(true));
        assert_eq!(a.on_key(key('I'), VP), Action::None);
    }

    #[test]
    fn zero_sized_viewport_does_not_panic() {
        let (_d, f) = files(1);
        let zero = Viewport {
            width: 0,
            height: 0,
        };
        let mut a = new_app();
        let Action::Load { path, generation } = a.on_state_bytes(&state(Some("1"), &f[0])) else {
            panic!("expected Load");
        };
        assert!(a.on_loaded(text_loaded(&path, generation, 100), zero));
        for k in [key('j'), ctrl('d'), key('G'), key('g'), key('g')] {
            a.on_key(k, zero);
        }
        a.on_key(key('G'), VP);
        assert_eq!(line(&a), 90);
    }
}
