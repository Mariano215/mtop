//! `mtop setup`: turn on each tool's own telemetry export, pointed at MTop.
//!
//! These are the only places MTop ever writes outside its own data directory,
//! and each write is shown first, confirmed, and preceded by a backup copy.
//! The files sit beside credentials, so only the named keys are touched and
//! nothing else in them is read back or printed.

use crate::scan::home;
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::{
    io::{IsTerminal, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
};

/// The `env` keys Claude Code reads. Prompts and responses stay redacted:
/// OTEL_LOG_USER_PROMPTS and OTEL_LOG_ASSISTANT_RESPONSES are never set.
const CLAUDE_ENV: [&str; 5] = [
    "CLAUDE_CODE_ENABLE_TELEMETRY",
    "OTEL_METRICS_EXPORTER",
    "OTEL_LOGS_EXPORTER",
    "OTEL_EXPORTER_OTLP_PROTOCOL",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
];

const CODEX_BEGIN: &str =
    "# mtop-begin: written by `mtop setup`; `mtop setup --remove` deletes this block";
const CODEX_END: &str = "# mtop-end";

pub struct Step {
    pub tool: &'static str,
    pub path: PathBuf,
    /// The file after the change, or None when there is nothing to change.
    next: Option<String>,
    pub summary: String,
}

/// Plan every change for the given receiver address without touching disk.
pub fn plan(otlp: SocketAddr, remove: bool) -> Result<Vec<Step>> {
    let home = home().context("no home directory")?;
    let base = format!("http://{otlp}");
    Ok(vec![
        claude(&home.join(".claude").join("settings.json"), &base, remove)?,
        codex(&home.join(".codex").join("config.toml"), &base, remove)?,
        gemini(&home.join(".gemini").join("settings.json"), &base, remove)?,
    ])
}

fn read_json(path: &Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let text = std::fs::read_to_string(path)?;
    match serde_json::from_str::<Value>(&text)? {
        Value::Object(m) => Ok(m),
        _ => bail!("{} is not a JSON object", path.display()),
    }
}

fn claude(path: &Path, base: &str, remove: bool) -> Result<Step> {
    let mut root = read_json(path)?;
    let env = root
        .entry("env")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("settings.json \"env\" is not an object")?;
    let before = env.clone();
    if remove {
        for k in CLAUDE_ENV {
            env.remove(k);
        }
    } else {
        env.insert(CLAUDE_ENV[0].into(), json!("1"));
        env.insert(CLAUDE_ENV[1].into(), json!("otlp"));
        env.insert(CLAUDE_ENV[2].into(), json!("otlp"));
        env.insert(CLAUDE_ENV[3].into(), json!("http/json"));
        env.insert(CLAUDE_ENV[4].into(), json!(base));
    }
    let changed = *env != before;
    let summary = if !changed {
        "already as requested".into()
    } else if remove {
        format!("remove {} from \"env\"", CLAUDE_ENV.join(", "))
    } else {
        let old = before
            .get(CLAUDE_ENV[4])
            .and_then(Value::as_str)
            .map(|e| format!(" (currently {e})"))
            .unwrap_or_default();
        format!("set \"env\" telemetry keys, endpoint {base}{old}")
    };
    if env.is_empty() {
        root.remove("env");
    }
    Ok(Step {
        tool: "Claude Code",
        path: path.into(),
        next: changed.then(|| serde_json::to_string_pretty(&Value::Object(root)).unwrap() + "\n"),
        summary,
    })
}

fn codex(path: &Path, base: &str, remove: bool) -> Result<Step> {
    let text = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };
    let block = format!(
        "\n{CODEX_BEGIN}\n[otel]\nexporter = {{ otlp-http = {{ endpoint = \"{base}/v1/logs\", protocol = \"json\" }} }}\n{CODEX_END}\n"
    );
    let stripped = match (text.find(CODEX_BEGIN), text.find(CODEX_END)) {
        (Some(b), Some(e)) if e > b => {
            let end = e + CODEX_END.len();
            let end = text[end..]
                .find('\n')
                .map(|i| end + i + 1)
                .unwrap_or(text.len());
            let start = if b > 0 && text.as_bytes()[b - 1] == b'\n' {
                b - 1
            } else {
                b
            };
            format!("{}{}", &text[..start], &text[end..])
        }
        _ => text.clone(),
    };
    let (next, summary) = if remove {
        (stripped, "delete the mtop [otel] block".to_string())
    } else if stripped.contains("[otel]") {
        bail!(
            "{} already has an [otel] section that mtop did not write; edit it by hand to point at {base}/v1/logs",
            path.display()
        );
    } else {
        (
            format!("{stripped}{block}"),
            format!("append an [otel] block exporting logs to {base}/v1/logs"),
        )
    };
    let changed = next != text;
    Ok(Step {
        tool: "Codex",
        path: path.into(),
        next: changed.then_some(next),
        summary: if changed {
            summary
        } else {
            "already as requested".into()
        },
    })
}

