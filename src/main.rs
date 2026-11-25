use std::io::{self, Read, Stdout, stdout};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use ratatui::backend::{Backend, ClearType, CrosstermBackend};
use ratatui::crossterm::event::{self, DisableFocusChange, EnableFocusChange, Event};
use ratatui::crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::crossterm::{cursor, execute};
// Used by the header line, which is off (issue #1).
// use ratatui::layout::{Constraint, Layout};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use ratatui_image::StatefulImage;
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;

use wtr::app::{Action, App, Body, Pane, Shown, Viewport};
use wtr::browser::{self, BrowserCmd};
use wtr::cli::{Command, Source, View, parse_args};
use wtr::event::AppEvent;
use wtr::external::build_command;
use wtr::highlight::Highlighter;
use wtr::icons;
use wtr::kind::{FileKind, SNIFF_LEN, classify};
use wtr::loader::spawn_load;
use wtr::row_text::Ctx;
use wtr::tail::spawn_read;
use wtr::theme;
// `header_line` is not used while the header line is off (issue #1).
// use wtr::view::header_line;
use wtr::view::{LinesView, TextView, info_lines, message_line, no_file_line};
use wtr::watch::{FsWatch, normalize};

type Term = Terminal<CrosstermBackend<Stdout>>;

const INPUT_POLL: Duration = Duration::from_millis(20);
/// Input is thrown away after a terminal query until this long passes quietly.
const DRAIN_QUIET: Duration = Duration::from_millis(50);
const IDLE_WAIT: Duration = Duration::from_millis(250);
const HL_BUDGET: Duration = Duration::from_millis(8);

fn main() -> anyhow::Result<()> {
    let view = match parse_args(std::env::args_os()) {
        Ok(Command::View(v)) => v,
        Ok(Command::ScanRecords(paths)) => {
            let roots = if paths.is_empty() {
                std::env::var_os("HOME")
                    .map(|h| vec![PathBuf::from(h).join(".claude/projects")])
                    .unwrap_or_default()
            } else {
                paths
            };
            let files = wtr::scan::jsonl_files(&roots);
            println!("{}", wtr::scan::scan(&files));
            return Ok(());
        }
        Err(e) => e.exit(),
    };
    let (tx, rx) = mpsc::channel();
    let watch = FsWatch::new(tx.clone())?;
    install_panic_hook();
    enter()?;
    let result = Viewer::start(view, watch, tx).and_then(|mut v| v.run(&rx));
    leave()?;
    Ok(result?)
}

fn enter() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    execute!(
        stdout(),
        EnterAlternateScreen,
        EnableFocusChange,
        cursor::Hide
    )
}

fn leave() -> io::Result<()> {
    let screen = execute!(
        stdout(),
        cursor::Show,
        DisableFocusChange,
        LeaveAlternateScreen
    );
    terminal::disable_raw_mode()?;
    screen
}

/// Only a panic on the main thread restores the terminal and is printed:
/// the default message from another thread would land on the alternate screen.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().name() == Some("main") {
            let _ = leave();
            original(info);
        }
    }));
}

/// Picks the image protocol and asks for the cell size. Reads stdin, so the
/// input thread must not be polling.
fn make_picker() -> Picker {
    let ghostty = std::env::var("TERM_PROGRAM").is_ok_and(|v| v == "ghostty");
    match std::env::var("WTR_IMAGE_PROTOCOL").as_deref() {
        Ok("halfblocks") => Picker::halfblocks(),
        // The override wins even when the cell-size query failed.
        Ok("kitty") => kitty(cell_size_picker().unwrap_or_else(Picker::halfblocks)),
        _ if ghostty => cell_size_picker()
            .map(kitty)
            .unwrap_or_else(Picker::halfblocks),
        _ => {
            let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
            // Only this full query can be answered late (Kitty, DA1); the
            // cell-size query is answered at once and leaves nothing behind.
            drain_input();
            picker
        }
    }
}

/// Asks only for the cell size. The Kitty query is not sent: herdr answers it
/// late and the answer leaks into the input.
fn cell_size_picker() -> Option<Picker> {
    let options = QueryStdioOptions {
        blacklist_protocols: vec![ProtocolType::Kitty, ProtocolType::Sixel],
        ..Default::default()
    };
    Picker::from_query_stdio_with_options(options).ok()
}

fn kitty(mut picker: Picker) -> Picker {
    picker.set_protocol_type(ProtocolType::Kitty);
    picker
}

/// Late answers to a terminal query must not be read as keys.
fn drain_input() {
    while event::poll(DRAIN_QUIET).unwrap_or(false) {
        if event::read().is_err() {
            break;
        }
    }
}

