//! Passive collector: read the usage numbers Claude Code already writes to disk.
//!
//! Claude Code appends one JSON object per line to
//! `~/.claude/projects/<slug>/<session>.jsonl`. Assistant lines carry the model
//! name and the token counts the API reported. Reading them needs no proxy, no
//! configuration and no root, so a plain `mtop` sees Claude Code sessions that
//! were started in any other terminal.

use crate::model::{RequestMetric, Shared, Usage, safe_label};
use crate::scan::home;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    hash::{DefaultHasher, Hash, Hasher},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};

/// A transcript untouched for this long is history, not activity: skip its
/// existing contents and report only what is appended from now on.
const RECENT: Duration = Duration::from_secs(3600);
/// One line of a transcript. Longer is truncated rather than buffered.
const MAX_LINE: usize = 256 * 1024;
/// Request ids kept for duplicate suppression.
const MAX_SEEN: usize = 100_000;

pub fn claude_dir() -> Option<PathBuf> {
    let dir = home()?.join(".claude").join("projects");
    dir.is_dir().then_some(dir)
}

#[derive(Default)]
pub struct Tailer {
    offsets: HashMap<PathBuf, u64>,
    seen: HashSet<u64>,
}

impl Tailer {
    /// Read every transcript once and push whatever is new into the store.
    pub fn poll(&mut self, dir: &Path, store: &Shared) {
        for path in transcripts(dir) {
            for metric in self.read_new(&path) {
                store.lock().unwrap().finish(metric);
            }
        }
    }

    fn read_new(&mut self, path: &Path) -> Vec<RequestMetric> {
        let Ok(mut file) = File::open(path) else {
            return vec![];
        };
        let Ok(meta) = file.metadata() else {
            return vec![];
        };
        let len = meta.len();
        let start = match self.offsets.get(path) {
            Some(&offset) => offset,
            // First sight of the file. An idle one starts at its end.
            None if is_recent(&meta) => 0,
            None => len,
        };
        // Truncation or rotation: start over rather than read from the middle.
        let start = if start > len { 0 } else { start };
        if file.seek(SeekFrom::Start(start)).is_err() {
            return vec![];
        }
        let mut text = String::new();
        // Lossy: a partial UTF-8 sequence at the tail must not drop the batch.
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return vec![];
        }
        text.push_str(&String::from_utf8_lossy(&bytes));

        let mut consumed = 0usize;
        let mut out = vec![];
        for line in text.split_inclusive('\n') {
            if !line.ends_with('\n') {
                break; // A line still being written. Re-read it next poll.
            }
            consumed += line.len();
            if line.len() > MAX_LINE {
                continue;
            }
            if let Some(metric) = parse(line) {
                if self.seen.len() >= MAX_SEEN {
                    self.seen.clear();
                }
                if self.seen.insert(metric.id) {
                    out.push(metric);
                }
            }
        }
        self.offsets.insert(path.into(), start + consumed as u64);
        out
    }
}

fn is_recent(meta: &std::fs::Metadata) -> bool {
    meta.modified()
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age < RECENT)
}

fn transcripts(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let Ok(projects) = std::fs::read_dir(dir) else {
        return out;
    };
    for project in projects.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        out.extend(
            files
                .flatten()
                .map(|f| f.path())
                .filter(|p| p.extension().is_some_and(|e| e == "jsonl")),
        );
    }
    out
}

/// The proxy numbers its requests from 1, so a hashed id must not land there.
/// The high bit marks a tailed request and keeps the two spaces apart.
fn id_for(request_id: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    request_id.hash(&mut hasher);
    hasher.finish() | (1 << 63)
}

/// One assistant line, or `None` for every other line shape.
fn parse(line: &str) -> Option<RequestMetric> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v["type"].as_str()? != "assistant" {
        return None;
    }
    let message = &v["message"];
    let usage = &message["usage"];
    // No usage object means no numbers to report, so there is nothing to show.
    let input = usage["input_tokens"].as_u64()?;
    let request_id = v["requestId"].as_str().unwrap_or_default();
    let id = if request_id.is_empty() {
        id_for(message["id"].as_str()?)
    } else {
        id_for(request_id)
    };
    let tool_calls = message["content"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|c| c["type"] == "tool_use")
                .count()
                .min(1024) as u64
        })
        .unwrap_or(0);
    Some(RequestMetric {
        id,
        provider: "claude-code".into(),
        model: safe_label(message["model"].as_str().unwrap_or("unknown")),
        status: safe_label(message["stop_reason"].as_str().unwrap_or("logged")),
        usage: Usage {
            input: Some(input),
            output: usage["output_tokens"].as_u64(),
            cache_read: usage["cache_read_input_tokens"].as_u64(),
            cache_write: usage["cache_creation_input_tokens"].as_u64(),
        },
        tool_calls,
        ..Default::default()
    })
}

pub async fn run(store: Shared, dir: PathBuf) {
    let mut tailer = Tailer::default();
    loop {
        let mut t = std::mem::take(&mut tailer);
        let s = store.clone();
        let d = dir.clone();
        // Directory walks and reads are blocking I/O; keep them off the runtime.
        if let Ok(t) = tokio::task::spawn_blocking(move || {
            t.poll(&d, &s);
            t
        })
        .await
        {
            tailer = t;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const LINE: &str = r#"{"type":"assistant","requestId":"req_A","message":{"model":"claude-fable-5-1","id":"msg_1","stop_reason":"tool_use","content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"t1"}],"usage":{"input_tokens":32,"cache_creation_input_tokens":1228,"cache_read_input_tokens":60875,"output_tokens":208}}}"#;

    #[test]
    fn reads_usage_and_ignores_other_lines() {
        let m = parse(LINE).unwrap();
        assert_eq!(m.model, "claude-fable-5-1");
        assert_eq!(m.usage.input, Some(32));
        assert_eq!(m.usage.output, Some(208));
        assert_eq!(m.usage.cache_read, Some(60875));
        assert_eq!(m.usage.cache_write, Some(1228));
        assert_eq!(m.tool_calls, 1);
        assert_eq!(m.provider, "claude-code");
        assert!(m.id & (1 << 63) != 0, "must not collide with proxy ids");
        assert!(parse(r#"{"type":"user","message":{}}"#).is_none());
        assert!(parse(r#"{"type":"assistant","message":{"usage":{}}}"#).is_none());
        assert!(parse("not json").is_none());
    }

    #[test]
    fn tails_appends_once_and_holds_partial_lines() {
        let dir = std::env::temp_dir().join(format!("mtop-tail-{}", std::process::id()));
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join("s.jsonl");
        std::fs::write(&path, format!("{LINE}\n")).unwrap();

        let store = crate::model::Store::shared(10);
        let mut t = Tailer::default();
        t.poll(&dir, &store);
        assert_eq!(store.lock().unwrap().completed, 1);

        // A second poll with no new bytes reports nothing.
        t.poll(&dir, &store);
        assert_eq!(store.lock().unwrap().completed, 1);

        // A half-written line waits for its newline.
        let mut f = File::options().append(true).open(&path).unwrap();
        let second = LINE.replace("req_A", "req_B");
        write!(f, "{}", &second[..20]).unwrap();
        t.poll(&dir, &store);
        assert_eq!(store.lock().unwrap().completed, 1);
        writeln!(f, "{}", &second[20..]).unwrap();
        t.poll(&dir, &store);
        assert_eq!(store.lock().unwrap().completed, 2);

        // The same request id twice is one request.
        writeln!(f, "{second}").unwrap();
        t.poll(&dir, &store);
        assert_eq!(store.lock().unwrap().completed, 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