fn gemini(path: &Path, base: &str, remove: bool) -> Result<Step> {
    let mut root = read_json(path)?;
    let before = root.get("telemetry").cloned();
    if remove {
        if before.as_ref().is_some_and(|t| t["otlpEndpoint"] == base) {
            root.remove("telemetry");
        }
    } else {
        root.insert(
            "telemetry".into(),
            json!({"enabled": true, "target": "local", "otlpEndpoint": base, "otlpProtocol": "http"}),
        );
    }
    let changed = root.get("telemetry") != before.as_ref();
    let summary = if !changed {
        "already as requested".into()
    } else if remove {
        "remove the \"telemetry\" block".into()
    } else {
        let old = before
            .as_ref()
            .and_then(|t| t["otlpEndpoint"].as_str())
            .map(|e| format!(" (currently {e})"))
            .unwrap_or_default();
        format!("set \"telemetry\" to a local OTLP target at {base}{old}")
    };
    Ok(Step {
        tool: "Gemini CLI",
        path: path.into(),
        next: changed.then(|| serde_json::to_string_pretty(&Value::Object(root)).unwrap() + "\n"),
        summary,
    })
}

/// Show every step, ask once, then back up and write each one. `yes` skips the question.
pub fn apply(steps: Vec<Step>, yes: bool) -> Result<()> {
    for step in &steps {
        println!(
            "{:<12} {}\n             {}",
            step.tool,
            step.path.display(),
            step.summary
        );
    }
    let pending: Vec<Step> = steps.into_iter().filter(|s| s.next.is_some()).collect();
    if pending.is_empty() {
        println!("\nnothing to write");
        return Ok(());
    }
    if !yes {
        anyhow::ensure!(
            std::io::stdin().is_terminal(),
            "not a terminal: pass --yes to apply without asking"
        );
        print!(
            "\nwrite {} file{}? [y/N] ",
            pending.len(),
            if pending.len() == 1 { "" } else { "s" }
        );
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("nothing written");
            return Ok(());
        }
    }
    for step in pending {
        let next = step.next.unwrap_or_default();
        if let Some(parent) = step.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let existed = step.path.exists();
        if existed {
            let backup = step.path.with_extension(format!(
                "{}.mtop.bak",
                step.path.extension().and_then(|e| e.to_str()).unwrap_or("")
            ));
            std::fs::copy(&step.path, &backup)?;
            println!("{:<12} backup {}", step.tool, backup.display());
        }
        write_atomic(&step.path, &next, existed)?;
        println!("{:<12} written {}", step.tool, step.path.display());
    }
    Ok(())
}

/// Write beside the target, sync, then rename over it, so a crash mid-write
/// leaves either the old file or the new one, never a torn one. These files
/// sit beside credentials: a new one is owner-only, like the tools create
/// it, and an existing one keeps its own bits.
fn write_atomic(path: &Path, text: &str, existed: bool) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}.mtop.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if existed {
            std::fs::metadata(path)?.permissions().mode()
        } else {
            0o600
        };
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_env_merge_keeps_other_keys_and_reverts() {
        let dir = std::env::temp_dir().join(format!("mtop-setup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"model":"x","env":{"KEEP":"1"}}"#).unwrap();
        let base = "http://127.0.0.1:4318";
        let step = claude(&path, base, false).unwrap();
        let next = step.next.unwrap();
        let v: Value = serde_json::from_str(&next).unwrap();
        assert_eq!(v["model"], "x");
        assert_eq!(v["env"]["KEEP"], "1");
        assert_eq!(v["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"], base);
        assert_eq!(v["env"]["OTEL_EXPORTER_OTLP_PROTOCOL"], "http/json");
        assert!(v["env"].get("OTEL_LOG_USER_PROMPTS").is_none());
        write_atomic(&path, &next, true).unwrap();
        assert!(!path.with_extension("json.mtop.tmp").exists());
        assert!(
            claude(&path, base, false).unwrap().next.is_none(),
            "idempotent"
        );
        let reverted = claude(&path, base, true).unwrap().next.unwrap();
        let v: Value = serde_json::from_str(&reverted).unwrap();
        assert_eq!(v["env"], json!({"KEEP":"1"}));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn codex_block_appends_once_and_strips_cleanly() {
        let dir = std::env::temp_dir().join(format!("mtop-setup-codex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "model = \"gpt-5.5\"\n").unwrap();
        let base = "http://127.0.0.1:4318";
        let next = codex(&path, base, false).unwrap().next.unwrap();
        assert!(next.contains("[otel]"));
        assert!(next.contains("http://127.0.0.1:4318/v1/logs"));
        std::fs::write(&path, &next).unwrap();
        assert!(
            codex(&path, base, false).unwrap().next.is_none(),
            "idempotent"
        );
        let reverted = codex(&path, base, true).unwrap().next.unwrap();
        assert_eq!(reverted, "model = \"gpt-5.5\"\n");
        // A foreign [otel] section is never overwritten.
        std::fs::write(&path, "[otel]\nexporter = \"none\"\n").unwrap();
        assert!(codex(&path, base, false).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
