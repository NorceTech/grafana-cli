use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputMode {
    Json,
    Table,
}

pub fn resolve_output(flag: Option<OutputMode>, is_tty: bool) -> OutputMode {
    flag.unwrap_or(if is_tty {
        OutputMode::Table
    } else {
        OutputMode::Json
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRecord {
    pub profile: String,
    pub datasource: String,
    pub ts: String,
    pub labels: BTreeMap<String, String>,
    pub line: String,
}

pub fn write_jsonl(out: &mut impl Write, records: &[LogRecord]) -> std::io::Result<()> {
    for record in records {
        writeln!(
            out,
            "{}",
            serde_json::to_string(record).map_err(std::io::Error::other)?
        )?;
    }
    Ok(())
}

pub fn render_table(headers: &[String], rows: &[Vec<String>]) -> String {
    let mut table = comfy_table::Table::new();
    let header: Vec<&str> = headers.iter().map(String::as_str).collect();
    table.set_header(header);
    for row in rows {
        let cells: Vec<&str> = row.iter().map(String::as_str).collect();
        table.add_row(cells);
    }
    table.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_mode_resolution() {
        assert_eq!(resolve_output(None, true), OutputMode::Table);
        assert_eq!(resolve_output(None, false), OutputMode::Json);
        assert_eq!(
            resolve_output(Some(OutputMode::Json), true),
            OutputMode::Json
        );
        assert_eq!(
            resolve_output(Some(OutputMode::Table), false),
            OutputMode::Table
        );
    }

    fn rec(line: &str) -> LogRecord {
        LogRecord {
            profile: "prod".into(),
            datasource: "loki-main".into(),
            ts: "2026-01-01T00:00:00Z".into(),
            labels: BTreeMap::from([("job".into(), "api".into())]),
            line: line.into(),
        }
    }

    #[test]
    fn jsonl_one_line_per_record() {
        let records = vec![rec("hello"), rec("world")];
        let mut buf = Vec::new();
        write_jsonl(&mut buf, &records).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["profile"], "prod");
        assert_eq!(v["datasource"], "loki-main");
        assert_eq!(v["ts"], "2026-01-01T00:00:00Z");
        assert_eq!(v["labels"]["job"], "api");
        assert_eq!(v["line"], "hello");
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn jsonl_empty_is_no_output() {
        let mut buf = Vec::new();
        write_jsonl(&mut buf, &[]).unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn log_record_serde_roundtrip() {
        let r = rec("line");
        let s = serde_json::to_string(&r).unwrap();
        let back: LogRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn table_contains_headers_and_cells() {
        let out = render_table(
            &["profile".to_string(), "line".to_string()],
            &[
                vec!["prod".into(), "hello".into()],
                vec!["dev".into(), "world".into()],
            ],
        );
        assert!(out.contains("profile"));
        assert!(out.contains("line"));
        assert!(out.contains("hello"));
        assert!(out.contains("world"));
    }
}
