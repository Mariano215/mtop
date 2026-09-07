use clap::{Parser, Subcommand};
use mtop::{
    history,
    model::{Backend, Price, RequestMetric, Store, Usage},
    poller,
    proxy::{Proxy, Timeouts},
    scan, tail, ui,
};
use std::{io::IsTerminal, net::SocketAddr, path::PathBuf, time::Duration};

const PROVIDERS: [&str; 3] = ["openai", "anthropic", "ollama"];

/// Split PROVIDER=URL. A bare URL, or a prefix that is not a known provider,
/// falls back to the --provider default.
fn split_upstream<'a>(spec: &'a str, default: &'a str) -> (&'a str, &'a str) {
    match spec.split_once('=') {
        Some((p, url)) if PROVIDERS.contains(&p) => (p, url),
        _ => (default, spec),
    }
}

/// Base-URL variables that point a child process at one proxy listener.
/// OpenAI clients expect the /v1 suffix in the base URL; Anthropic and Ollama
/// clients append their own paths, so those get the bare origin.
fn env_for(provider: &str, addr: SocketAddr) -> Vec<(&'static str, String)> {
    let origin = format!("http://{addr}");
    match provider {
        "openai" => vec![
            ("OPENAI_BASE_URL", format!("{origin}/v1")),
            ("OPENAI_API_BASE", format!("{origin}/v1")),
        ],
        "anthropic" => vec![("ANTHROPIC_BASE_URL", origin)],
        "ollama" => vec![("OLLAMA_HOST", origin)],
        _ => vec![],
    }
}

#[derive(Parser)]
#[command(
    version,
    about = "Local model telemetry and an opt-in streaming API proxy"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// Run without network access, showing synthetic data.
    #[arg(long)]
    demo: bool,
    /// Emit one JSON snapshot, then exit. Does not start the proxy.
    #[arg(long)]
    once: bool,
    #[arg(long, default_value = "http://127.0.0.1:11434")]
    ollama: String,
    #[arg(long)]
    no_ollama: bool,
    /// Do not read Claude Code or Codex transcripts from the home directory.
    #[arg(long)]
    no_tail: bool,
    #[arg(long)]
    vllm: Option<String>,
    /// Upstream to observe. Repeat for several. Either a bare URL, which uses
    /// --provider, or PROVIDER=URL, for example anthropic=https://api.anthropic.com.
    /// Give the origin only, with no /v1 suffix. Each upstream gets its own
    /// loopback port, starting at --listen and counting up in the order given.
    #[arg(long, global = true)]
    upstream: Vec<String>,
    #[arg(long, default_value = "127.0.0.1:8088", global = true)]
    listen: SocketAddr,
    /// Parser for any --upstream given as a bare URL.
    #[arg(long, default_value = "openai", value_parser = PROVIDERS, global = true)]
    provider: String,
    /// JSON array of exact model IDs and user-supplied prices per million tokens.
    #[arg(long, global = true)]
    prices: Option<PathBuf>,
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u16).range(1..=10000), global = true)]
    capacity: u16,
    /// Seconds allowed for the whole upstream exchange, including a long streamed response.
    #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u32).range(1..=86400), global = true)]
    request_timeout: u32,
    /// Seconds allowed to read the client request body before forwarding starts.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=86400), global = true)]
    body_timeout: u32,
    /// Record completed requests to a SQLite file. Off unless given. With no
    /// path, uses the default under XDG_DATA_HOME or ~/.local/share.
    /// Only numbers and bounded labels are written: no prompts, no credentials.
    #[arg(long, global = true, num_args = 0..=1, default_missing_value = "")]
    history: Option<PathBuf>,
    /// Metrics-only is always enabled in v0.1; accepted for explicit invocation.
    #[arg(long)]
    metrics_only: bool,
}

#[derive(Subcommand)]
enum Command {
    /// List AI tools and provider keys found on this machine, and the command
    /// that observes them. Reads no file contents and no key values.
    Scan,
    /// Summarize recorded requests per model. Needs an earlier run with --history.
    History {
        /// Days back to include. 0 means everything.
        #[arg(long, default_value_t = 30)]
        days: u32,
    },
    /// Run a command with its base URLs pointed at MTop, then report what it
    /// used. Without --upstream, the upstreams come from `scan`.
    Run {
        /// The command to run, and its arguments.
        #[arg(trailing_var_arg = true, required = true)]
        argv: Vec<String>,
    },
}