#[derive(Default)]
struct Gate {
    paused: bool,
    /// The input thread is outside `poll` and waiting.
    idle: bool,
}

/// Reads terminal input on its own thread; can be paused while an external
/// tool or a terminal query owns stdin.
struct Input {
    gate: Arc<(Mutex<Gate>, Condvar)>,
}

fn lock(m: &Mutex<Gate>) -> MutexGuard<'_, Gate> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Input {
    fn start(tx: Sender<AppEvent>) -> io::Result<Self> {
        let gate = Arc::new((Mutex::new(Gate::default()), Condvar::new()));
        let shared = gate.clone();
        std::thread::Builder::new()
            .name("input".into())
            .spawn(move || input_loop(&shared, &tx))?;
        Ok(Self { gate })
    }

    /// Returns once the thread has stopped outside `poll`.
    fn pause(&self) {
        let (m, cv) = &*self.gate;
        let mut g = lock(m);
        g.paused = true;
        while !g.idle {
            g = cv.wait(g).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn resume(&self) {
        let (m, cv) = &*self.gate;
        lock(m).paused = false;
        cv.notify_all();
    }
}

fn input_loop(gate: &(Mutex<Gate>, Condvar), tx: &Sender<AppEvent>) {
    let (m, cv) = gate;
    loop {
        {
            let mut g = lock(m);
            while g.paused {
                g.idle = true;
                cv.notify_all();
                g = cv.wait(g).unwrap_or_else(PoisonError::into_inner);
            }
            g.idle = false;
        }
        let ev = match event::poll(INPUT_POLL) {
            Ok(true) => event::read(),
            Ok(false) => continue,
            Err(e) => Err(e),
        };
        let Ok(ev) = ev else { break };
        if tx.send(AppEvent::Input(ev)).is_err() {
            break;
        }
    }
    // A stopped thread must never keep `pause` waiting.
    lock(m).idle = true;
    cv.notify_all();
}

/// State-file bytes with `file` and `session` made absolute-normalized, so the
/// paths the App holds match the ones file events arrive with. Bytes that do
/// not parse are passed on unchanged (the App keeps the previous display for
/// them).
fn normalize_state(bytes: &[u8]) -> Vec<u8> {
    let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return bytes.to_vec();
    };
    for key in ["file", "session"] {
        if let Some(serde_json::Value::String(p)) = v.get_mut(key) {
            *p = normalize(Path::new(p)).to_string_lossy().into_owned();
        }
    }
    serde_json::to_vec(&v).unwrap_or_else(|_| bytes.to_vec())
}

/// The state-file bytes that show fixed file and/or session paths.
fn file_state(file: Option<&Path>, session: Option<&Path>) -> Vec<u8> {
    let mut map = serde_json::Map::new();
    if let Some(f) = file {
        map.insert("file".into(), serde_json::json!(f.to_string_lossy()));
    }
    if let Some(s) = session {
        map.insert("session".into(), serde_json::json!(s.to_string_lossy()));
    }
    serde_json::Value::Object(map).to_string().into_bytes()
}

/// A changed path worth any work: the state file, the watched file, or the watched session.
fn is_relevant(
    p: &Path,
    state: Option<&Path>,
    watched_file: Option<&Path>,
    watched_session: Option<&Path>,
) -> bool {
    Some(p) == state || Some(p) == watched_file || Some(p) == watched_session
}

fn no_session_line() -> Line<'static> {
    Line::styled(
        format!("{} no session", icons::NO_SESSION),
        Style::default().fg(theme::OVERLAY1),
    )
}

fn sniff_kind(path: &Path) -> FileKind {
    let Ok(meta) = std::fs::metadata(path) else {
        return FileKind::Other;
    };
    if !meta.is_file() {
        return FileKind::Other;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return FileKind::Other;
    };
    let mut head = Vec::new();
    if file.take(SNIFF_LEN as u64).read_to_end(&mut head).is_err() {
        return FileKind::Other;
    }
    classify(path, &head)
}

fn render(f: &mut Frame, app: &mut App, image: &mut Option<StatefulProtocol>) {
    // The header line (kind mark and file name) is not drawn: wtr only shows
    // the preview (issue #1). It may come back depending on the tree's design.
    // let [head, body] =
    //     Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(f.area());
    // f.render_widget(
    //     header_line(
    //         shown.map(|s| s.path.as_path()),
    //         shown.map(|s| s.kind),
    //         app.message(),
    //         head.width,
    //     ),
    //     head,
    // );
    let body = f.area();
    render_body(f, app, image, body);
    // A message (e.g. `fx not found`) goes over the right end of the first
    // row, covering only its own cells, until the next key or it expires.
    if let Some(m) = app.message() {
        let line = message_line(m, body.width);
        let w = line.width() as u16;
        let at = Rect {
            x: body.right() - w,
            y: body.y,
            width: w,
            height: body.height.min(1),
        };
        f.render_widget(line, at);
    }
}

