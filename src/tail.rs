//! Passive collectors: read the usage numbers coding agents already write to disk.
//!
//! Claude Code appends one JSON object per line to
//! `~/.claude/projects/<slug>/<session>.jsonl`; assistant lines carry the model
//! and the token counts the API reported. Codex appends to
//! `~/.codex/sessions/<y>/<m>/<d>/rollout-*.jsonl`; `token_count` events carry
//! the last turn's usage. Reading these needs no proxy, no configuration and no
//! root, so a plain `mtop` sees sessions started in any other terminal.

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
/// A transcript written to this recently counts as an active session.
const ACTIVE: Duration = Duration::from_secs(120);
/// One line of a transcript. Longer is truncated rather than buffered.
const MAX_LINE: usize = 256 * 1024;
/// Request ids kept for duplicate suppression.
const MAX_SEEN: usize = 100_000;
/// Directory depth walked under a source root.
const MAX_DEPTH: usize = 6;

/// One tool whose transcripts are readable.
pub struct Source {
    pub name: &'static str,
    /// Directory under the home directory.
    pub dir: &'static str,
    parse: fn(&str, &Path, &mut FileState) -> Option<RequestMetric>,
}

pub const SOURCES: &[Source] = &[
    Source {
        name: "Claude Code",
        dir: ".claude/projects",
        parse: parse_claude,
    },
    Source {
        name: "Codex",
        dir: ".codex/sessions",
        parse: parse_codex,
    },
];

impl Source {
    pub fn path(&self) -> Option<PathBuf> {
        let dir = home()?.join(self.dir);
        dir.is_dir().then_some(dir)
    }
}

#[derive(Default)]
struct FileState {
    offset: u64,
    /// Codex names the model on a turn line, not on the usage line.
    model: String,
}

#[derive(Default)]
pub struct Tailer {
    files: HashMap<PathBuf, FileState>,
    seen: HashSet<u64>,
}

impl Tailer {
    /// Read every transcript once, push whatever is new into the store, and
    /// return how many transcripts were written to in the last two minutes.
    pub fn poll(&mut self, source: &Source, dir: &Path, store: &Shared) -> usize {
        let mut active = 0;
        for path in transcripts(dir) {
            let (metrics, is_active) = self.read_new(source, &path);
            active += usize::from(is_active);
            for metric in metrics {
                store.lock().unwrap().finish(metric);
            }
        }
        active
    }

    fn read_new(&mut self, source: &Source, path: &Path) -> (Vec<RequestMetric>, bool) {
        let Ok(mut file) = File::open(path) else {
            return (vec![], false);
        };
        let Ok(meta) = file.metadata() else {
            return (vec![], false);
        };
        let len = meta.len();
        let age = meta.modified().ok().and_then(|m| m.elapsed().ok());
        let active = age.is_some_and(|a| a < ACTIVE);
        let state = self.files.entry(path.into()).or_insert_with(|| FileState {
            // First sight of the file. An idle one starts at its end.
            offset: if age.is_some_and(|a| a < RECENT) {
                0
            } else {
                len
            },
            model: String::new(),
        });
        // Truncation or rotation: start over rather than read from the middle.
        if state.offset > len {
            state.offset = 0;
        }
        let start = state.offset;
        if file.seek(SeekFrom::Start(start)).is_err() {
            return (vec![], active);
        }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return (vec![], active);
        }
        // Lossy: a partial UTF-8 sequence at the tail must not drop the batch.
        let text = String::from_utf8_lossy(&bytes);

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
            if let Some(metric) = (source.parse)(line, path, state) {
                if self.seen.len() >= MAX_SEEN {
                    self.seen.clear();
                }
                if self.seen.insert(metric.id) {
                    out.push(metric);
                }
            }
        }
        state.offset = start + consumed as u64;
        (out, active)
    }
}

fn transcripts(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth < MAX_DEPTH {
                    walk(&path, depth + 1, out);
                }
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                out.push(path);
            }
        }
    }
    let mut out = vec![];
    walk(dir, 0, &mut out);
    out
}

/// The proxy numbers its requests from 1, so a hashed id must not land there.
/// The high bit marks a tailed request and keeps the two spaces apart.
fn id_for(key: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish() | (1 << 63)
}

