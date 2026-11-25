use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};

use serde_json::Value;

use crate::event::AppEvent;
use crate::external::find_in_path;

/// Encodes `path` as a `file://` URL, escaping spaces, `%`, `#`, and `?`.
pub fn file_url(path: &Path) -> String {
    let mut out = String::from("file://");
    for ch in path.to_string_lossy().chars() {
        match ch {
            ' ' => out.push_str("%20"),
            '%' => out.push_str("%25"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            _ => out.push(ch),
        }
    }
    out
}

/// Returns true when `path` has an `.html` or `.htm` extension (case-insensitive).
pub fn is_html(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
}

/// Abstracts running `terminal-browser` for testing.
pub trait Run {
    fn run(&mut self, args: &[String]) -> Result<String, String>;
}

/// Runs the real `terminal-browser` executable.
pub struct TbRun {
    pub exe: PathBuf,
}

impl Run for TbRun {
    fn run(&mut self, args: &[String]) -> Result<String, String> {
        let out = Command::new(&self.exe)
            .args(args)
            .env_remove("TERM_PROGRAM")
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let line = stderr.lines().next().unwrap_or("").trim().to_string();
            return Err(if line.is_empty() {
                format!("exit {}", out.status)
            } else {
                line
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Tracks one `terminal-browser` tab per state tab and dispatches `open` / `follow`.
pub struct Controller<R: Run> {
    run: R,
    tabs: HashMap<Option<String>, (String, u64)>,
    on_browse: Option<String>,
}

impl<R: Run> Controller<R> {
    pub fn new(run: R, on_browse: Option<String>) -> Self {
        Self {
            run,
            tabs: HashMap::new(),
            on_browse,
        }
    }

    /// Opens `url` in the browser tab associated with `tab`, creating one if needed.
    pub fn open(&mut self, tab: Option<String>, url: &str) -> Result<(), String> {
        let (key, ids) = self.ls()?;
        let existing = self
            .tabs
            .get(&tab)
            .filter(|(k, id)| *k == key && ids.contains(id))
            .map(|(_, id)| *id);

        if let Some(id) = existing {
            let id_str = id.to_string();
            self.run.run(&str_args(&[
                "action",
                "--browser",
                &key,
                "--tab",
                &id_str,
                "--follow",
                "--",
                "goto",
                url,
            ]))?;
        } else {
            let reply =
                self.run
                    .run(&str_args(&["action", "--browser", &key, "--", "open", url]))?;
            if let Some(id) = parse_opened_tab(&reply) {
                self.tabs.insert(tab, (key.clone(), id));
            }
        }

        self.run
            .run(&str_args(&["action", "--browser", &key, "done"]))?;

        if let Some(cmd) = &self.on_browse {
            run_on_browse(cmd)?;
        }
        Ok(())
    }

    /// Brings the remembered browser tab for `tab` forward, or discards it if gone.
    pub fn follow(&mut self, tab: Option<String>) {
        let Some((saved_key, saved_id)) = self.tabs.get(&tab).cloned() else {
            return;
        };
        let Ok((key, ids)) = self.ls() else {
            return;
        };
        if key == saved_key && ids.contains(&saved_id) {
            let id_str = saved_id.to_string();
            let _ = self.run.run(&str_args(&[
                "action",
                "--browser",
                &key,
                "--tab",
                &id_str,
                "--follow",
                "--",
                "get",
                "url",
            ]));
            let _ = self
                .run
                .run(&str_args(&["action", "--browser", &key, "done"]));
        } else {
            self.tabs.remove(&tab);
        }
    }

    fn ls(&mut self) -> Result<(String, Vec<u64>), String> {
        let raw = self.run.run(&str_args(&["ls", "--all", "--json"]))?;
        let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        let browser = v
            .get("browsers")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .ok_or_else(|| String::from("no browser"))?;
        let key = browser
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| String::from("no browser"))?
            .to_string();
        let ids = browser
            .get("tabs")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.get("id").and_then(Value::as_u64))
                    .collect()
            })
            .unwrap_or_default();
        Ok((key, ids))
    }
}

fn str_args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn parse_opened_tab(reply: &str) -> Option<u64> {
    serde_json::from_str::<Value>(reply)
        .ok()
        .and_then(|v| v.get("openedTab").and_then(Value::as_u64))
}

fn run_on_browse(cmd: &str) -> Result<(), String> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        let code = status
            .code()
            .map_or_else(|| String::from("?"), |c| c.to_string());
        return Err(format!("on-browse exit {code}"));
    }
    Ok(())
}

