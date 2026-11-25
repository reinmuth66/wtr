use crate::event::AppEvent;
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{Debouncer, new_debouncer};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

pub const DEBOUNCE: Duration = Duration::from_millis(50);

/// Resolve the nearest existing ancestor folder (FSEvents reports
/// `/private/var/...` for `/var/...`) and re-attach the remaining suffix.
/// Falls back to the path as given.
pub fn normalize(p: &Path) -> PathBuf {
    let mut cur = p.parent();
    while let Some(ancestor) = cur {
        if let Ok(canon) = ancestor.canonicalize()
            && let Ok(rest) = p.strip_prefix(ancestor)
        {
            return canon.join(rest);
        }
        cur = ancestor.parent();
    }
    p.to_path_buf()
}

/// Symlinks followed before giving up (a loop, or a very long chain).
const MAX_LINKS: usize = 40;

/// Every hop from `normalize(p)` through symlinks followed one at a time,
/// with the parent (or nearest existing ancestor) normalized at each step.
pub fn link_chain(p: &Path) -> Vec<PathBuf> {
    let mut cur = normalize(p);
    let mut chain = vec![cur.clone()];
    for _ in 0..MAX_LINKS {
        let Ok(t) = std::fs::read_link(&cur) else {
            break;
        };
        let next = match cur.parent() {
            Some(parent) => parent.join(t),
            None => t,
        };
        cur = normalize(&next);
        chain.push(cur.clone());
    }
    chain
}

/// The real file behind `p`: the last element of `link_chain(p)`. Unlike
/// `canonicalize`, this still answers when the last target does not exist
/// (yet), so a link whose target was deleted is still watched where the target
/// will come back.
pub fn link_target(p: &Path) -> PathBuf {
    link_chain(p).pop().expect("link_chain is non-empty")
}

/// Real or intermediate path -> the watched (symlink) paths it stands behind.
type Links = Arc<Mutex<HashMap<PathBuf, Vec<PathBuf>>>>;

/// Ancestor folders watched as proxies for missing parent folders.
type Proxies = Arc<Mutex<HashSet<PathBuf>>>;

/// Watches the parent folders of the given files, so that in-place writes,
/// rename replacements, and delete-and-recreate are all noticed. A file that
/// is a symlink also has every hop's folder watched; FSEvents reports the
/// hop's path, which is passed on together with the link's path. Missing
/// parent folders are watched via their nearest existing ancestor.
pub struct FsWatch {
    debouncer: Debouncer<RecommendedWatcher>,
    dirs: HashSet<PathBuf>,
    links: Links,
    proxies: Proxies,
    resync: Arc<AtomicBool>,
    file_dirs: HashMap<PathBuf, HashSet<PathBuf>>,
}