fn render_body(f: &mut Frame, app: &mut App, image: &mut Option<StatefulProtocol>, body: Rect) {
    if app.pane() == Pane::History {
        let vp = Viewport {
            width: body.width,
            height: body.height,
        };
        match app.history_view(vp) {
            Some(view) => f.render_widget(view, body),
            None => f.render_widget(no_session_line(), body),
        }
        return;
    }
    let shown = app.shown();
    let Some(s) = shown else {
        f.render_widget(no_file_line(), body);
        return;
    };
    let scroll = s.scroll;
    match &s.body {
        Body::Text { doc, hl } => f.render_widget(TextView { doc, hl, scroll }, body),
        Body::Table(lines) => f.render_widget(LinesView { lines, scroll }, body),
        Body::Info { label, size } => {
            f.render_widget(Paragraph::new(info_lines(label, *size)), body)
        }
        Body::Missing => f.render_widget(no_file_line(), body),
        Body::Image(_) => {
            if let Some(p) = image {
                f.render_stateful_widget(StatefulImage::<StatefulProtocol>::default(), body, p);
            }
        }
    }
}

struct Viewer {
    app: App,
    terminal: Term,
    picker: Picker,
    /// The terminal may have changed (resize, pane shown again) since the
    /// picker was made; ask again before the next image is built.
    picker_stale: bool,
    image: Option<StatefulProtocol>,
    input: Input,
    tx: Sender<AppEvent>,
    browser: Sender<BrowserCmd>,
    /// The normalized state file (`None` when showing fixed paths).
    state: Option<PathBuf>,
    watch: FsWatch,
    /// What `watch` was last set to successfully.
    watching: Vec<PathBuf>,
}

impl Viewer {
    /// Needs the terminal entered: the picker query comes before the input thread.
    fn start(view: View, watch: FsWatch, tx: Sender<AppEvent>) -> io::Result<Self> {
        let terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
        let picker = make_picker();
        let input = Input::start(tx.clone())?;
        let path_var = std::env::var_os("PATH");
        let browser = browser::start(path_var.clone(), view.on_browse, tx.clone());
        let ctx = Ctx {
            home: std::env::var_os("HOME").map(PathBuf::from),
            icon_width: view.icon_width,
        };
        let (state, file, session) = match view.source {
            Source::State(p) => (Some(normalize(&p)), None, None),
            Source::Fixed { file, session } => (
                None,
                file.map(|f| normalize(&f)),
                session.map(|s| normalize(&s)),
            ),
        };
        let mut v = Self {
            app: App::new(Highlighter::new(), ctx, path_var),
            terminal,
            picker,
            picker_stale: false,
            image: None,
            input,
            tx,
            browser,
            state,
            watch,
            watching: Vec::new(),
        };
        // Watch before every read, so a write in between is not missed: the
        // state file before reading it, the shown file/session before loading.
        v.sync_watch();
        let first = match &v.state {
            Some(p) => normalize_state(&std::fs::read(p).unwrap_or_default()),
            None => file_state(file.as_deref(), session.as_deref()),
        };
        let actions = v.app.on_state_bytes(&first);
        v.sync_watch();
        v.act_all(actions)?;
        Ok(v)
    }

