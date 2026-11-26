use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use crate::event::AppEvent;

/// Complete JSONL records read from a byte offset, plus the new offset.
#[derive(Debug)]
pub struct Chunk {
    pub records: Vec<serde_json::Value>,
    pub end: u64,
    pub reset: bool,
}

/// Result of an asynchronous session file read.
pub struct SessionRead {
    pub generation: u64,
    pub result: io::Result<Chunk>,
}

/// Reads complete JSONL lines from `offset` (or from `0` if the file shrank).
/// Empty or unparseable lines are discarded. `end` is the byte after the last
/// `\n`, or the starting offset if no complete line was present.
pub fn read_from(path: &Path, offset: u64) -> io::Result<Chunk> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let (start, reset) = if len < offset {
        (0, true)
    } else {
        (offset, false)
    };
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else {
        return Ok(Chunk {
            records: Vec::new(),
            end: start,
            reset,
        });
    };

    let mut records = Vec::new();
    for raw in buf[..last_nl].split(|&b| b == b'\n') {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_slice(line) {
            records.push(value);
        }
    }

    Ok(Chunk {
        records,
        end: start + last_nl as u64 + 1,
        reset,
    })
}

/// Reads `path` from `offset` on a named `"session"` thread and sends
/// `AppEvent::SessionRead`. Panics are caught and reported as an `io::Error`.
pub fn spawn_read(path: PathBuf, offset: u64, generation: u64, tx: Sender<AppEvent>) {
    let tx_err = tx.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("session".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| read_from(&path, offset)))
                .unwrap_or_else(|_| Err(io::Error::other("panicked")));
            let _ = tx.send(AppEvent::SessionRead(SessionRead { generation, result }));
        })
    {
        let _ = tx_err.send(AppEvent::SessionRead(SessionRead {
            generation,
            result: Err(err),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use std::path::Path;

    fn tmp(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("session.jsonl");
        std::fs::write(&p, bytes).unwrap();
        (d, p)
    }

    #[test]
    fn partial_last_line_waits() {
        let (_d, p) = tmp(b"{\"a\":1}\n{\"b\":");
        let c1 = read_from(&p, 0).unwrap();
        assert_eq!(c1.records, vec![json!({"a": 1})]);
        assert_eq!(c1.end, 8);
        assert!(!c1.reset);

        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"2}\n").unwrap();
        let len = std::fs::metadata(&p).unwrap().len();

        let c2 = read_from(&p, 8).unwrap();
        assert_eq!(c2.records, vec![json!({"b": 2})]);
        assert_eq!(c2.end, len);
        assert!(!c2.reset);
    }

    #[test]
    fn broken_and_empty_lines_are_skipped() {
        let input = b"{\"a\":1}\n\nnot json\n{\"c\":3}\n";
        let (_d, p) = tmp(input);
        let c = read_from(&p, 0).unwrap();
        assert_eq!(c.records, vec![json!({"a": 1}), json!({"c": 3})]);
        assert_eq!(c.end, input.len() as u64);
        assert!(!c.reset);
    }

    #[test]
    fn shorter_file_resets() {
        let (_d, p) = tmp(b"{\"a\":1}\n{\"b\":2}\n");
        let c1 = read_from(&p, 0).unwrap();
        assert_eq!(c1.records.len(), 2);

        std::fs::write(&p, b"{\"x\":9}\n").unwrap();
        let c2 = read_from(&p, c1.end).unwrap();
        assert!(c2.reset);
        assert_eq!(c2.records, vec![json!({"x": 9})]);
        assert_eq!(c2.end, 8);
    }

    #[test]
    fn no_complete_line_keeps_offset() {
        let (_d, p) = tmp(b"{\"a\"");
        let c = read_from(&p, 0).unwrap();
        assert!(c.records.is_empty());
        assert_eq!(c.end, 0);
        assert!(!c.reset);
    }

    #[test]
    fn missing_file_is_error() {
        assert!(read_from(Path::new("/no/such.jsonl"), 0).is_err());
    }

    #[test]
    fn crlf_lines() {
        let (_d, p) = tmp(b"{\"a\":1}\r\n");
        let c = read_from(&p, 0).unwrap();
        assert_eq!(c.records, vec![json!({"a": 1})]);
    }

    #[test]
    fn spawn_read_sends_generation() {
        let (_d, p) = tmp(b"{\"a\":1}\n");
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_read(p, 0, 42, tx);
        let AppEvent::SessionRead(sr) = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap()
        else {
            panic!()
        };
        assert_eq!(sr.generation, 42);
        let chunk = sr.result.unwrap();
        assert_eq!(chunk.records, vec![json!({"a": 1})]);
    }
}
