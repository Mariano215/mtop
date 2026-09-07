use clap::Parser;
use mtop::{
    model::{Backend, Price, RequestMetric, Store, Usage},
    poller,
    proxy::{Proxy, Timeouts},
    ui,
};
use std::{io::IsTerminal, net::SocketAddr, path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(
    version,
    about = "Local model telemetry and an opt-in streaming API proxy"
)]
struct Args {
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
    /// Upstream origin, for example https://api.anthropic.com (no /v1 suffix).
    #[arg(long)]
    upstream: Option<String>,
    #[arg(long, default_value = "127.0.0.1:8088")]
    listen: SocketAddr,
    #[arg(long, default_value = "openai", value_parser = ["openai", "anthropic", "ollama"])]
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    anyhow::ensure!(
        args.listen.ip().is_loopback(),
        "proxy listener must be a loopback address"
    );
    anyhow::ensure!(
        !(args.once && args.upstream.is_some()),
        "--once cannot run an upstream proxy"
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
    let server = if !args.demo {
        if let Some(upstream) = args.upstream {
            let timeouts = Timeouts {
                body: Duration::from_secs(args.body_timeout as u64),
                upstream: Duration::from_secs(args.request_timeout as u64),
            };
            let router =
                Proxy::new(store.clone(), &upstream, &args.provider, prices, timeouts)?.router();
            let listener = tokio::net::TcpListener::bind(args.listen).await?;
            Some(tokio::spawn(
                async move { axum::serve(listener, router).await },
            ))
        } else {
            None
        }
    } else {
        None
    };
    let result = tokio::task::spawn_blocking(move || ui::run(store, args.demo)).await?;
    if let Some(server) = server {
        server.abort();
    }
    result
}