impl FsWatch {
    pub fn new(tx: Sender<AppEvent>) -> notify::Result<Self> {
        let links: Links = Arc::default();
        let proxies: Proxies = Arc::default();
        let resync = Arc::new(AtomicBool::new(false));
        let shared_links = links.clone();
        let shared_proxies = proxies.clone();
        let shared_resync = resync.clone();
        let debouncer = new_debouncer(
            DEBOUNCE,
            move |res: notify_debouncer_mini::DebounceEventResult| {
                if let Ok(events) = res {
                    let links = shared_links.lock().unwrap_or_else(PoisonError::into_inner);
                    let proxies = shared_proxies
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    let mut paths = Vec::new();
                    let mut need_resync = false;
                    for e in &events {
                        let p = normalize(&e.path);
                        if p.parent().is_some_and(|parent| proxies.contains(parent)) {
                            need_resync = true;
                        }
                        if let Some(ls) = links.get(&p) {
                            paths.extend(ls.iter().cloned());
                        }
                        paths.push(p);
                    }
                    if need_resync {
                        shared_resync.store(true, Ordering::Release);
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
            proxies,
            resync,
            file_dirs: HashMap::new(),
        })
    }

    /// Returns `true` once if an event was observed directly under a proxy
    /// ancestor folder (meaning a missing folder may now exist).
    pub fn take_resync(&self) -> bool {
        self.resync.swap(false, Ordering::AcqRel)
    }

    /// Watch the parent folder (or nearest existing ancestor) of every hop in
    /// each file's `link_chain` (once per folder) and stop watching folders no
    /// longer needed. Files without a parent are skipped. Symlinks are read
    /// again on every call, so call it again after a link may have been
    /// pointed elsewhere.
    ///
    /// Returns `Ok(true)` if any file that was also present in the previous
    /// call now has a different set of watched folders.
    pub fn set_files(&mut self, files: &[&Path]) -> notify::Result<bool> {
        let mut links: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        let mut new_proxies: HashSet<PathBuf> = HashSet::new();
        let mut want: HashSet<PathBuf> = HashSet::new();
        let mut new_file_dirs: HashMap<PathBuf, HashSet<PathBuf>> = HashMap::new();
        let mut changed = false;

        for f in files {
            let chain = link_chain(f);
            let path = chain[0].clone();
            let mut f_dirs = HashSet::new();
            for hop in &chain {
                if hop != &path {
                    links.entry(hop.clone()).or_default().push(path.clone());
                }
                if let Some((dir, is_proxy)) = watch_dir_for(hop) {
                    if is_proxy {
                        new_proxies.insert(dir.clone());
                    }
                    want.insert(dir.clone());
                    f_dirs.insert(dir);
                }
            }
            if let Some(prev) = self.file_dirs.get(&path)
                && prev != &f_dirs
            {
                changed = true;
            }
            new_file_dirs.insert(path, f_dirs);
        }

        self.file_dirs = new_file_dirs;
        *self.proxies.lock().unwrap_or_else(PoisonError::into_inner) = new_proxies;
        *self.links.lock().unwrap_or_else(PoisonError::into_inner) = links;

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
        first_err.map_or(Ok(changed), Err)
    }
}

/// Returns the parent directory of `p` if it exists, or the nearest existing
/// ancestor directory marked as a proxy (`is_proxy = true`).
fn watch_dir_for(p: &Path) -> Option<(PathBuf, bool)> {
    let direct = p.parent()?;
    if direct.is_dir() {
        return Some((direct.to_path_buf(), false));
    }
    let mut cur = direct.parent();
    while let Some(anc) = cur {
        if anc.is_dir() {
            return Some((anc.to_path_buf(), true));
        }
        cur = anc.parent();
    }
    None
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

    #[test]
    fn normalize_resolves_through_a_missing_folder() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("new/sub/a.txt");
        assert_eq!(
            normalize(&p),
            d.path().canonicalize().unwrap().join("new/sub/a.txt")
        );
    }

    #[test]
    fn link_chain_lists_every_hop() {
        let d = tempfile::tempdir().unwrap();
        let e = tempfile::tempdir().unwrap();
        let f = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        let b = e.path().join("b");
        let c = f.path().join("c");
        std::os::unix::fs::symlink(&b, &a).unwrap();
        std::os::unix::fs::symlink(&c, &b).unwrap();
        assert_eq!(
            link_chain(&a),
            vec![normalize(&a), normalize(&b), normalize(&c)]
        );
    }

    #[test]
    fn missing_folder_is_watched_through_its_ancestor_without_retrying() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("new/a.txt");
        let (tx, _rx) = channel();
        let mut w = FsWatch::new(tx).unwrap();
        assert!(!w.set_files(&[&p]).unwrap());
        assert!(!w.set_files(&[&p]).unwrap());
        assert_eq!(w.dirs.len(), 1);
        assert!(w.dirs.contains(&d.path().canonicalize().unwrap()));
    }

    #[test]
    fn file_in_a_folder_created_later_is_seen() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("new/a.txt");
        let (tx, rx) = channel();
        let mut w = FsWatch::new(tx).unwrap();
        assert!(!w.set_files(&[&p]).unwrap());
        std::thread::sleep(Duration::from_millis(200));
        std::fs::create_dir(d.path().join("new")).unwrap();
        let end = Instant::now() + Duration::from_secs(2);
        while !w.take_resync() {
            assert!(Instant::now() < end, "timed out waiting for resync");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(w.set_files(&[&p]).unwrap());
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(&p, "hello").unwrap();
        assert!(wait_for(&rx, &normalize(&p)));
    }

    #[test]
    fn a_mid_chain_link_in_another_folder_is_watched() {
        let d = tempfile::tempdir().unwrap();
        let e = tempfile::tempdir().unwrap();
        let f = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        let b = e.path().join("b");
        let c = f.path().join("c");
        let c2 = f.path().join("c2");
        std::fs::write(&c, "1").unwrap();
        std::fs::write(&c2, "2").unwrap();
        std::os::unix::fs::symlink(&b, &a).unwrap();
        std::os::unix::fs::symlink(&c, &b).unwrap();
        let (tx, rx) = channel();
        let mut w = FsWatch::new(tx).unwrap();
        w.set_files(&[&a]).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        std::fs::remove_file(&b).unwrap();
        std::os::unix::fs::symlink(&c2, &b).unwrap();
        std::fs::write(&c2, "3").unwrap();
        assert!(wait_for(&rx, &normalize(&a)));
    }

    #[test]
    fn retargeting_a_link_reports_a_change() {
        let d = tempfile::tempdir().unwrap();
        let e = tempfile::tempdir().unwrap();
        let f = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        let x = e.path().join("x");
        let y = f.path().join("y");
        std::os::unix::fs::symlink(&x, &a).unwrap();
        let (tx, _rx) = channel();
        let mut w = FsWatch::new(tx).unwrap();
        assert!(!w.set_files(&[&a]).unwrap());
        std::fs::remove_file(&a).unwrap();
        std::os::unix::fs::symlink(&y, &a).unwrap();
        assert!(w.set_files(&[&a]).unwrap());
        assert!(!w.set_files(&[&a]).unwrap());
    }
}
