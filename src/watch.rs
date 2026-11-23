use crate::event::AppEvent;
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{Debouncer, new_debouncer};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

pub const DEBOUNCE: Duration = Duration::from_millis(50);

/// Resolve the parent folder (FSEvents reports `/private/var/...` for `/var/...`)
/// and re-attach the file name. Falls back to the path as given.
pub fn normalize(p: &Path) -> PathBuf {
    let (Some(parent), Some(name)) = (p.parent(), p.file_name()) else {
        return p.to_path_buf();
    };
    match parent.canonicalize() {
        Ok(parent) => parent.join(name),
        Err(_) => p.to_path_buf(),
    }
}

/// Watches the parent folders of the given files, so that in-place writes,
/// rename replacements, and delete-and-recreate are all noticed.
pub struct FsWatch {
    debouncer: Debouncer<RecommendedWatcher>,
    dirs: HashSet<PathBuf>,
}

impl FsWatch {
    pub fn new(tx: Sender<AppEvent>) -> notify::Result<Self> {
        let debouncer = new_debouncer(
            DEBOUNCE,
            move |res: notify_debouncer_mini::DebounceEventResult| {
                if let Ok(events) = res {
                    let paths: Vec<PathBuf> = events.iter().map(|e| normalize(&e.path)).collect();
                    if !paths.is_empty() {
                        let _ = tx.send(AppEvent::Fs(paths));
                    }
                }
            },
        )?;
        Ok(Self {
            debouncer,
            dirs: HashSet::new(),
        })
    }

    /// Watch the parent folder of each file (once per folder) and stop
    /// watching folders no longer needed. Files without a parent are skipped.
    pub fn set_files(&mut self, files: &[&Path]) -> notify::Result<()> {
        let want: HashSet<PathBuf> = files
            .iter()
            .filter_map(|f| normalize(f).parent().map(Path::to_path_buf))
            .collect();
        let stale: Vec<PathBuf> = self.dirs.difference(&want).cloned().collect();
        for d in stale {
            // The folder may be gone already; nothing left to unwatch then.
            let _ = self.debouncer.watcher().unwatch(&d);
            self.dirs.remove(&d);
        }
        for d in want {
            if self.dirs.contains(&d) {
                continue;
            }
            self.debouncer
                .watcher()
                .watch(&d, RecursiveMode::NonRecursive)?;
            self.dirs.insert(d);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Instant;

    fn wait_for(rx: &Receiver<AppEvent>, want: &Path) -> bool {
        let end = Instant::now() + Duration::from_secs(3);
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            if let Ok(AppEvent::Fs(ps)) = rx.recv_timeout(left)
                && ps.iter().any(|p| p == want)
            {
                return true;
            }
        }
        false
    }

    #[test]
    fn normalize_resolves_symlinked_parent() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            normalize(&d.path().join("s.json")),
            d.path().canonicalize().unwrap().join("s.json")
        );
    }

    #[test]
    fn normalize_keeps_path_when_parent_is_missing() {
        let p = Path::new("/nonexistent-wtr-dir/s.json");
        assert_eq!(normalize(p), p);
    }

    #[test]
    fn sees_in_place_write() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        std::fs::write(&p, "{}").unwrap();
        let (tx, rx) = channel();
        let mut w = FsWatch::new(tx).unwrap();
        w.set_files(&[&p]).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(&p, r#"{"file":"/a"}"#).unwrap();
        assert!(wait_for(&rx, &normalize(&p)));
    }

    #[test]
    fn sees_rename_replace() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        let tmp = d.path().join("s.json.tmp");
        std::fs::write(&p, "{}").unwrap();
        let (tx, rx) = channel();
        let mut w = FsWatch::new(tx).unwrap();
        w.set_files(&[&p]).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(&tmp, r#"{"file":"/a"}"#).unwrap();
        std::fs::rename(&tmp, &p).unwrap();
        assert!(wait_for(&rx, &normalize(&p)));
    }

    #[test]
    fn switching_files_moves_the_watch() {
        let d1 = tempfile::tempdir().unwrap();
        let d2 = tempfile::tempdir().unwrap();
        let a = d1.path().join("a");
        let b = d2.path().join("b");
        std::fs::write(&a, "1").unwrap();
        std::fs::write(&b, "1").unwrap();
        let (tx, rx) = channel();
        let mut w = FsWatch::new(tx).unwrap();
        w.set_files(&[&a]).unwrap();
        w.set_files(&[&b]).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(&a, "2").unwrap();
        let end = Instant::now() + Duration::from_secs(1);
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            if let Ok(AppEvent::Fs(ps)) = rx.recv_timeout(left) {
                assert!(!ps.contains(&normalize(&a)), "event for unwatched file");
            }
        }
        std::fs::write(&b, "2").unwrap();
        assert!(wait_for(&rx, &normalize(&b)));
    }

    #[test]
    fn same_folder_is_watched_once_and_parentless_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        let q = d.path().join("other.txt");
        let (tx, _rx) = channel();
        let mut w = FsWatch::new(tx).unwrap();
        w.set_files(&[&p, &q, Path::new("/")]).unwrap();
        assert_eq!(w.dirs.len(), 1);
    }
}
