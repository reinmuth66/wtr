use crate::event::AppEvent;
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{Debouncer, new_debouncer};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};
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

/// Symlinks followed before giving up (a loop, or a very long chain).
const MAX_LINKS: usize = 40;

/// The real file behind `p`: symlinks are followed one at a time, with the
/// parent normalized at each step. Unlike `canonicalize`, this still answers
/// when the last target does not exist (yet), so a link whose target was
/// deleted is still watched where the target will come back.
pub fn link_target(p: &Path) -> PathBuf {
    let mut p = normalize(p);
    for _ in 0..MAX_LINKS {
        let Ok(t) = std::fs::read_link(&p) else { break };
        let next = match p.parent() {
            Some(parent) => parent.join(t),
            None => t,
        };
        p = normalize(&next);
    }
    p
}

/// Real path -> the watched (symlink) paths it stands behind.
type Links = Arc<Mutex<HashMap<PathBuf, Vec<PathBuf>>>>;

/// Watches the parent folders of the given files, so that in-place writes,
/// rename replacements, and delete-and-recreate are all noticed. A file that
/// is a symlink also has its target's folder watched; FSEvents reports the
/// target's path, which is passed on together with the link's path.
pub struct FsWatch {
    debouncer: Debouncer<RecommendedWatcher>,
    dirs: HashSet<PathBuf>,
    links: Links,
}

impl FsWatch {
    pub fn new(tx: Sender<AppEvent>) -> notify::Result<Self> {
        let links: Links = Arc::default();
        let shared = links.clone();
        let debouncer = new_debouncer(
            DEBOUNCE,
            move |res: notify_debouncer_mini::DebounceEventResult| {
                if let Ok(events) = res {
                    let links = shared.lock().unwrap_or_else(PoisonError::into_inner);
                    let mut paths = Vec::new();
                    for e in &events {
                        let p = normalize(&e.path);
                        if let Some(ls) = links.get(&p) {
                            paths.extend(ls.iter().cloned());
                        }
                        paths.push(p);
                    }
                    if !paths.is_empty() {
                        let _ = tx.send(AppEvent::Fs(paths));
                    }
                }
            },
        )?;
        Ok(Self {
            debouncer,
            dirs: HashSet::new(),
            links,
        })
    }

    /// Watch the parent folder of each file and of each symlink's target
    /// (once per folder) and stop watching folders no longer needed. Files
    /// without a parent are skipped. Symlinks are read again on every call,
    /// so call it again after a link may have been pointed elsewhere.
    pub fn set_files(&mut self, files: &[&Path]) -> notify::Result<()> {
        let mut links: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        let mut real = Vec::new();
        for f in files {
            let (path, target) = (normalize(f), link_target(f));
            if target != path {
                links.entry(target.clone()).or_default().push(path.clone());
                real.push(target);
            }
            real.push(path);
        }
        *self.links.lock().unwrap_or_else(PoisonError::into_inner) = links;
        let want: HashSet<PathBuf> = real
            .iter()
            .filter_map(|f| f.parent().map(Path::to_path_buf))
            .collect();
        let stale: Vec<PathBuf> = self.dirs.difference(&want).cloned().collect();
        for d in stale {
            // The folder may be gone already; nothing left to unwatch then.
            let _ = self.debouncer.watcher().unwatch(&d);
            self.dirs.remove(&d);
        }
        // One folder failing must not leave the others unwatched: keep going
        // and return the first error at the end. Failed folders are not
        // recorded, so a later call retries them.
        let mut first_err = None;
        for d in want {
            if self.dirs.contains(&d) {
                continue;
            }
            match self
                .debouncer
                .watcher()
                .watch(&d, RecursiveMode::NonRecursive)
            {
                Ok(()) => {
                    self.dirs.insert(d);
                }
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        first_err.map_or(Ok(()), Err)
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
    fn link_target_follows_chains_and_dangling_links() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().canonicalize().unwrap();
        let target = d.path().join("AGENTS.md");
        let mid = d.path().join("mid");
        let link = d.path().join("CLAUDE.md");
        // Relative targets resolve against the link's folder.
        std::os::unix::fs::symlink("mid", &link).unwrap();
        std::os::unix::fs::symlink(&target, &mid).unwrap();
        // The target does not exist yet.
        assert_eq!(link_target(&link), real.join("AGENTS.md"));
        std::fs::write(&target, "x").unwrap();
        assert_eq!(link_target(&link), target.canonicalize().unwrap());
        // A plain file is its own target; a loop gives up.
        assert_eq!(link_target(&target), normalize(&target));
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::os::unix::fs::symlink(&b, &a).unwrap();
        std::os::unix::fs::symlink(&a, &b).unwrap();
        let _ = link_target(&a);
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
    fn a_failing_folder_does_not_stop_the_others() {
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("missing").join("x");
        let p = d.path().join("s.json");
        std::fs::write(&p, "{}").unwrap();
        let (tx, rx) = channel();
        let mut w = FsWatch::new(tx).unwrap();
        // Each fresh `want` set may be visited in a different (hash) order.
        for files in [
            [missing.as_path(), p.as_path()],
            [p.as_path(), missing.as_path()],
        ] {
            w.set_files(&[]).unwrap();
            assert!(w.set_files(&files).is_err());
            assert_eq!(w.dirs.len(), 1, "only the existing folder is recorded");
            std::thread::sleep(Duration::from_millis(200));
            std::fs::write(&p, "2").unwrap();
            assert!(wait_for(&rx, &normalize(&p)));
        }
    }

    #[test]
    fn symlinked_file_reports_its_link_when_the_target_changes() {
        let links = tempfile::tempdir().unwrap();
        let targets = tempfile::tempdir().unwrap();
        let target = targets.path().join("AGENTS.md");
        let link = links.path().join("CLAUDE.md");
        std::fs::write(&target, "1").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let (tx, rx) = channel();
        let mut w = FsWatch::new(tx).unwrap();
        w.set_files(&[&link]).unwrap();
        assert_eq!(w.dirs.len(), 2, "the link's folder and the target's");
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(&target, "2").unwrap();
        assert!(wait_for(&rx, &normalize(&link)));
        // A rename replacement of the target is seen as well.
        let tmp = targets.path().join("AGENTS.md.tmp");
        std::fs::write(&tmp, "3").unwrap();
        std::fs::rename(&tmp, &target).unwrap();
        assert!(wait_for(&rx, &normalize(&link)));
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