/// One Claude Code assistant line, or `None` for every other line shape.
fn parse_claude(line: &str, _: &Path, _: &mut FileState) -> Option<RequestMetric> {
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
        id_for(&message["id"].as_str()?)
    } else {
        id_for(&request_id)
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

/// One Codex `token_count` event, or `None`. Turn lines only update the model.
fn parse_codex(line: &str, path: &Path, state: &mut FileState) -> Option<RequestMetric> {
    let v: Value = serde_json::from_str(line).ok()?;
    let payload = &v["payload"];
    if let Some(model) = payload["model"].as_str() {
        state.model = safe_label(model);
    }
    if payload["type"].as_str()? != "token_count" {
        return None;
    }
    let last = &payload["info"]["last_token_usage"];
    let input = last["input_tokens"].as_u64()?;
    // Codex reports cached tokens as a subset of input, like the OpenAI API.
    Some(RequestMetric {
        id: id_for(&(path, v["timestamp"].as_str().unwrap_or_default())),
        provider: "codex".into(),
        model: if state.model.is_empty() {
            "unknown".into()
        } else {
            state.model.clone()
        },
        status: "logged".into(),
        usage: Usage {
            input: Some(input),
            output: last["output_tokens"].as_u64(),
            cache_read: last["cached_input_tokens"].as_u64(),
            cache_write: None,
        },
        ..Default::default()
    })
}

/// One pass over every source whose directory exists, updating its status line.
pub fn poll_all(tailer: &mut Tailer, store: &Shared) {
    for (source, dir) in SOURCES.iter().filter_map(|s| s.path().map(|p| (s, p))) {
        let active = tailer.poll(source, &dir, store);
        store.lock().unwrap().source(
            source.name,
            format!("watching ~/{}, {active} active", source.dir),
        );
    }
}

/// Tail every source whose directory exists, once a second, forever.
pub async fn run(store: Shared) {
    let mut tailer = Tailer::default();
    loop {
        let mut t = std::mem::take(&mut tailer);
        let s = store.clone();
        // Directory walks and reads are blocking I/O; keep them off the runtime.
        if let Ok(t) = tokio::task::spawn_blocking(move || {
            poll_all(&mut t, &s);
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
    fn reads_claude_usage_and_ignores_other_lines() {
        let mut st = FileState::default();
        let p = Path::new("x");
        let m = parse_claude(LINE, p, &mut st).unwrap();
        assert_eq!(m.model, "claude-fable-5-1");
        assert_eq!(m.usage.input, Some(32));
        assert_eq!(m.usage.output, Some(208));
        assert_eq!(m.usage.cache_read, Some(60875));
        assert_eq!(m.usage.cache_write, Some(1228));
        assert_eq!(m.tool_calls, 1);
        assert_eq!(m.provider, "claude-code");
        assert!(m.id & (1 << 63) != 0, "must not collide with proxy ids");
        assert!(parse_claude(r#"{"type":"user","message":{}}"#, p, &mut st).is_none());
        assert!(
            parse_claude(r#"{"type":"assistant","message":{"usage":{}}}"#, p, &mut st).is_none()
        );
        assert!(parse_claude("not json", p, &mut st).is_none());
    }

    #[test]
    fn reads_codex_usage_with_model_from_turn_line() {
        let mut st = FileState::default();
        let p = Path::new("rollout.jsonl");
        assert!(
            parse_codex(
                r#"{"type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
                p,
                &mut st
            )
            .is_none()
        );
        let empty =
            r#"{"timestamp":"t0","type":"event_msg","payload":{"type":"token_count","info":null}}"#;
        assert!(parse_codex(empty, p, &mut st).is_none());
        let line = r#"{"timestamp":"t1","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":56830},"last_token_usage":{"input_tokens":19627,"cached_input_tokens":2432,"output_tokens":341,"reasoning_output_tokens":19}}}}"#;
        let m = parse_codex(line, p, &mut st).unwrap();
        assert_eq!(m.model, "gpt-5.5");
        assert_eq!(m.provider, "codex");
        assert_eq!(m.usage.input, Some(19627));
        assert_eq!(m.usage.cache_read, Some(2432));
        assert_eq!(m.usage.output, Some(341));
        let again = parse_codex(line, p, &mut st).unwrap();
        assert_eq!(
            m.id, again.id,
            "same file and timestamp is the same request"
        );
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
        let src = &SOURCES[0];
        assert_eq!(t.poll(src, &dir, &store), 1, "a fresh file is active");
        assert_eq!(store.lock().unwrap().completed, 1);

        // A second poll with no new bytes reports nothing.
        t.poll(src, &dir, &store);
        assert_eq!(store.lock().unwrap().completed, 1);

        // A half-written line waits for its newline.
        let mut f = File::options().append(true).open(&path).unwrap();
        let second = LINE.replace("req_A", "req_B");
        write!(f, "{}", &second[..20]).unwrap();
        t.poll(src, &dir, &store);
        assert_eq!(store.lock().unwrap().completed, 1);
        writeln!(f, "{}", &second[20..]).unwrap();
        t.poll(src, &dir, &store);
        assert_eq!(store.lock().unwrap().completed, 2);

        // The same request id twice is one request.
        writeln!(f, "{second}").unwrap();
        t.poll(src, &dir, &store);
        assert_eq!(store.lock().unwrap().completed, 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
