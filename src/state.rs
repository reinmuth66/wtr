use crate::watch::normalize;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The state file written by the hook. Unknown keys are ignored.
#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
pub struct StateFile {
    pub tab: Option<String>,
    pub file: Option<PathBuf>,
}

/// Parse the state file. Broken, empty or mistyped content gives `None`,
/// so the caller can keep the previous content and wait.
pub fn parse_state(bytes: &[u8]) -> Option<StateFile> {
    serde_json::from_slice(bytes).ok()
}

#[derive(Debug, PartialEq)]
pub enum Target {
    Show(PathBuf),
    Keep,
    Missing,
}

const README_NAMES: [&str; 3] = ["readme.md", "readme.markdown", "readme"];

/// Decide what to show for the `file` of the state file.
/// A folder resolves to its README when it has one, otherwise `Keep`.
pub fn resolve_target(file: Option<&Path>) -> Target {
    let Some(path) = file else {
        return Target::Missing;
    };
    let Ok(meta) = std::fs::metadata(path) else {
        return Target::Missing;
    };
    if !meta.is_dir() {
        return Target::Show(path.to_path_buf());
    }
    match find_readme(path) {
        // The folder itself may be a symlink: normalize like any shown file,
        // so file events (which carry real paths) match it.
        Some(readme) => Target::Show(normalize(&readme)),
        None => Target::Keep,
    }
}

fn find_readme(dir: &Path) -> Option<PathBuf> {
    let mut entries: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_file())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    // Uppercase sorts first, so the smallest name wins among case variants.
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    README_NAMES.iter().find_map(|candidate| {
        entries
            .iter()
            .find(|(name, _)| name.to_lowercase() == *candidate)
            .map(|(_, path)| path.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    /// A `session` key (written by tools from when wtr showed the session
    /// history) and other unknown keys are ignored.
    fn parses_full() {
        let s = parse_state(br#"{"tab":"1","file":"/a","session":"/s.jsonl","x":1}"#).unwrap();
        assert_eq!(
            s,
            StateFile {
                tab: Some("1".into()),
                file: Some("/a".into()),
            }
        );
    }
    #[test]
    fn missing_keys_are_none() {
        assert_eq!(parse_state(b"{}").unwrap(), StateFile::default());
    }
    #[test]
    fn broken_is_none() {
        assert!(parse_state(br#"{"tab":"1","fi"#).is_none());
        assert!(parse_state(b"").is_none());
        assert!(parse_state(br#"{"tab":1}"#).is_none());
    }
    #[test]
    fn readme_order_and_case() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("readme"), "").unwrap();
        std::fs::write(d.path().join("Readme.Markdown"), "").unwrap();
        assert_eq!(
            resolve_target(Some(d.path())),
            Target::Show(normalize(&d.path().join("Readme.Markdown")))
        );
        std::fs::write(d.path().join("README.md"), "").unwrap();
        std::fs::write(d.path().join("readme.md"), "").unwrap();
        assert_eq!(
            resolve_target(Some(d.path())),
            Target::Show(normalize(&d.path().join("README.md")))
        );
    }
    #[test]
    fn readme_of_a_symlinked_folder_is_its_real_path() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("README.md"), "").unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            resolve_target(Some(&link)),
            Target::Show(real.canonicalize().unwrap().join("README.md"))
        );
    }
    #[test]
    fn dir_without_readme_keeps() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(resolve_target(Some(d.path())), Target::Keep);
    }
    #[test]
    fn missing_paths() {
        assert_eq!(resolve_target(None), Target::Missing);
        assert_eq!(
            resolve_target(Some(Path::new("/no/such/file"))),
            Target::Missing
        );
    }
}
