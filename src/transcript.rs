use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The newest assistant message is at the end of the file by definition,
/// so a small tail always reaches it.
const TAIL_BYTES: u64 = 64 * 1024;

pub fn last_assistant_timestamp_ms(path: &str) -> Option<i64> {
    let root = crate::schema::home_dir()?.join(".claude");
    last_assistant_ts_under(path, &root)
}

/// transcript_path is external input; require the resolved real path to
/// live under the allowed root so a hostile payload cannot make us read
/// arbitrary files.
pub fn last_assistant_ts_under(path: &str, allowed_root: &Path) -> Option<i64> {
    let real = Path::new(path).canonicalize().ok()?;
    let root = allowed_root.canonicalize().ok()?;
    if !real.starts_with(&root) {
        return None;
    }
    let mut f = std::fs::File::open(&real).ok()?;
    let size = f.metadata().ok()?.len();
    if size == 0 {
        return None;
    }
    let start = size.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if start > 0 && !buf.starts_with(b"\n") && !lines.is_empty() {
        lines.remove(0); // partial first line when the tail starts mid-line
    }
    for line in lines.iter().rev() {
        // Cheap substring pre-filter before json parsing each line.
        if !line.contains("\"assistant\"") || !line.contains("\"timestamp\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let is_assistant = v.pointer("/message/role").and_then(|r| r.as_str()) == Some("assistant")
            || v.get("role").and_then(|r| r.as_str()) == Some("assistant");
        if !is_assistant {
            continue;
        }
        if let Some(ts) = v.get("timestamp").and_then(|t| t.as_str()).and_then(parse_iso8601_ms) {
            return Some(ts);
        }
    }
    None
}

/// Parse "YYYY-MM-DDTHH:MM:SS(.fff...)Z" (or "+00:00") to epoch ms.
/// Other offsets are rejected; Claude Code always emits UTC.
pub fn parse_iso8601_ms(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z').or_else(|| s.strip_suffix("+00:00")).unwrap_or(s);
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { s.get(r)?.parse().ok() };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let millis: i64 = match b.get(19) {
        Some(b'.') => {
            let frac: String = s[20..].chars().take_while(|c| c.is_ascii_digit()).collect();
            if frac.is_empty() {
                return None;
            }
            format!("{frac:0<3}")[..3].parse().ok()?
        }
        None => 0,
        Some(_) => return None,
    };
    Some((days_from_civil(y, mo, d) * 86_400 + h * 3_600 + mi * 60 + sec) * 1_000 + millis)
}

/// Days since 1970-01-01 for a proleptic Gregorian date
/// (Howard Hinnant's civil-days algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_vectors() {
        assert_eq!(parse_iso8601_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601_ms("2026-07-02T23:00:49.920Z"), Some(1_783_033_249_920));
        assert_eq!(parse_iso8601_ms("2026-01-01T00:00:00+00:00"), Some(1_767_225_600_000));
    }

    #[test]
    fn iso8601_rejects_garbage() {
        assert_eq!(parse_iso8601_ms(""), None);
        assert_eq!(parse_iso8601_ms("not a date"), None);
        assert_eq!(parse_iso8601_ms("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_iso8601_ms("2026-07-02 23:00:49Z"), None);
        assert_eq!(parse_iso8601_ms("2026-07-02T23:00:49+02:00"), None);
    }

    fn write_transcript(dir: &Path, name: &str, lines: &[&str]) -> String {
        let path = dir.join(name);
        std::fs::write(&path, lines.join("\n")).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn newest_assistant_timestamp_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(dir.path(), "s.jsonl", &[
            r#"{"type":"assistant","timestamp":"2026-07-02T22:00:00Z","message":{"role":"assistant"}}"#,
            r#"{"type":"user","timestamp":"2026-07-02T22:30:00Z","message":{"role":"user"}}"#,
            r#"{"type":"assistant","timestamp":"2026-07-02T23:00:49.920Z","message":{"role":"assistant"}}"#,
        ]);
        assert_eq!(last_assistant_ts_under(&path, dir.path()), Some(1_783_033_249_920));
    }

    #[test]
    fn outer_role_fallback_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(dir.path(), "s.jsonl", &[
            r#"{"role":"assistant","timestamp":"1970-01-01T00:00:00Z"}"#,
        ]);
        assert_eq!(last_assistant_ts_under(&path, dir.path()), Some(0));
    }

    #[test]
    fn no_assistant_message_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(dir.path(), "s.jsonl", &[
            r#"{"type":"user","timestamp":"2026-07-02T22:30:00Z","message":{"role":"user"}}"#,
        ]);
        assert_eq!(last_assistant_ts_under(&path, dir.path()), None);
    }

    #[test]
    fn empty_missing_and_garbage_files_are_none() {
        let dir = tempfile::tempdir().unwrap();
        let empty = write_transcript(dir.path(), "e.jsonl", &[]);
        assert_eq!(last_assistant_ts_under(&empty, dir.path()), None);
        assert_eq!(last_assistant_ts_under("/does/not/exist", dir.path()), None);
        let garbage = write_transcript(dir.path(), "g.jsonl", &["{broken", "also broken"]);
        assert_eq!(last_assistant_ts_under(&garbage, dir.path()), None);
    }

    #[test]
    fn path_outside_allowed_root_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let path = write_transcript(other.path(), "s.jsonl", &[
            r#"{"role":"assistant","timestamp":"1970-01-01T00:00:00Z"}"#,
        ]);
        assert_eq!(last_assistant_ts_under(&path, dir.path()), None);
    }

    #[test]
    fn tail_window_skips_partial_first_line() {
        let dir = tempfile::tempdir().unwrap();
        // File bigger than the 64 KiB tail: pad with long user lines, put
        // the assistant entry at the end.
        let pad = format!(r#"{{"type":"user","filler":"{}"}}"#, "x".repeat(1000));
        let mut lines: Vec<String> = (0..100).map(|_| pad.clone()).collect();
        lines.push(r#"{"role":"assistant","timestamp":"1970-01-01T00:00:00Z"}"#.to_string());
        let path = dir.path().join("big.jsonl");
        std::fs::write(&path, lines.join("\n")).unwrap();
        let p = path.to_string_lossy().into_owned();
        assert_eq!(last_assistant_ts_under(&p, dir.path()), Some(0));
    }
}
