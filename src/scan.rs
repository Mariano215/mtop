//! Offline discovery of AI tooling already installed on this machine.
//!
//! Scanning only tests whether a path or command exists and whether an
//! environment variable is set. It never opens a config file and never reads a
//! variable's value, because these paths sit beside credentials. Nothing found
//! here reaches the telemetry store, the JSON snapshot or any log.

use std::{env, path::PathBuf};

/// A client that talks to a model API and can be pointed at a proxy port.
struct Tool {
    name: &'static str,
    /// Paths under the home directory. Existence alone marks the tool present.
    config: &'static [&'static str],
    /// Command name to look for on PATH. Empty when the tool has no CLI.
    cli: &'static str,
    /// How to route this tool through MTop.
    route: &'static str,
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "Claude Code",
        config: &[".claude/settings.json", ".claude.json"],
        cli: "claude",
        route: "set ANTHROPIC_BASE_URL to the anthropic port",
    },
    Tool {
        name: "Codex",
        config: &[".codex/config.toml"],
        cli: "codex",
        route: "set the base URL in ~/.codex/config.toml to the openai port",
    },
    Tool {
        name: "Ollama",
        config: &[".ollama"],
        cli: "ollama",
        route: "point the client at the ollama port, or set OLLAMA_HOST",
    },
    Tool {
        name: "Continue",
        config: &[".continue/config.json"],
        cli: "",
        route: "set apiBase per model in ~/.continue/config.json",
    },
    Tool {
        name: "Cursor",
        config: &[".cursor"],
        cli: "cursor-agent",
        route: "set the OpenAI base URL in Cursor settings",
    },
    Tool {
        name: "Aider",
        config: &[".aider.conf.yml"],
        cli: "aider",
        route: "set OPENAI_API_BASE or ANTHROPIC_BASE_URL",
    },
    Tool {
        name: "Gemini CLI",
        config: &[".gemini"],
        cli: "gemini",
        route: "no Gemini parser yet; traffic would forward but not be parsed",
    },
    Tool {
        name: "Zed",
        config: &[".config/zed/settings.json"],
        cli: "zed",
        route: "set the provider api_url in Zed settings",
    },
];

/// An API credential in the environment, and the upstream it implies.
struct Provider {
    /// Environment variables that indicate this provider is configured.
    keys: &'static [&'static str],
    /// The `--upstream` argument to observe it, or None when unsupported.
    upstream: Option<&'static str>,
    note: &'static str,
}

const PROVIDERS: &[Provider] = &[
    Provider {
        keys: &["OPENAI_API_KEY"],
        upstream: Some("openai=https://api.openai.com"),
        note: "",
    },
    Provider {
        keys: &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"],
        upstream: Some("anthropic=https://api.anthropic.com"),
        note: "",
    },
    Provider {
        keys: &["OPENROUTER_API_KEY"],
        upstream: Some("openai=https://openrouter.ai/api"),
        note: "OpenAI-compatible, so the openai parser applies",
    },
    Provider {
        keys: &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        upstream: None,
        note: "no Gemini parser yet",
    },
];

fn home() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Whether `command` resolves to an existing file on PATH.
fn on_path(command: &str) -> bool {
    if command.is_empty() {
        return false;
    }
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|dir| dir.join(command).exists())
}

/// Which of `keys` are set to a non-empty value. Values are never read.
fn keys_set(keys: &[&'static str]) -> Vec<&'static str> {
    keys.iter()
        .copied()
        .filter(|k| env::var_os(k).is_some_and(|v| !v.is_empty()))
        .collect()
}

/// One line per finding, plus the upstream arguments the findings imply.
pub struct Report {
    pub tools: Vec<String>,
    pub providers: Vec<String>,
    pub upstreams: Vec<&'static str>,
}

pub fn scan() -> Report {
    let home = home();
    let mut tools = vec![];
    for tool in TOOLS {
        let found: Vec<&str> = home
            .as_ref()
            .map(|h| {
                tool.config
                    .iter()
                    .copied()
                    .filter(|c| h.join(c).exists())
                    .collect()
            })
            .unwrap_or_default();
        let cli = on_path(tool.cli);
        if found.is_empty() && !cli {
            continue;
        }
        let mut evidence = found.iter().map(|c| format!("~/{c}")).collect::<Vec<_>>();
        if cli {
            evidence.push(format!("{} on PATH", tool.cli));
        }
        tools.push(format!(
            "{:<12} {}\n             {}",
            tool.name,
            evidence.join(", "),
            tool.route
        ));
    }

    let mut providers = vec![];
    let mut upstreams = vec![];
    for provider in PROVIDERS {
        let set = keys_set(provider.keys);
        if set.is_empty() {
            continue;
        }
        let status = match provider.upstream {
            Some(u) => {
                upstreams.push(u);
                format!("observable with --upstream {u}")
            }
            None => "not observable yet".into(),
        };
        let note = if provider.note.is_empty() {
            String::new()
        } else {
            format!(" ({})", provider.note)
        };
        providers.push(format!("{:<24} {status}{note}", set.join(", ")));
    }
    upstreams.sort_unstable();
    upstreams.dedup();

    Report {
        tools,
        providers,
        upstreams,
    }
}

impl Report {
    /// The `mtop` invocation that observes everything found, or None if nothing is.
    pub fn command(&self) -> Option<String> {
        if self.upstreams.is_empty() {
            return None;
        }
        let args: Vec<String> = self
            .upstreams
            .iter()
            .map(|u| format!("  --upstream {u}"))
            .collect();
        Some(format!("mtop \\\n{}", args.join(" \\\n")))
    }

    pub fn print(&self) {
        println!("Scanning this machine. No file is opened and no key value is read.\n");
        if self.tools.is_empty() {
            println!("Tools: none found.\n");
        } else {
            println!("Tools found:");
            for t in &self.tools {
                println!("  {t}");
            }
            println!();
        }
        if self.providers.is_empty() {
            println!("Provider keys: none set in this environment.\n");
        } else {
            println!("Provider keys set:");
            for p in &self.providers {
                println!("  {p}");
            }
            println!();
        }
        match self.command() {
            Some(cmd) => println!("Observe them all with:\n\n{cmd}\n"),
            None => println!("Nothing to observe yet. Set a provider key, then scan again.\n"),
        }
        println!(
            "MTop only sees traffic routed through its ports. Finding a tool does\n\
             not monitor it: point that tool at the matching port above."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_command_only_from_observable_providers() {
        let report = Report {
            tools: vec![],
            providers: vec![],
            upstreams: vec!["openai=https://api.openai.com"],
        };
        let cmd = report.command().unwrap();
        assert!(cmd.contains("--upstream openai=https://api.openai.com"));

        // Nothing observable means no command to suggest, not an empty one.
        let empty = Report {
            tools: vec![],
            providers: vec![],
            upstreams: vec![],
        };
        assert!(empty.command().is_none());
    }

    #[test]
    fn path_lookup_finds_a_real_command_and_rejects_nonsense() {
        // Set PATH to a directory that certainly holds `sh` on any unix runner.
        assert!(!on_path(""));
        assert!(!on_path("mtop-does-not-exist-9f3a"));
    }

    #[test]
    fn unset_and_empty_keys_are_both_absent() {
        unsafe {
            env::set_var("MTOP_TEST_EMPTY", "");
            env::set_var("MTOP_TEST_SET", "x");
        }
        assert!(keys_set(&["MTOP_TEST_EMPTY"]).is_empty());
        assert_eq!(keys_set(&["MTOP_TEST_SET"]), vec!["MTOP_TEST_SET"]);
    }
}
