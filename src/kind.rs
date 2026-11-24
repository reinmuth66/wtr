use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Text,
    Json,
    Csv,
    Tsv,
    Image,
    Other,
}

/// How many leading bytes `classify` needs to see.
pub const SNIFF_LEN: usize = 8192;

const IMAGE_EXTS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp"];

const OTHER_EXTS: [&str; 25] = [
    "pdf",
    "doc",
    "docx",
    "xls",
    "xlsx",
    "ppt",
    "pptx",
    "odt",
    "ods",
    "odp",
    "3mf",
    "kicad_pcb",
    "kicad_sch",
    "kicad_pro",
    "kicad_sym",
    "kicad_mod",
    "zip",
    "tar",
    "gz",
    "tgz",
    "bz2",
    "xz",
    "zst",
    "7z",
    "rar",
];

/// Classify a file by extension first, then by its leading bytes.
/// Text that is not UTF-8 (Latin-1, Shift_JIS, UTF-16) is still `Text`;
/// only a NUL byte makes content `Other`.
pub fn classify(path: &Path, head: &[u8]) -> FileKind {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let ext = ext.as_str();
    if IMAGE_EXTS.contains(&ext) {
        return FileKind::Image;
    }
    if OTHER_EXTS.contains(&ext) {
        return FileKind::Other;
    }
    if ext == "json" || path.file_name().is_some_and(|n| n == "flake.lock") {
        return FileKind::Json;
    }
    match ext {
        "csv" => return FileKind::Csv,
        "tsv" => return FileKind::Tsv,
        _ => {}
    }
    if head.starts_with(&[0xff, 0xfe]) || head.starts_with(&[0xfe, 0xff]) {
        return FileKind::Text;
    }
    if head.contains(&0) {
        return FileKind::Other;
    }
    FileKind::Text
}

/// Human readable size in 1024 steps: `B` as an integer, the rest with one decimal.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// The extension in capitals (`PDF`), or `FILE` when there is none.
pub fn kind_label(path: &Path) -> String {
    match path.extension() {
        Some(e) => e.to_string_lossy().to_uppercase(),
        None => "FILE".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn c(name: &str, head: &[u8]) -> FileKind {
        classify(Path::new(name), head)
    }

    #[test]
    fn by_extension() {
        assert_eq!(c("a.PNG", b""), FileKind::Image);
        assert_eq!(c("a.svg", b"<svg"), FileKind::Image);
        assert_eq!(c("a.pdf", b"%PDF"), FileKind::Other);
        assert_eq!(c("b.kicad_pcb", b"(kicad_pcb"), FileKind::Other);
        assert_eq!(c("a.json", b"{}"), FileKind::Json);
        assert_eq!(c("/x/flake.lock", b"{}"), FileKind::Json);
        assert_eq!(c("a.csv", b"a,b"), FileKind::Csv);
        assert_eq!(c("a.tsv", b"a\tb"), FileKind::Tsv);
        assert_eq!(c("a.yaml", b"a: 1"), FileKind::Text);
        assert_eq!(c("Makefile", b"all:"), FileKind::Text);
    }

    #[test]
    fn nul_is_other() {
        assert_eq!(c("a.bin", b"ab\0cd"), FileKind::Other);
    }

    #[test]
    fn cut_multibyte_at_end_is_text() {
        let s = "あいう".as_bytes();
        assert_eq!(c("a.txt", &s[..s.len() - 1]), FileKind::Text);
    }

    #[test]
    fn non_utf8_text_is_text() {
        assert_eq!(c("latin.txt", &[0x61, 0xff, 0x62, b'\n']), FileKind::Text);
        assert_eq!(
            c("sjis.txt", &[0x82, 0xa0, 0x82, 0xa2, b'\n']),
            FileKind::Text
        );
        assert_eq!(
            c("utf16.txt", &[0xff, 0xfe, b'a', 0, b'b', 0]),
            FileKind::Text
        );
    }

    #[test]
    fn sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536 * 1024), "1.5 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn labels() {
        assert_eq!(kind_label(Path::new("a.pdf")), "PDF");
        assert_eq!(kind_label(Path::new("LICENSE")), "FILE");
    }
}