/// Commands sent to the background `browser` thread.
pub enum BrowserCmd {
    Open { tab: Option<String>, url: String },
    Follow { tab: Option<String> },
}

/// Spawns the `browser` worker thread and returns its command sender.
pub fn start(
    path_var: Option<OsString>,
    on_browse: Option<String>,
    events: Sender<AppEvent>,
) -> Sender<BrowserCmd> {
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name(String::from("browser"))
        .spawn(move || {
            let Some(exe) = find_in_path("terminal-browser", path_var.as_deref()) else {
                while let Ok(cmd) = rx.recv() {
                    if matches!(cmd, BrowserCmd::Open { .. }) {
                        let _ = events.send(AppEvent::Message(String::from(
                            "terminal-browser not found",
                        )));
                    }
                }
                return;
            };
            let mut ctrl = Controller::new(TbRun { exe }, on_browse);
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    BrowserCmd::Open { tab, url } => {
                        if let Err(e) = ctrl.open(tab, &url) {
                            let _ = events.send(AppEvent::Message(e));
                        }
                    }
                    BrowserCmd::Follow { tab } => {
                        ctrl.follow(tab);
                    }
                }
            }
        });
    tx
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::Path;

    use super::*;

    struct Fake {
        calls: Vec<Vec<String>>,
        replies: VecDeque<Result<String, String>>,
    }

    impl Fake {
        fn new(replies: impl IntoIterator<Item = Result<&'static str, &'static str>>) -> Self {
            Self {
                calls: Vec::new(),
                replies: replies
                    .into_iter()
                    .map(|r| r.map(str::to_string).map_err(str::to_string))
                    .collect(),
            }
        }
    }

    impl Run for Fake {
        fn run(&mut self, args: &[String]) -> Result<String, String> {
            self.calls.push(args.to_vec());
            self.replies
                .pop_front()
                .expect("unexpected call in Fake::run")
        }
    }

    const LS: &str =
        r#"{"self":null,"browsers":[{"key":"9-1","tabs":[{"id":2,"url":"a","active":true}]}]}"#;
    const LS_WITH_5: &str = r#"{"self":null,"browsers":[{"key":"9-1","tabs":[{"id":2,"url":"a","active":true},{"id":5,"url":"file:///x.html","active":true}]}]}"#;

    #[test]
    fn first_open_creates_a_tab_and_remembers_it() {
        let fake = Fake::new([Ok(LS), Ok(r#"{"openedTab":5}"#), Ok("{}")]);
        let mut ctrl = Controller::new(fake, None);
        ctrl.open(None, "file:///x.html").unwrap();
        assert_eq!(
            ctrl.run.calls,
            vec![
                vec!["ls", "--all", "--json"],
                vec!["action", "--browser", "9-1", "--", "open", "file:///x.html"],
                vec!["action", "--browser", "9-1", "done"],
            ]
        );
    }

    #[test]
    fn second_open_reuses_the_tab_with_follow() {
        let fake = Fake::new([
            Ok(LS),
            Ok(r#"{"openedTab":5}"#),
            Ok("{}"),
            Ok(LS_WITH_5),
            Ok("{}"),
            Ok("{}"),
        ]);
        let mut ctrl = Controller::new(fake, None);
        ctrl.open(None, "file:///x.html").unwrap();
        ctrl.run.calls.clear();
        ctrl.open(None, "https://e").unwrap();
        assert_eq!(
            ctrl.run.calls,
            vec![
                vec!["ls", "--all", "--json"],
                vec![
                    "action",
                    "--browser",
                    "9-1",
                    "--tab",
                    "5",
                    "--follow",
                    "--",
                    "goto",
                    "https://e"
                ],
                vec!["action", "--browser", "9-1", "done"],
            ]
        );
    }

    #[test]
    fn a_closed_tab_is_made_again() {
        let fake = Fake::new([
            Ok(LS),
            Ok(r#"{"openedTab":5}"#),
            Ok("{}"),
            Ok(LS),
            Ok(r#"{"openedTab":6}"#),
            Ok("{}"),
        ]);
        let mut ctrl = Controller::new(fake, None);
        ctrl.open(None, "file:///x.html").unwrap();
        ctrl.run.calls.clear();
        ctrl.open(None, "https://e").unwrap();
        assert_eq!(
            ctrl.run.calls[1],
            vec!["action", "--browser", "9-1", "--", "open", "https://e"]
        );
    }

    #[test]
    fn tabs_are_per_state_tab() {
        let fake = Fake::new([
            Ok(LS),
            Ok(r#"{"openedTab":5}"#),
            Ok("{}"),
            Ok(LS_WITH_5),
            Ok(r#"{"openedTab":6}"#),
            Ok("{}"),
        ]);
        let mut ctrl = Controller::new(fake, None);
        ctrl.open(Some("1".into()), "file:///a.html").unwrap();
        ctrl.open(Some("2".into()), "file:///b.html").unwrap();
        assert_eq!(
            ctrl.run.calls[1],
            vec!["action", "--browser", "9-1", "--", "open", "file:///a.html"]
        );
        assert_eq!(
            ctrl.run.calls[4],
            vec!["action", "--browser", "9-1", "--", "open", "file:///b.html"]
        );
    }

    #[test]
    fn follow_brings_the_tab_forward_or_does_nothing() {
        let fake = Fake::new([
            Ok(LS),
            Ok(r#"{"openedTab":5}"#),
            Ok("{}"),
            Ok(LS_WITH_5),
            Ok("{}"),
            Ok("{}"),
        ]);
        let mut ctrl = Controller::new(fake, None);
        ctrl.follow(Some("1".into()));
        assert!(ctrl.run.calls.is_empty());

        ctrl.open(Some("1".into()), "file:///x.html").unwrap();
        ctrl.run.calls.clear();
        ctrl.follow(Some("1".into()));
        assert_eq!(
            ctrl.run.calls,
            vec![
                vec!["ls", "--all", "--json"],
                vec![
                    "action",
                    "--browser",
                    "9-1",
                    "--tab",
                    "5",
                    "--follow",
                    "--",
                    "get",
                    "url"
                ],
                vec!["action", "--browser", "9-1", "done"],
            ]
        );
    }

    #[test]
    fn no_browser() {
        let fake = Fake::new([Ok(r#"{"browsers":[]}"#)]);
        let mut ctrl = Controller::new(fake, None);
        assert_eq!(ctrl.open(None, "file:///x.html").unwrap_err(), "no browser");
    }

    #[test]
    fn file_urls() {
        assert_eq!(
            file_url(Path::new("/a b/c#1.html")),
            "file:///a%20b/c%231.html"
        );
    }

    #[test]
    fn on_browse_runs_after_open() {
        let fake = Fake::new([Ok(LS), Ok(r#"{"openedTab":5}"#), Ok("{}")]);
        let mut ctrl = Controller::new(fake, Some("exit 3".into()));
        assert_eq!(
            ctrl.open(None, "file:///x.html").unwrap_err(),
            "on-browse exit 3"
        );
    }
}
