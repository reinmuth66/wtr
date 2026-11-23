use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, OnceLock};

use ratatui::text::Line;

use crate::csv_table::{MAX_ROWS, read_table, render_table};
use crate::event::AppEvent;
use crate::highlight::PLAIN_THRESHOLD;
use crate::kind::{FileKind, SNIFF_LEN, classify, kind_label};
use crate::text_doc::{TextDoc, is_json_document, text_doc_from_bytes};

/// Longest side, in pixels, of a rasterized SVG.
const SVG_MAX_SIDE: f32 = 2048.0;

/// At most this much of a CSV / TSV file is read, so an unterminated quote
/// cannot turn the rest of a huge file into one cell.
const CSV_READ_LIMIT: u64 = 64 * 1024 * 1024;

pub enum Content {
    Text { doc: TextDoc, plain: bool },
    Table(Vec<Line<'static>>),
    Image(image::DynamicImage),
    Info { label: String, size: u64 },
    Missing,
}

pub struct Loaded {
    pub generation: u64,
    pub path: PathBuf,
    pub kind: FileKind,
    pub content: Content,
}

/// Loads `path` on a new thread and sends the result as `AppEvent::Loaded`.
/// A closed receiver is ignored.
pub fn spawn_load(path: PathBuf, generation: u64, tx: Sender<AppEvent>) {
    std::thread::spawn(move || {
        let (kind, content) = load(&path);
        let _ = tx.send(AppEvent::Loaded(Loaded {
            generation,
            path,
            kind,
            content,
        }));
    });
}

/// Synchronous load. Only regular files are opened: opening a FIFO would
/// block forever.
pub fn load(path: &Path) -> (FileKind, Content) {
    let Ok(meta) = std::fs::metadata(path) else {
        return (FileKind::Other, Content::Missing);
    };
    if !meta.is_file() {
        return info(path, 0);
    }
    let size = meta.len();
    let Ok(mut file) = File::open(path) else {
        return (FileKind::Other, Content::Missing);
    };
    let mut head = Vec::new();
    if file
        .by_ref()
        .take(SNIFF_LEN as u64)
        .read_to_end(&mut head)
        .is_err()
    {
        return (FileKind::Other, Content::Missing);
    }
    let mut kind = classify(path, &head);
    match kind {
        FileKind::Text | FileKind::Json => {
            let mut bytes = head;
            if file.read_to_end(&mut bytes).is_err() {
                return (FileKind::Other, Content::Missing);
            }
            if kind == FileKind::Text && path.extension().is_none() && is_json_document(&bytes) {
                kind = FileKind::Json;
            }
            (kind, text_content(path, &bytes, kind, size))
        }
        FileKind::Csv | FileKind::Tsv => {
            let delimiter = if kind == FileKind::Csv { b',' } else { b'\t' };
            let table = File::open(path)
                .map_err(csv::Error::from)
                .and_then(|f| read_table(f.take(CSV_READ_LIMIT), delimiter, MAX_ROWS));
            match table {
                Ok(t) => (kind, Content::Table(render_table(&t))),
                Err(_) => {
                    let mut bytes = head;
                    if file.read_to_end(&mut bytes).is_err() {
                        return (FileKind::Other, Content::Missing);
                    }
                    (
                        FileKind::Text,
                        text_content(path, &bytes, FileKind::Text, size),
                    )
                }
            }
        }
        FileKind::Image => match decode_image(path) {
            Some(img) => (kind, Content::Image(img)),
            None => (
                kind,
                Content::Info {
                    label: kind_label(path),
                    size,
                },
            ),
        },
        FileKind::Other => info(path, size),
    }
}

fn info(path: &Path, size: u64) -> (FileKind, Content) {
    (
        FileKind::Other,
        Content::Info {
            label: kind_label(path),
            size,
        },
    )
}

fn text_content(path: &Path, bytes: &[u8], kind: FileKind, size: u64) -> Content {
    Content::Text {
        doc: text_doc_from_bytes(path, bytes, kind),
        plain: size > PLAIN_THRESHOLD,
    }
}

fn decode_image(path: &Path) -> Option<image::DynamicImage> {
    let is_svg = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"));
    if is_svg {
        decode_svg(path)
    } else {
        image::ImageReader::open(path)
            .ok()?
            .with_guessed_format()
            .ok()?
            .decode()
            .ok()
    }
}

/// Loading system fonts takes about 140ms, so it is done once.
fn fontdb() -> Arc<resvg::usvg::fontdb::Database> {
    static DB: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
    .clone()
}

fn decode_svg(path: &Path) -> Option<image::DynamicImage> {
    let data = std::fs::read(path).ok()?;
    let opt = resvg::usvg::Options {
        fontdb: fontdb(),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_data(&data, &opt).ok()?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    let scale = (SVG_MAX_SIDE / w.max(h)).min(1.0);
    let pw = ((w * scale).round() as u32).max(1);
    let ph = ((h * scale).round() as u32).max(1);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, ph)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(pw as f32 / w, ph as f32 / h),
        &mut pixmap.as_mut(),
    );
    let mut rgba = Vec::with_capacity(pixmap.pixels().len() * 4);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    image::RgbaImage::from_raw(pw, ph, rgba).map(image::DynamicImage::ImageRgba8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tmp(name: &str, bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        (d, p)
    }

    #[test]
    fn text_and_json() {
        let (_d, p) = tmp("a.json", br#"{"a":1}"#);
        let (k, Content::Text { doc, plain }) = load(&p) else {
            panic!()
        };
        assert_eq!((k, doc.lines.len(), plain), (FileKind::Json, 3, false));
    }

    #[test]
    fn extensionless_json_is_json() {
        let (_d, p) = tmp(".prettierrc", br#"{"semi":false}"#);
        assert!(matches!(load(&p), (FileKind::Json, Content::Text { .. })));
        let (_d2, p2) = tmp("notes", b"{ not json");
        assert!(matches!(load(&p2), (FileKind::Text, Content::Text { .. })));
    }

    #[test]
    fn csv_becomes_table() {
        let (_d, p) = tmp("a.csv", b"a,b\n1,2\n");
        assert!(matches!(load(&p), (FileKind::Csv, Content::Table(l)) if l.len() == 2));
    }

    #[test]
    fn png_decodes() {
        let (_d, p) = tmp("a.png", b"");
        image::RgbaImage::new(2, 3).save(&p).unwrap();
        assert!(matches!(load(&p).1, Content::Image(i) if i.width() == 2 && i.height() == 3));
    }

    #[test]
    fn svg_decodes() {
        let (_d, p) = tmp(
            "a.svg",
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="5"/>"#,
        );
        assert!(matches!(load(&p).1, Content::Image(i) if i.width() == 4 && i.height() == 5));
    }

    #[test]
    fn broken_image_is_info() {
        let (_d, p) = tmp("a.png", b"not a png");
        assert!(matches!(load(&p).1, Content::Info { label, size: 9 } if label == "PNG"));
    }

    #[test]
    fn other_and_missing() {
        let (_d, p) = tmp("a.pdf", b"%PDF-1.7");
        assert!(matches!(load(&p).1, Content::Info { label, size: 8 } if label == "PDF"));
        assert!(matches!(load(Path::new("/no/such")).1, Content::Missing));
    }

    #[test]
    fn large_text_is_plain() {
        let (_d, p) = tmp("a.txt", &vec![b'a'; (PLAIN_THRESHOLD + 1) as usize]);
        assert!(matches!(load(&p).1, Content::Text { plain: true, .. }));
    }

    #[test]
    fn spawn_sends_generation() {
        let (_d, p) = tmp("a.txt", b"x");
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_load(p, 7, tx);
        let AppEvent::Loaded(l) = rx.recv_timeout(Duration::from_secs(5)).unwrap() else {
            panic!()
        };
        assert_eq!(l.generation, 7);
    }

    #[test]
    fn large_svg_is_shrunk() {
        let (_d, p) = tmp(
            "big.svg",
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="4096" height="1024"/>"#,
        );
        assert!(matches!(load(&p).1, Content::Image(i) if i.width() == 2048 && i.height() == 512));
    }

    #[test]
    fn spawn_survives_closed_receiver() {
        let (_d, p) = tmp("a.txt", b"x");
        let (tx, rx) = std::sync::mpsc::channel();
        drop(rx);
        spawn_load(p, 1, tx);
    }

    /// `mkfifo(1)` instead of `libc::mkfifo`: libc is only a transitive dependency.
    #[cfg(unix)]
    #[test]
    fn fifo_is_not_opened() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("pipe.txt");
        let status = std::process::Command::new("mkfifo")
            .arg(&p)
            .status()
            .unwrap();
        assert!(status.success());
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_load(p, 1, tx);
        let AppEvent::Loaded(l) = rx.recv_timeout(Duration::from_secs(5)).unwrap() else {
            panic!()
        };
        assert_eq!(l.kind, FileKind::Other);
        assert!(matches!(l.content, Content::Info { label, size: 0 } if label == "TXT"));
    }
}
