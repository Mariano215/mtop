use clap::{Parser, Subcommand};
use mtop::{
    model::{Backend, Price, RequestMetric, Store, Usage},
    poller,
    proxy::{Proxy, Timeouts},
    scan, ui,
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
    #[arg(long)]
    vllm: Option<String>,
    /// Upstream to observe. Repeat for several. Either a bare URL, which uses
    /// --provider, or PROVIDER=URL, for example anthropic=https://api.anthropic.com.
    /// Give the origin only, with no /v1 suffix. Each upstream gets its own
    /// loopback port, starting at --listen and counting up in the order given.
    #[arg(long)]
    upstream: Vec<String>,
    #[arg(long, default_value = "127.0.0.1:8088")]
    listen: SocketAddr,
    /// Parser for any --upstream given as a bare URL.
    #[arg(long, default_value = "openai", value_parser = PROVIDERS)]
    provider: String,
    /// JSON array of exact model IDs and user-supplied prices per million tokens.
    #[arg(long)]
    prices: Option<PathBuf>,
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u16).range(1..=10000))]
    capacity: u16,
    /// Seconds allowed for the whole upstream exchange, including a long streamed response.
    #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u32).range(1..=86400))]
    request_timeout: u32,
    /// Seconds allowed to read the client request body before forwarding starts.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=86400))]
    body_timeout: u32,
    /// Metrics-only is always enabled in v0.1; accepted for explicit invocation.
    #[arg(long)]
    metrics_only: bool,
}

#[derive(Subcommand)]
enum Command {
    /// List AI tools and provider keys found on this machine, and the command
    /// that observes them. Reads no file contents and no key values.
    Scan,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if let Some(Command::Scan) = args.command {
        scan::scan().print();
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
    let mut servers = vec![];
    if !args.demo {
        for (offset, spec) in args.upstream.iter().enumerate() {
            let (provider, url) = split_upstream(spec, &args.provider);
            let mut addr = args.listen;
            addr.set_port(args.listen.port() + offset as u16);
            let router =
                Proxy::new(store.clone(), url, provider, prices.clone(), timeouts)?.router();
            let listener = tokio::net::TcpListener::bind(addr).await?;
            store
                .lock()
                .unwrap()
                .listeners
                .push(format!("{provider} {url} -> http://{addr}"));
            servers.push(tokio::spawn(
                async move { axum::serve(listener, router).await },
            ));
        }
    }
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
