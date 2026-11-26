use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::kind::FileKind;

/// The external viewer for a file: `nvim` normally, a specialised one when `alt` is set.
pub fn tool_for(kind: FileKind, alt: bool) -> (&'static str, &'static [&'static str]) {
    match (alt, kind) {
        (true, FileKind::Csv) => ("csvlens", &[]),
        (true, FileKind::Tsv) => ("csvlens", &["-t"]),
        (true, FileKind::Json) => ("fx", &[]),
        _ => ("nvim", &[]),
    }
}

/// The first executable regular file called `name` in the folders of `path_var`.
pub fn find_in_path(name: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path_var?)
        .map(|dir| dir.join(name))
        .find(|p| {
            std::fs::metadata(p)
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}

/// The nearest ancestor holding `.git` (a folder, or a file for a worktree).
pub fn repo_root(file: &Path) -> Option<PathBuf> {
    file.ancestors()
        .skip(1)
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Where an external viewer runs: the repository root, else the file's folder.
pub fn work_dir(file: &Path) -> PathBuf {
    repo_root(file)
        .or_else(|| file.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Builds, without spawning, the command that opens `path` in its external viewer.
pub fn build_command(
    path: &Path,
    kind: FileKind,
    alt: bool,
    path_var: Option<&OsStr>,
) -> Result<Command, String> {
    let (tool, args) = tool_for(kind, alt);
    let exe = find_in_path(tool, path_var).ok_or_else(|| format!("{tool} not found"))?;
    let mut cmd = Command::new(exe);
    cmd.args(args).arg(path).current_dir(work_dir(path));
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools() {
        assert_eq!(tool_for(FileKind::Csv, false).0, "nvim");
        assert_eq!(tool_for(FileKind::Csv, true), ("csvlens", &[][..]));
        assert_eq!(tool_for(FileKind::Tsv, true), ("csvlens", &["-t"][..]));
        assert_eq!(tool_for(FileKind::Json, true).0, "fx");
        assert_eq!(tool_for(FileKind::Image, true).0, "nvim");
        assert_eq!(tool_for(FileKind::Text, true).0, "nvim");
    }

    #[test]
    fn finds_only_executables() {
        let d = tempfile::tempdir().unwrap();
        let exe = d.path().join("nvim");
        std::fs::write(&exe, "").unwrap();
        std::fs::write(d.path().join("fx"), "").unwrap();
        std::fs::set_permissions(&exe, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let pv = d.path().as_os_str();
        assert_eq!(find_in_path("nvim", Some(pv)), Some(exe));
        assert_eq!(find_in_path("fx", Some(pv)), None);
    }

    #[test]
    fn repo_root_with_git_file_or_dir() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("r/sub")).unwrap();
        std::fs::write(d.path().join("r/.git"), "gitdir: x").unwrap();
        assert_eq!(
            repo_root(&d.path().join("r/sub/a.txt")),
            Some(d.path().join("r"))
        );
        assert_eq!(work_dir(&d.path().join("a.txt")), d.path());
    }

    #[test]
    fn missing_tool_message() {
        let e = build_command(
            Path::new("/x/a.json"),
            FileKind::Json,
            true,
            Some(OsStr::new("")),
        )
        .unwrap_err();
        assert_eq!(e, "fx not found");
    }
}