/// Bind one loopback listener per upstream, counting up from `listen`.
/// Returns the server tasks and the provider/address pairs actually bound.
async fn start_proxies(
    store: &mtop::model::Shared,
    upstreams: &[String],
    default_provider: &str,
    listen: SocketAddr,
    prices: &[Price],
    timeouts: Timeouts,
) -> anyhow::Result<(Vec<tokio::task::JoinHandle<()>>, Vec<(String, SocketAddr)>)> {
    let mut servers = vec![];
    let mut bound = vec![];
    for (offset, spec) in upstreams.iter().enumerate() {
        let (provider, url) = split_upstream(spec, default_provider);
        let mut addr = listen;
        addr.set_port(listen.port() + offset as u16);
        let router = Proxy::new(store.clone(), url, provider, prices.to_vec(), timeouts)?.router();
        let listener = tokio::net::TcpListener::bind(addr).await?;
        store
            .lock()
            .unwrap()
            .listeners
            .push(format!("{provider} {url} -> http://{addr}"));
        servers.push(tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        }));
        bound.push((provider.to_string(), addr));
    }
    Ok((servers, bound))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if let Some(Command::Scan) = args.command {
        scan::scan().print();
        return Ok(());
    }
    // An empty --history value means "use the default path".
    let history_path = args.history.as_ref().map(|p| {
        if p.as_os_str().is_empty() {
            history::default_path()
        } else {
            p.clone()
        }
    });
    if let Some(Command::History { days }) = args.command {
        let path = history_path.unwrap_or_else(history::default_path);
        history::print_summary(&history::summary(&path, days)?, days);
        return Ok(());
    }
    anyhow::ensure!(
        args.listen.ip().is_loopback(),
        "proxy listener must be a loopback address"
    );
    anyhow::ensure!(
        !(args.once && !args.upstream.is_empty()),
        "--once cannot run an upstream proxy"
    );
    anyhow::ensure!(
        args.listen
            .port()
            .checked_add(args.upstream.len() as u16)
            .is_some(),
        "not enough ports above --listen for {} upstreams",
        args.upstream.len()
    );
    let prices: Vec<Price> = if let Some(path) = args.prices {
        serde_json::from_slice(&std::fs::read(path)?)?
    } else {
        vec![]
    };
    for p in &prices {
        anyhow::ensure!(
            [
                Some(p.input_per_million),
                Some(p.output_per_million),
                p.cache_read_per_million,
                p.cache_write_per_million
            ]
            .into_iter()
            .flatten()
            .all(|v| v.is_finite() && v >= 0.),
            "prices must be finite and nonnegative"
        );
    }
    let store = Store::shared(args.capacity as usize);
    if let Some(path) = &history_path {
        store.lock().unwrap().history = Some(history::open(path)?);
    }
    if let Some(Command::Run { argv }) = &args.command {
        let timeouts = Timeouts {
            body: Duration::from_secs(args.body_timeout as u64),
            upstream: Duration::from_secs(args.request_timeout as u64),
        };
        // With no --upstream, take whatever `scan` can prove is configured.
        let discovered: Vec<String>;
        let upstreams = if args.upstream.is_empty() {
            discovered = scan::scan()
                .upstreams
                .into_iter()
                .map(String::from)
                .collect();
            &discovered
        } else {
            &args.upstream
        };
        anyhow::ensure!(
            !upstreams.is_empty(),
            "nothing to observe: pass --upstream, or set a provider key and check `mtop scan`"
        );
        let (servers, bound) = start_proxies(
            &store,
            upstreams,
            &args.provider,
            args.listen,
            &prices,
            timeouts,
        )
        .await?;

        let mut child = tokio::process::Command::new(&argv[0]);
        child.args(&argv[1..]);
        for (provider, addr) in &bound {
            for (key, value) in env_for(provider, *addr) {
                eprintln!("mtop: {key}={value}");
                child.env(key, value);
            }
        }
        let status = child
            .spawn()
            .map_err(|e| anyhow::anyhow!("cannot run {}: {e}", argv[0]))?
            .wait()
            .await?;
        for server in servers {
            server.abort();
        }
        let s = store.lock().unwrap();
        eprintln!(
            "\nmtop: observed {} request{}, {} priced (${:.6}), {} unpriced",
            s.completed,
            if s.completed == 1 { "" } else { "s" },
            s.completed.saturating_sub(s.unpriced),
            s.known_cost_usd,
            s.unpriced
        );
        std::process::exit(status.code().unwrap_or(1));
    }
    if !args.demo {
        // Every tool found gets a line. The tailer overwrites the ones it reads.
        let mut s = store.lock().unwrap();
        for (name, route) in scan::scan().found {
            s.source(name, format!("installed; {route}"));
        }
    }
    if args.demo {
        let mut s = store.lock().unwrap();
        s.backends.push(Backend {
            source: "ollama".into(),
            model: "demo-local-model".into(),
            status: "synthetic".into(),
            vram_bytes: Some(8 * 1024 * 1024 * 1024),
            ..Default::default()
        });
        s.finish(RequestMetric {
            id: 1,
            provider: "demo".into(),
            model: "demo-cloud-model".into(),
            status: "synthetic".into(),
            usage: Usage {
                input: Some(8420),
                output: Some(412),
                ..Default::default()
            },
            ttft_ms: Some(320.),
            tool_calls: 2,
            ..Default::default()
        });
    } else if args.once {
        let client = poller::client()?;
        for (source, base) in [
            (!args.no_ollama).then_some(("ollama", args.ollama.as_str())),
            args.vllm.as_deref().map(|v| ("vllm", v)),
        ]
        .into_iter()
        .flatten()
        {
            let rows = poller::once(&client, source, base)
                .await
                .unwrap_or_else(|_| {
                    vec![Backend {
                        source: source.into(),
                        status: "unavailable".into(),
                        ..Default::default()
                    }]
                });
            store.lock().unwrap().backend(rows, source);
        }
        if !args.no_tail {
            tail::poll_all(&mut tail::Tailer::default(), &store);
        }
    } else {
        anyhow::ensure!(
            std::io::stdout().is_terminal(),
            "TUI requires a terminal; use --once for JSON"
        );
        if !args.no_ollama {
            tokio::spawn(poller::run(store.clone(), "ollama", args.ollama));
        }
        if let Some(base) = args.vllm {
            tokio::spawn(poller::run(store.clone(), "vllm", base));
        }
        if !args.no_tail {
            tokio::spawn(tail::run(store.clone()));
        }
    }
    if args.once {
        println!("{}", serde_json::to_string_pretty(&*store.lock().unwrap())?);
        return Ok(());
    }
    anyhow::ensure!(
        std::io::stdout().is_terminal(),
        "TUI requires a terminal; use --demo --once for JSON"
    );
    let timeouts = Timeouts {
        body: Duration::from_secs(args.body_timeout as u64),
        upstream: Duration::from_secs(args.request_timeout as u64),
    };
    let servers = if args.demo {
        vec![]
    } else {
        start_proxies(
            &store,
            &args.upstream,
            &args.provider,
            args.listen,
            &prices,
            timeouts,
        )
        .await?
        .0
    };
    let result = tokio::task::spawn_blocking(move || ui::run(store, args.demo)).await?;
    for server in servers {
        server.abort();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::split_upstream;

    #[test]
    fn base_url_variables_match_each_client_convention() {
        let addr: std::net::SocketAddr = "127.0.0.1:8088".parse().unwrap();
        // OpenAI clients want /v1 in the base URL; the other two append their own paths.
        assert_eq!(
            super::env_for("openai", addr),
            vec![
                ("OPENAI_BASE_URL", "http://127.0.0.1:8088/v1".to_string()),
                ("OPENAI_API_BASE", "http://127.0.0.1:8088/v1".to_string()),
            ]
        );
        assert_eq!(
            super::env_for("anthropic", addr),
            vec![("ANTHROPIC_BASE_URL", "http://127.0.0.1:8088".to_string())]
        );
        assert_eq!(
            super::env_for("ollama", addr),
            vec![("OLLAMA_HOST", "http://127.0.0.1:8088".to_string())]
        );
        assert!(super::env_for("gemini", addr).is_empty());
    }

    #[test]
    fn splits_provider_prefix_and_leaves_bare_urls_alone() {
        assert_eq!(
            split_upstream("anthropic=https://api.anthropic.com", "openai"),
            ("anthropic", "https://api.anthropic.com")
        );
        // A bare URL falls back to --provider.
        assert_eq!(
            split_upstream("https://api.openai.com", "openai"),
            ("openai", "https://api.openai.com")
        );
        // An unknown prefix is not a provider, so the whole spec stays the URL.
        assert_eq!(
            split_upstream("https://x.test/?a=b", "ollama"),
            ("ollama", "https://x.test/?a=b")
        );
    }
}
