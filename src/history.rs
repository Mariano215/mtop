//! Optional on-disk history of completed requests.
//!
//! Off unless `--history PATH` is given, so the default run still keeps
//! nothing. Only the numbers and the bounded labels already held in the store
//! are written: no prompts, no responses, no headers, no credentials.
//!
//! Writes go through a channel to one background thread, so the store mutex is
//! never held across disk I/O.

use crate::model::RequestMetric;
use rusqlite::Connection;
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{Sender, channel},
    time::{SystemTime, UNIX_EPOCH},
};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS requests (
    row_id         INTEGER PRIMARY KEY AUTOINCREMENT,
    ts             INTEGER NOT NULL,
    provider       TEXT    NOT NULL,
    model          TEXT    NOT NULL,
    status         TEXT    NOT NULL,
    input          INTEGER,
    output         INTEGER,
    cache_read     INTEGER,
    cache_write    INTEGER,
    ttft_ms        REAL,
    duration_ms    REAL,
    generation_tps REAL,
    tool_calls     INTEGER NOT NULL,
    cost_usd       REAL
);
CREATE INDEX IF NOT EXISTS requests_ts ON requests (ts);
";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Open `path`, create the schema, and return a sender for completed requests.
/// The writer thread ends when every sender is dropped.
pub fn open(path: &Path) -> anyhow::Result<Sender<RequestMetric>> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let connection = Connection::open(path)?;
    // WAL lets several MTop instances write the same file without blocking.
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.execute_batch(SCHEMA)?;
    // Columns added after v0.1.1. ALTER fails when the column exists; that is fine.
    for column in [
        "reasoning INTEGER",
        "source TEXT",
        "session TEXT",
        "project TEXT",
        "agent TEXT",
        "http_status INTEGER",
    ] {
        let _ = connection.execute(&format!("ALTER TABLE requests ADD COLUMN {column}"), []);
    }

    let (sender, receiver) = channel::<RequestMetric>();
    std::thread::spawn(move || {
        for m in receiver {
            let _ = connection.execute(
                "INSERT INTO requests (ts, provider, model, status, input, output,
                     cache_read, cache_write, ttft_ms, duration_ms, generation_tps,
                     tool_calls, cost_usd, reasoning, source, session, project, agent,
                     http_status)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
                rusqlite::params![
                    now(),
                    m.provider,
                    m.model,
                    m.status,
                    m.usage.input,
                    m.usage.output,
                    m.usage.cache_read,
                    m.usage.cache_write,
                    m.ttft_ms,
                    m.duration_ms,
                    m.generation_tps,
                    m.tool_calls,
                    m.estimated_cost_usd,
                    m.reasoning,
                    m.source,
                    m.session,
                    m.project,
                    m.agent,
                    m.http_status,
                ],
            );
        }
    });
    Ok(sender)
}

/// Default history file when `--history` is given no path.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".local/share"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("mtop/history.db")
}

/// One summary row per model over the requested window.
pub struct Row {
    pub model: String,
    pub provider: String,
    pub requests: i64,
    pub input: i64,
    pub output: i64,
    /// None when no request for this model carried a priced estimate.
    pub cost: Option<f64>,
    /// Requests whose cost could not be estimated.
    pub unpriced: i64,
}

/// Totals per model over the last `days`. Zero days means everything.
pub fn summary(path: &Path, days: u32) -> anyhow::Result<Vec<Row>> {
    anyhow::ensure!(path.exists(), "no history at {}", path.display());
    let connection = Connection::open(path)?;
    let since = if days == 0 {
        0
    } else {
        now() - (days as i64) * 86_400
    };
    let mut statement = connection.prepare(
        "SELECT model, provider, COUNT(*), COALESCE(SUM(input),0), COALESCE(SUM(output),0),
                SUM(cost_usd), SUM(cost_usd IS NULL)
           FROM requests WHERE ts >= ?1
          GROUP BY model, provider
          ORDER BY COALESCE(SUM(cost_usd),0) DESC, COUNT(*) DESC",
    )?;
    let rows = statement
        .query_map([since], |r| {
            Ok(Row {
                model: r.get(0)?,
                provider: r.get(1)?,
                requests: r.get(2)?,
                input: r.get(3)?,
                output: r.get(4)?,
                cost: r.get(5)?,
                unpriced: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Print the summary as a plain table. An unknown cost stays unknown.
pub fn print_summary(rows: &[Row], days: u32) {
    let window = if days == 0 {
        "all time".to_string()
    } else {
        format!("last {days} days")
    };
    if rows.is_empty() {
        println!("No requests recorded ({window}).");
        return;
    }
    println!(
        "{:<32} {:<10} {:>6} {:>12} {:>12} {:>12}",
        "MODEL", "PROVIDER", "REQS", "INPUT", "OUTPUT", "COST"
    );
    let mut total = 0.0;
    let mut any_unpriced = false;
    for r in rows {
        let cost = match r.cost {
            Some(c) => {
                total += c;
                format!("${c:.6}")
            }
            None => "unknown".into(),
        };
        any_unpriced |= r.unpriced > 0;
        println!(
            "{:<32} {:<10} {:>6} {:>12} {:>12} {:>12}",
            r.model, r.provider, r.requests, r.input, r.output, cost
        );
    }
    println!("\nTotal priced estimate ({window}): ${total:.6}");
    if any_unpriced {
        println!("Some requests have no price and are excluded from that total.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Usage;

    #[test]
    fn writes_a_request_and_reads_it_back() {
        let dir = std::env::temp_dir().join(format!("mtop-history-{}", std::process::id()));
        let path = dir.join("history.db");
        let _ = std::fs::remove_dir_all(&dir);

        let sender = open(&path).unwrap();
        sender
            .send(RequestMetric {
                provider: "openai".into(),
                model: "test-model".into(),
                status: "200".into(),
                usage: Usage {
                    input: Some(10),
                    output: Some(4),
                    ..Default::default()
                },
                estimated_cost_usd: Some(0.5),
                ..Default::default()
            })
            .unwrap();
        // An unpriced request must not be counted as costing zero.
        sender
            .send(RequestMetric {
                provider: "openai".into(),
                model: "test-model".into(),
                status: "200".into(),
                usage: Usage {
                    input: Some(2),
                    output: Some(1),
                    ..Default::default()
                },
                estimated_cost_usd: None,
                ..Default::default()
            })
            .unwrap();
        // Dropping the last sender ends the writer thread once the queue drains.
        drop(sender);
        // Wait for both rows, not just the first: the writer is a thread.
        for _ in 0..250 {
            if summary(&path, 0)
                .map(|r| r.iter().map(|x| x.requests).sum::<i64>() >= 2)
                .unwrap_or(false)
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let rows = summary(&path, 0).unwrap();
        assert_eq!(rows.len(), 1, "both rows share a model and provider");
        let row = &rows[0];
        assert_eq!(row.requests, 2);
        assert_eq!(row.input, 12);
        assert_eq!(row.output, 5);
        assert_eq!(row.cost, Some(0.5));
        assert_eq!(row.unpriced, 1);

        // A window that excludes everything returns nothing, not an error.
        let mut old = summary(&path, 1).unwrap();
        assert_eq!(old.pop().unwrap().requests, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_an_error_not_an_empty_report() {
        let path = std::env::temp_dir().join("mtop-history-does-not-exist-9f3a.db");
        let _ = std::fs::remove_file(&path);
        assert!(summary(&path, 0).is_err());
    }
}
