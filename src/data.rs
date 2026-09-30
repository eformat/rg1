//! JSONL and CSV record readers with dotted-path field extraction.

use crate::prefilter::Prefilter;
use crate::record::{Record, RecordKind};

fn note(msg: &str) {
    eprintln!("rg1: {msg}");
}

/// Navigate a dotted path (e.g. `user.name`, `items.0.id`) through a JSON value.
pub fn field_value<'a>(v: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cur = v;
    for seg in path.split('.') {
        match cur {
            serde_json::Value::Object(map) => {
                cur = map.get(seg)?;
            }
            serde_json::Value::Array(items) => {
                let idx: usize = seg.parse().ok()?;
                cur = items.get(idx)?;
            }
            _ => return None,
        }
    }
    Some(cur)
}

fn json_leaf_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

pub fn jsonl_records(
    path: &Option<std::path::PathBuf>,
    content: &str,
    field: Option<&str>,
    filter: Option<&Prefilter>,
    p: &mut crate::inputs::Prepared,
) {
    for (i, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let lineno = i as u64 + 1;
        let body = match field {
            Some(f) => match serde_json::from_str::<serde_json::Value>(line) {
                Ok(v) => match field_value(&v, f).and_then(json_leaf_string) {
                    Some(s) => s,
                    None => {
                        note(&format!("line {lineno}: field '{f}' missing or not a scalar; skipping"));
                        p.errors += 1;
                        continue;
                    }
                },
                Err(e) => {
                    note(&format!("line {lineno}: invalid JSON ({e}); skipping"));
                    p.errors += 1;
                    continue;
                }
            },
            None => line.to_string(),
        };
        let rec = Record::new(path.clone(), RecordKind::Jsonl, lineno, lineno, body);
        crate::inputs::keep(p, rec, filter);
    }
}

pub fn csv_records(
    path: &Option<std::path::PathBuf>,
    content: &str,
    field: Option<&str>,
    filter: Option<&Prefilter>,
    p: &mut crate::inputs::Prepared,
) {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(content.as_bytes());
    let headers: Vec<String> = match reader.headers() {
        Ok(h) => h.iter().map(|s| s.to_string()).collect(),
        Err(e) => {
            note(&format!("csv: {e}"));
            p.errors += 1;
            return;
        }
    };
    for result in reader.records() {
        let row = match result {
            Ok(r) => r,
            Err(e) => {
                note(&format!("csv: {e}; skipping row"));
                p.errors += 1;
                continue;
            }
        };
        let lineno = row.position().map(|pos| pos.line()).unwrap_or(0);
        let body = match field {
            Some(f) => match csv_field(&row, &headers, f) {
                Some(s) => s,
                None => {
                    note(&format!("csv row at line {lineno}: field '{f}' missing; skipping"));
                    p.errors += 1;
                    continue;
                }
            },
            None => row.iter().collect::<Vec<_>>().join(", "),
        };
        let rec = Record::new(path.clone(), RecordKind::Csv, lineno, lineno, body);
        crate::inputs::keep(p, rec, filter);
    }
}

/// CSV field lookup: exact header name first (dotted headers supported),
/// then the first segment as a header or 1-based column index.
fn csv_field(row: &csv::StringRecord, headers: &[String], field: &str) -> Option<String> {
    if let Some(idx) = headers.iter().position(|h| h == field) {
        return row.get(idx).map(|s| s.to_string());
    }
    let first = field.split('.').next()?;
    if let Some(idx) = headers.iter().position(|h| h == first) {
        return row.get(idx).map(|s| s.to_string());
    }
    if let Ok(idx) = first.parse::<usize>() {
        return row.get(idx.saturating_sub(1)).map(|s| s.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_paths() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"user":{"name":"Ada","tags":["x","y"]}}"#).unwrap();
        assert_eq!(
            field_value(&v, "user.name").and_then(json_leaf_string),
            Some("Ada".to_string())
        );
        assert_eq!(
            field_value(&v, "user.tags.1").and_then(json_leaf_string),
            Some("y".to_string())
        );
        assert!(field_value(&v, "user.missing").is_none());
    }

    #[test]
    fn csv_field_lookup() {
        let headers = vec!["name".to_string(), "user.email".to_string()];
        let row = csv::StringRecord::from(vec!["Ada", "a@example.com"]);
        assert_eq!(csv_field(&row, &headers, "name"), Some("Ada".to_string()));
        assert_eq!(
            csv_field(&row, &headers, "user.email"),
            Some("a@example.com".to_string())
        );
        assert_eq!(csv_field(&row, &headers, "2"), Some("a@example.com".to_string()));
    }
}