    fn run(&mut self, rx: &Receiver<AppEvent>) -> io::Result<()> {
        let mut highlighting = false;
        loop {
            self.sync_watch();
            self.draw()?;
            let wait = if highlighting {
                Duration::ZERO
            } else {
                IDLE_WAIT
            };
            let mut events = Vec::new();
            match rx.recv_timeout(wait) {
                Ok(ev) => events.push(ev),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
            events.extend(rx.try_iter());
            // Resize / focus events come in bursts: redraw once.
            let mut refresh = false;
            for ev in events {
                if self.handle(ev, &mut refresh)? {
                    return Ok(());
                }
            }
            if refresh {
                // Querying reads stdin, which can cost keys: ask only when an
                // image needs the answer (at once if one is shown).
                self.picker_stale = true;
                self.redraw_all()?;
            }
            highlighting = self.app.tick_highlight(HL_BUDGET);
            // The loop wakes at least every `IDLE_WAIT`, so a message goes
            // within that much of `MESSAGE_TTL`; the next `draw` clears it.
            self.app.expire_message(std::time::Instant::now());
        }
    }

    /// Returns true to quit.
    fn handle(&mut self, ev: AppEvent, refresh: &mut bool) -> io::Result<bool> {
        match ev {
            AppEvent::Input(Event::Key(key)) => {
                let action = self.app.on_key(key, self.viewport());
                self.act(action)
            }
            AppEvent::Input(Event::Resize(..) | Event::FocusGained) => {
                *refresh = true;
                Ok(false)
            }
            AppEvent::Input(_) => Ok(false),
            AppEvent::Fs(paths) => self.on_fs(paths),
            AppEvent::Loaded(l) => {
                if self.app.on_loaded(l, self.viewport()) {
                    self.rebuild_image();
                }
                // A symlink may now point elsewhere: let the next
                // `sync_watch` read the links again.
                self.watching.clear();
                Ok(false)
            }
            AppEvent::SessionRead(r) => {
                let action = self.app.on_session_read(r, self.viewport());
                self.act(action)
            }
            AppEvent::Message(m) => {
                self.app.set_message(Some(m));
                Ok(false)
            }
        }
    }

    fn apply_state(&mut self, bytes: &[u8]) -> io::Result<bool> {
        let prev_tab = self.app.tab();
        let actions = self.app.on_state_bytes(bytes);
        let new_tab = self.app.tab();
        if new_tab != prev_tab {
            let _ = self.browser.send(BrowserCmd::Follow { tab: new_tab });
        }
        self.act_all(actions)
    }

    fn on_fs(&mut self, mut paths: Vec<PathBuf>) -> io::Result<bool> {
        let watched_file = self.app.watched_file().map(Path::to_path_buf);
        let watched_session = self.app.watched_session().map(Path::to_path_buf);
        paths.retain(|p| {
            is_relevant(
                p,
                self.state.as_deref(),
                watched_file.as_deref(),
                watched_session.as_deref(),
            )
        });
        paths.sort();
        paths.dedup();
        if let Some(state) = &self.state
            && paths.contains(state)
            && let Ok(bytes) = std::fs::read(state)
            && self.apply_state(&normalize_state(&bytes))?
        {
            return Ok(true);
        }
        for p in &paths {
            let actions = self.app.on_fs_change(p);
            if self.act_all(actions)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn act_all(&mut self, actions: Vec<Action>) -> io::Result<bool> {
        for action in actions {
            if self.act(action)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Returns true to quit.
    fn act(&mut self, action: Action) -> io::Result<bool> {
        match action {
            Action::None => {}
            Action::Load { path, generation } => spawn_load(path, generation, self.tx.clone()),
            Action::ReadSession {
                path,
                offset,
                generation,
            } => spawn_read(path, offset, generation, self.tx.clone()),
            Action::Open { path, kind, alt } => {
                let kind = kind.unwrap_or_else(|| sniff_kind(&path));
                self.open_external(&path, kind, alt)?;
            }
            Action::Browse { url } => {
                let _ = self.browser.send(BrowserCmd::Open {
                    tab: self.app.tab(),
                    url,
                });
            }
            Action::Redraw => self.redraw_all()?,
            Action::Quit => return Ok(true),
        }
        Ok(false)
    }

    fn open_external(&mut self, path: &Path, kind: FileKind, alt: bool) -> io::Result<()> {
        let mut cmd = match build_command(path, kind, alt, std::env::var_os("PATH").as_deref()) {
            Ok(cmd) => cmd,
            Err(m) => {
                self.app.set_message(Some(m));
                return Ok(());
            }
        };
        self.input.pause();
        leave()?;
        let status = cmd.status();
        enter()?;
        self.input.resume();
        self.redraw_all()?;
        if let Err(e) = status {
            self.app.set_message(Some(e.to_string()));
        }
        Ok(())
    }

    /// Clears the screen and the previous frame, so the next draw sends every
    /// cell and the image again. `Terminal::clear` is not used: it asks the
    /// terminal for the cursor position, which can fail or time out.
    fn redraw_all(&mut self) -> io::Result<()> {
        self.terminal.backend_mut().clear_region(ClearType::All)?;
        self.terminal.swap_buffers();
        self.rebuild_image();
        Ok(())
    }

    /// The cell size is a stand-in while the pane is hidden; ask again when
    /// it may have changed.
    fn fresh_picker(&mut self) {
        if !self.picker_stale {
            return;
        }
        self.input.pause();
        self.picker = make_picker();
        self.input.resume();
        self.picker_stale = false;
    }

    fn rebuild_image(&mut self) {
        if matches!(
            self.app.shown(),
            Some(Shown {
                body: Body::Image(_),
                ..
            })
        ) {
            self.fresh_picker();
        }
        self.image = match self.app.shown() {
            Some(Shown {
                body: Body::Image(img),
                ..
            }) => Some(self.picker.new_resize_protocol(img.clone())),
            _ => None,
        };
    }

    /// Watches the state file, the watched file, and the watched session;
    /// a failure is retried on the next turn and never stops the viewer.
    fn sync_watch(&mut self) {
        let want: Vec<PathBuf> = self
            .state
            .iter()
            .cloned()
            .chain(self.app.watched_file().map(Path::to_path_buf))
            .chain(self.app.watched_session().map(Path::to_path_buf))
            .collect();
        if want == self.watching && !self.watch.take_resync() {
            return;
        }
        let files: Vec<&Path> = want.iter().map(PathBuf::as_path).collect();
        if let Ok(changed) = self.watch.set_files(&files) {
            self.watching = want;
            if changed {
                if let Some(state) = &self.state
                    && let Ok(bytes) = std::fs::read(state)
                {
                    let _ = self.apply_state(&normalize_state(&bytes));
                }
                let watched_file = self.app.watched_file().map(Path::to_path_buf);
                let watched_session = self.app.watched_session().map(Path::to_path_buf);
                if let Some(p) = &watched_file {
                    let actions = self.app.on_fs_change(p);
                    let _ = self.act_all(actions);
                }
                if let Some(p) = &watched_session
                    && watched_file.as_ref() != Some(p)
                {
                    let actions = self.app.on_fs_change(p);
                    let _ = self.act_all(actions);
                }
            }
        }
    }

    fn viewport(&self) -> Viewport {
        let size = self.terminal.size().unwrap_or_default();
        Viewport {
            width: size.width,
            // No header line (issue #1); with it, this was `height - 1`.
            // height: size.height.saturating_sub(1),
            height: size.height,
        }
    }

    fn draw(&mut self) -> io::Result<()> {
        let (app, image) = (&mut self.app, &mut self.image);
        self.terminal.draw(|f| render(f, app, image))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wtr::state::parse_state;

    #[test]
    fn state_file_path_is_normalized_and_other_keys_kept() {
        let d = tempfile::tempdir().unwrap();
        let raw = d.path().join("a.txt");
        let bytes = serde_json::json!({"tab": "1", "file": raw.to_str().unwrap()}).to_string();
        let s = parse_state(&normalize_state(bytes.as_bytes())).unwrap();
        assert_eq!(s.file, Some(normalize(&raw)));
        assert_eq!(s.tab.as_deref(), Some("1"));
    }

    #[test]
    fn broken_state_is_passed_on_unchanged() {
        assert_eq!(normalize_state(br#"{"fi"#), br#"{"fi"#);
        assert_eq!(normalize_state(b""), b"");
    }

    #[test]
    fn file_state_names_the_file() {
        let s = parse_state(&file_state(Some(Path::new("/x/a b.txt")), None)).unwrap();
        assert_eq!(s.file.as_deref(), Some(Path::new("/x/a b.txt")));
        assert!(s.session.is_none());
    }

    #[test]
    fn only_state_and_watched_paths_are_relevant() {
        let (s, w, sess) = (
            Path::new("/s.json"),
            Path::new("/a.txt"),
            Path::new("/s.jsonl"),
        );
        assert!(is_relevant(s, Some(s), Some(w), Some(sess)));
        assert!(is_relevant(w, Some(s), Some(w), Some(sess)));
        assert!(is_relevant(sess, Some(s), Some(w), Some(sess)));
        assert!(is_relevant(w, None, Some(w), None));
        assert!(!is_relevant(
            Path::new("/tmp/other"),
            Some(s),
            Some(w),
            Some(sess)
        ));
        assert!(!is_relevant(Path::new("/tmp/other"), None, None, None));
    }

    #[test]
    fn session_path_is_normalized_and_relevant() {
        let d = tempfile::tempdir().unwrap();
        let raw_session = d.path().join("s.jsonl");
        let bytes = serde_json::json!({
            "tab": "1",
            "session": raw_session.to_str().unwrap()
        })
        .to_string();
        let parsed = parse_state(&normalize_state(bytes.as_bytes())).unwrap();
        let norm = normalize(&raw_session);
        assert_eq!(parsed.session, Some(norm.clone()));
        assert!(is_relevant(&norm, None, None, Some(&norm)));
    }
}
