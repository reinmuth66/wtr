use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::records::{Class, FIELDS, classify, content_kind, record_kind};

#[derive(Default)]
struct UnknownEntry {
    count: u64,
    first_version: Option<String>,
    fields: BTreeSet<String>,
}

#[derive(Default)]
struct MissingEntry {
    count: u64,
    last_version: Option<String>,
}

/// Compares dotted version strings numerically segment by segment (non-numeric segments count as 0).
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.split('.').map(|s| s.parse::<u64>().unwrap_or(0));
    let mut bi = b.split('.').map(|s| s.parse::<u64>().unwrap_or(0));
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (x, y) => {
                let ord = x.unwrap_or(0).cmp(&y.unwrap_or(0));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

/// Recursively collects `*.jsonl` files under `roots` (or the root itself if it is a file), sorted by path.
pub fn jsonl_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        if root.is_file() {
            out.push(root.clone());
        } else if root.is_dir() {
            collect_dir(root, &mut out);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn collect_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if ft.is_dir() {
            collect_dir(&path, out);
        } else if ft.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

/// Scans session JSONL files for unknown record/content kinds and missing used fields.
pub fn scan(files: &[PathBuf]) -> Value {
    let mut file_count: u64 = 0;
    let mut record_count: u64 = 0;
    let mut unknown: BTreeMap<String, UnknownEntry> = BTreeMap::new();
    let mut missing: BTreeMap<(String, String), MissingEntry> = BTreeMap::new();

    let mut buf = Vec::new();
    for path in files {
        let Ok(file) = File::open(path) else {
            continue;
        };
        file_count += 1;
        let mut reader = BufReader::new(file);
        let mut tool_names: HashMap<String, String> = HashMap::new();
        let mut pending_tools: Vec<(String, Value, Option<String>)> = Vec::new();

        loop {
            buf.clear();
            let Ok(n) = reader.read_until(b'\n', &mut buf) else {
                break;
            };
            if n == 0 {
                break;
            }
            let Ok(line) = std::str::from_utf8(&buf) else {
                continue;
            };
            let Ok(rec) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            record_count += 1;

            let version = rec.get("version").and_then(Value::as_str);
            let rkind = record_kind(&rec);

            if classify(&rkind) == Class::Unknown {
                let field_src = if rkind.starts_with("attachment:") {
                    rec.get("attachment")
                } else {
                    Some(&rec)
                };
                record_unknown(&mut unknown, rkind.clone(), version, field_src);
            }

            check_fields(&mut missing, &rkind, &rec, version);

            if rkind == "assistant"
                && let Some(items) = rec.pointer("/message/content").and_then(Value::as_array)
            {
                for item in items {
                    let ckind = content_kind("assistant", item);
                    if classify(&ckind) == Class::Unknown {
                        record_unknown(&mut unknown, ckind, version, Some(item));
                    }
                    if item.get("type").and_then(Value::as_str) == Some("tool_use")
                        && let (Some(id), Some(name)) = (
                            item.get("id").and_then(Value::as_str),
                            item.get("name").and_then(Value::as_str),
                        )
                    {
                        tool_names.insert(id.to_string(), name.to_string());
                    }
                }
            } else if rkind == "user"
                && let Some(items) = rec.pointer("/message/content").and_then(Value::as_array)
            {
                let has_tool_obj = rec.get("toolUseResult").is_some_and(Value::is_object);
                for item in items {
                    let ckind = content_kind("user", item);
                    if classify(&ckind) == Class::Unknown {
                        record_unknown(&mut unknown, ckind, version, Some(item));
                    }
                    if has_tool_obj
                        && item.get("type").and_then(Value::as_str) == Some("tool_result")
                        && let Some(id) = item.get("tool_use_id").and_then(Value::as_str)
                    {
                        if let Some(name) = tool_names.get(id) {
                            let tkind = format!("tool:{name}");
                            check_fields(&mut missing, &tkind, &rec, version);
                        } else {
                            pending_tools.push((
                                id.to_string(),
                                rec.clone(),
                                version.map(str::to_string),
                            ));
                        }
                    }
                }
            }
        }

        for (id, rec, ver) in pending_tools {
            if let Some(name) = tool_names.get(&id) {
                let tkind = format!("tool:{name}");
                check_fields(&mut missing, &tkind, &rec, ver.as_deref());
            }
        }
    }

    let unknown_json: Vec<Value> = unknown
        .into_iter()
        .map(|(kind, e)| {
            let fields: Vec<String> = e.fields.into_iter().collect();
            json!({
                "kind": kind,
                "count": e.count,
                "first_version": e.first_version,
                "fields": fields,
            })
        })
        .collect();

    let missing_json: Vec<Value> = missing
        .into_iter()
        .map(|((kind, field), e)| {
            json!({
                "kind": kind,
                "field": field,
                "count": e.count,
                "last_version": e.last_version,
            })
        })
        .collect();

    json!({
        "files": file_count,
        "records": record_count,
        "unknown": unknown_json,
        "missing_fields": missing_json,
    })
}

fn record_unknown(
    unknown: &mut BTreeMap<String, UnknownEntry>,
    kind: String,
    version: Option<&str>,
    field_src: Option<&Value>,
) {
    let entry = unknown.entry(kind).or_default();
    entry.count += 1;
    if let Some(v) = version {
        let replace = entry
            .first_version
            .as_deref()
            .is_none_or(|cur| version_cmp(v, cur) == Ordering::Less);
        if replace {
            entry.first_version = Some(v.to_string());
        }
    }
    if let Some(obj) = field_src.and_then(Value::as_object) {
        for key in obj.keys() {
            entry.fields.insert(key.clone());
        }
    }
}

fn check_fields(
    missing: &mut BTreeMap<(String, String), MissingEntry>,
    kind: &str,
    rec: &Value,
    version: Option<&str>,
) {
    for &(fkind, ptr) in FIELDS {
        if fkind == kind && rec.pointer(ptr).is_none() {
            let entry = missing
                .entry((fkind.to_string(), ptr.to_string()))
                .or_default();
            entry.count += 1;
            if let Some(v) = version {
                let replace = entry
                    .last_version
                    .as_deref()
                    .is_none_or(|cur| version_cmp(v, cur) == Ordering::Greater);
                if replace {
                    entry.last_version = Some(v.to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cmp::Ordering;

    #[test]
    fn counts_unknown_kinds_with_first_version_and_field_names() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.jsonl");
        let f2 = dir.path().join("b.jsonl");
        std::fs::write(
            &f1,
            "{\"type\":\"brand-new\",\"version\":\"2.1.330\",\"secret\":\"x\"}\n",
        )
        .unwrap();
        std::fs::write(&f2, "{\"type\":\"brand-new\",\"version\":\"2.1.329\"}\n").unwrap();

        let out = scan(&[f1, f2]);
        assert_eq!(out["files"], 2);
        assert_eq!(out["records"], 2);
        assert_eq!(
            out["unknown"],
            json!([{
                "kind": "brand-new",
                "count": 2,
                "first_version": "2.1.329",
                "fields": ["secret", "type", "version"]
            }])
        );
        let serialized = out.to_string();
        assert!(!serialized.contains("\"x\""));
    }

    #[test]
    fn unknown_content_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.jsonl");
        std::fs::write(
            &f,
            "{\"type\":\"assistant\",\"version\":\"2.1.330\",\"message\":{\"content\":[{\"type\":\"server_tool_use\",\"id\":\"1\"}]}}\n",
        )
        .unwrap();

        let out = scan(&[f]);
        assert_eq!(
            out["unknown"],
            json!([{
                "kind": "assistant/server_tool_use",
                "count": 1,
                "first_version": "2.1.330",
                "fields": ["id", "type"]
            }])
        );
    }

    #[test]
    fn known_kinds_are_not_listed() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.jsonl");
        std::fs::write(
            &f,
            concat!(
                "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n",
                "{\"type\":\"system\",\"subtype\":\"turn_duration\",\"durationMs\":10}\n",
                "{\"type\":\"cost-state\"}\n"
            ),
        )
        .unwrap();

        let out = scan(&[f]);
        assert_eq!(out["unknown"], json!([]));
        assert_eq!(out["missing_fields"], json!([]));
    }

    #[test]
    fn missing_used_fields() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.jsonl");
        std::fs::write(
            &f,
            concat!(
                "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"e1\",\"name\":\"Edit\",\"input\":{}}]}}\n",
                "{\"type\":\"user\",\"version\":\"2.1.331\",\"toolUseResult\":{\"filePath\":\"/a\"},\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"e1\",\"content\":\"ok\"}]}}\n"
            ),
        )
        .unwrap();

        let out = scan(&[f]);
        assert_eq!(
            out["missing_fields"],
            json!([{
                "kind": "tool:Edit",
                "field": "/toolUseResult/structuredPatch",
                "count": 1,
                "last_version": "2.1.331"
            }])
        );
    }

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(version_cmp("2.1.99", "2.1.326"), Ordering::Less);
        assert_eq!(version_cmp("2.1.326", "2.1.99"), Ordering::Greater);
        assert_eq!(version_cmp("2.1.0", "2.1"), Ordering::Equal);
        assert_eq!(version_cmp("2.1.abc", "2.1.0"), Ordering::Equal);
    }

    #[test]
    fn finds_jsonl_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join("s/subagents")).unwrap();
        let a = d.join("a.jsonl");
        let b = d.join("s/subagents/b.jsonl");
        let c = d.join("c.txt");
        std::fs::write(&a, "").unwrap();
        std::fs::write(&b, "").unwrap();
        std::fs::write(&c, "").unwrap();

        assert_eq!(jsonl_files(&[d.to_path_buf()]), vec![a, b]);
    }
}
