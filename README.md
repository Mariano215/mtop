```
╔════════════════════════════════════════════════════════════════════╗
║                                                                    ║
║               ███╗   ███╗████████╗ ██████╗ ██████╗                 ║
║               ████╗ ████║╚══██╔══╝██╔═══██╗██╔══██╗                ║
║               ██╔████╔██║   ██║   ██║   ██║██████╔╝                ║
║               ██║╚██╔╝██║   ██║   ██║   ██║██╔═══╝                 ║
║               ██║ ╚═╝ ██║   ██║   ╚██████╔╝██║                     ║
║               ╚═╝     ╚═╝   ╚═╝    ╚═════╝ ╚═╝                     ║
║                                                                    ║
║   M O D E L   T E L E M E T R Y   C O N S O L E                    ║
╠════════════════════════════════════════════════════════════════════╣
║  v0.1  ·  RUST + RATATUI  ·  LOOPBACK ONLY  ·  NO PERSISTENCE      ║
╚════════════════════════════════════════════════════════════════════╝
```

# MTop

[![CI](https://github.com/Mariano215/mtop/actions/workflows/ci.yml/badge.svg)](https://github.com/Mariano215/mtop/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://rustup.rs/)

A terminal console for local model telemetry and opt-in API request
observation. Think `htop`, but for the model calls your machine is making.

**v0.1 is a tested starter implementation, not a universal passive AI monitor.**
Read [Explicit limits](#explicit-limits) before you rely on it.

## Why

Model API spend is invisible until the invoice arrives, and the tools making
those calls (Claude Code, Codex, Cursor, Aider, a local Ollama) each report
their own usage differently, or not at all. MTop sits in one place and answers
three questions:

- What models is this machine actually calling, and how often?
- How fast do they respond, and how many tokens do they burn?
- What is that costing, with unknowns shown as unknown rather than zero?

It does this without a kernel module, without root, and without sending
anything anywhere. `mtop scan` finds the tools you already have,
`mtop run -- <cmd>` routes one of them through MTop with no config editing,
and `--history` keeps the numbers so you can look back.

**How it sees a tool.** Three doors, and each tool has one or more:

1. **Telemetry.** Claude Code, Codex and Gemini CLI can export their own usage
   over OpenTelemetry. `mtop setup` turns that on, pointed at MTop. This is the
   supported path and the most complete: the tool reports its own token
   counts, timings and (for Claude Code) cost. See [Setup](#setup).
2. **Transcripts.** Claude Code and Codex also write usage to disk. A plain
   `mtop` reads those files with no setup at all. See
   [Claude Code and Codex without a proxy](#claude-code-and-codex-without-a-proxy).
3. **Proxy.** Anything that accepts a base URL can be pointed at MTop with
   `mtop run -- <cmd>`. This is the only door for key-based tools like Aider.

**What it is not.** It cannot see network traffic from a process that uses
none of the doors. There is no eBPF and no packet sniffing. A tool that talks
to its vendor's own backend, like Cursor or the desktop chat apps, is not
observable from your machine by MTop or by anything else. That is a deliberate scope choice, not a missing feature: see
[the specification](docs/SPEC.md) for the reasoning.

## Install

Prebuilt binaries for Linux (x86_64, aarch64), macOS (arm64, x86_64) and
Windows (x86_64) are attached to each
[release](https://github.com/Mariano215/mtop/releases), each with a SHA-256
file. On Arch, build from `packaging/aur/PKGBUILD`. Put the binary on your
PATH, then:

```sh
mtop --demo   # synthetic data, no network, to see the screen
mtop          # live: every tool found on this machine, and its calls
mtop setup    # one confirmed step to turn on Claude Code, Codex and Gemini CLI telemetry
```

Every `cargo run --locked --` example below is the same as `mtop` with the
binary installed.

## Build from source

Install the stable Rust toolchain using [rustup](https://rustup.rs/) if needed, then:

```sh
git clone https://github.com/Mariano215/mtop.git
cd mtop
cargo test --locked
cargo run --locked -- --demo
cargo run --locked -- --once
cargo run --locked
```

The normal mode polls Ollama at `http://127.0.0.1:11434`. An unavailable service is displayed explicitly.
`--demo` uses synthetic data and makes no network calls. `--once` emits JSON and exits.

## Keys

The dashboard reads these keys. There are no other bindings.

| Key | Action |
|-----|--------|
| `j` / `Down` | Move the selection down |
| `k` / `Up` | Move the selection up |
| `Space` | Freeze the display. Collection continues in the background |
| `q` / `Esc` | Quit and restore the terminal |
| `Ctrl+C` | Quit and restore the terminal |

## Options

| Flag | Default | Meaning |
|------|---------|---------|
| `scan` | | Subcommand. List installed AI tools and provider keys, then exit |
| `run -- <CMD>` | | Subcommand. Run `<CMD>` with its base URLs pointed at MTop |
| `history` | | Subcommand. Summarize recorded requests per model, then exit |
| `--history [PATH]` | off | Record completed requests to SQLite. No path means the default file |
| `--demo` | off | Synthetic data, no network calls |
| `--once` | off | Print one JSON snapshot and exit. Cannot run with `--upstream` |
| `--ollama <URL>` | `http://127.0.0.1:11434` | Ollama origin to poll |
| `--no-ollama` | off | Skip Ollama polling |
| `--no-tail` | off | Do not read Claude Code or Codex transcripts from the home directory |
| `--otlp <ADDR>` | `127.0.0.1:4318` | OpenTelemetry receiver address. Must be loopback |
| `--no-otlp` | off | Do not start the OpenTelemetry receiver |
| `setup [--remove] [--yes]` | | Subcommand. Write each tool's telemetry export config, pointed at `--otlp` |
| `--vllm <URL>` | none | vLLM origin for server-level metrics |
| `--upstream <SPEC>` | none | Turn on a proxy listener. Repeatable. `URL` or `PROVIDER=URL`. No `/v1` suffix |
| `--listen <ADDR>` | `127.0.0.1:8088` | First proxy port. Must be loopback. Later upstreams count up from here |
| `--provider <NAME>` | `openai` | Parser for any `--upstream` given as a bare URL: `openai`, `anthropic` or `ollama` |
| `--prices <FILE>` | none | JSON price table, see [Prices](#prices) |
| `--capacity <N>` | `1000` | Retained request rows, 1 to 10000 |
| `--request-timeout <SECONDS>` | `600` | Whole upstream exchange, 1 to 86400 |
| `--body-timeout <SECONDS>` | `30` | Client body intake, 1 to 86400 |
| `--metrics-only` | always on | Accepted for explicit invocation. v0.1 has no other mode |

## Find what to observe

```sh
cargo run --locked -- scan
```

`scan` lists the AI tools and provider keys on this machine, then prints the
exact `mtop` command that observes them. It tests only whether a path or
command exists and whether a variable is set. It never opens a config file and
never reads a key's value, because those paths sit beside credentials. Nothing
it finds reaches the store, the JSON snapshot or any log.

Finding a tool does not monitor it. You still have to point that tool at the
matching port.

## Setup

```sh
mtop setup
```

For each of Claude Code, Codex and Gemini CLI, `setup` prints the file and the
change, asks, copies the file to `<file>.mtop.bak`, then writes. Nothing is
written without a `y`, or `--yes`. `mtop setup --remove` takes the same keys
out again. Only the named keys are touched; the rest of each file is kept as
is and never printed.

| Tool | File | Change |
|------|------|--------|
| Claude Code | `~/.claude/settings.json` | five `env` keys: enable telemetry, OTLP exporters, `http/json`, endpoint |
| Codex | `~/.codex/config.toml` | an `[otel]` block between `# mtop-begin` and `# mtop-end` markers |
| Gemini CLI | `~/.gemini/settings.json` | a `telemetry` block with a local OTLP target |

Then run `mtop` and use the tool in any other terminal. The tool pushes each
completed API call to `http://127.0.0.1:4318/v1/logs`, and the request table
shows it with status `telemetry`. Prompt and response text stay redacted:
`setup` never sets `OTEL_LOG_USER_PROMPTS` or its equivalents, and the
receiver reads only the model name and the numeric fields of three events
(`claude_code.api_request`, `codex.sse_event`, `gemini_cli.api_response`).
Metrics and traces posts are accepted and discarded.

If a tool already exports somewhere else, `setup` says so ("currently
http://...") before asking, and a Codex `[otel]` section MTop did not write is
never overwritten. The receiver binds loopback only and caps bodies at 4 MiB.

## Claude Code and Codex without a proxy

Claude Code appends every assistant turn, with the model name and the token
counts the API reported, to a transcript under `~/.claude/projects/`. Codex
appends a `token_count` event per turn under `~/.codex/sessions/`. A plain
`mtop` reads both and shows each turn as a `claude-code` or `codex` request,
so a session started in any other terminal appears with no routing and no
configuration, whether you logged in with a subscription or a key.
Transcripts idle for more than an hour are skipped until they grow again.

The dashboard header lists every tool `scan` found and what MTop is doing
about it: `watching` with a count of transcripts written to in the last two
minutes, or `installed` with the routing step that would observe it.

Only the numbers and the model name are taken. Prompts and responses in the
same lines are never retained. Use `--no-tail` to turn this off.

## Run a tool through MTop

`run` starts the listeners, points the child process at them, runs it, and
reports what it used. Nothing global changes: the variables are set for that
one process only.

```sh
# Use whatever `scan` found.
cargo run --locked -- run -- claude

# Or name the upstreams yourself.
cargo run --locked -- run --upstream ollama=http://127.0.0.1:11434 -- ollama run qwen3:0.6b
```

Variables set per provider: `openai` sets `OPENAI_BASE_URL` and
`OPENAI_API_BASE` with the `/v1` suffix; `anthropic` sets
`ANTHROPIC_BASE_URL`; `ollama` sets `OLLAMA_HOST`. MTop prints each one it
sets. The child keeps your terminal, so there is no dashboard in this mode.
The exit code is the child's.

## Keep history

Off by default: a plain run still stores nothing on disk. Add `--history` to
record completed requests to SQLite, then read them back later.

```sh
# Record to the default file, ~/.local/share/mtop/history.db.
cargo run --locked -- --history run -- claude -p 'say ok'

# Or name the file.
cargo run --locked -- --history /tmp/mtop.db --upstream ollama=http://127.0.0.1:11434

# Summarize per model. --days 0 means everything.
cargo run --locked -- history --days 30
```

Only the numbers and bounded labels already held in memory are written:
timestamp, provider, model, status, token counts, timings, tool count and any
cost estimate. No prompts, no responses, no headers and no credentials. A
request with no price stays unknown in the report and is excluded from the
total, never counted as zero. Writes go through one background thread, so the
store lock is never held across disk I/O, and the file uses WAL so several
MTop instances can record to it at once.

## Observe requests

Start MTop with one explicitly configured upstream origin. Keep `/v1` in the client URL, not the upstream origin:

```sh
# Observe Ollama requests, including its final token/timing statistics.
cargo run --locked -- --upstream http://127.0.0.1:11434 --provider ollama

# In another terminal, replace YOUR_INSTALLED_MODEL with an installed model name.
curl http://127.0.0.1:8088/api/generate -d '{"model":"YOUR_INSTALLED_MODEL","prompt":"Say hello"}'

# Observe an OpenAI-compatible provider. Configure your client's base URL
# to http://127.0.0.1:8088/v1 and keep its existing API-key configuration.
cargo run --locked -- --upstream https://api.openai.com --provider openai

# Anthropic Messages: configure a compatible client to use this proxy origin.
cargo run --locked -- --upstream https://api.anthropic.com --provider anthropic

# Optional vLLM server-level metrics.
cargo run --locked -- --vllm http://127.0.0.1:8000
```

### Several providers at once

Repeat `--upstream`. Each one gets its own port, counting up from `--listen`
in the order you write them. The running dashboard lists the ports at the top,
so you can read them off while configuring a client.

```sh
cargo run --locked -- \
  --upstream openai=https://api.openai.com \
  --upstream anthropic=https://api.anthropic.com \
  --upstream ollama=http://127.0.0.1:11434
# openai     -> http://127.0.0.1:8088
# anthropic  -> http://127.0.0.1:8089
# ollama     -> http://127.0.0.1:8090
```

OpenRouter and other OpenAI-compatible services use the `openai` parser:

```sh
cargo run --locked -- --upstream openai=https://openrouter.ai/api
```

Only clients routed through the proxy are observed. No request modification enables extra usage fields:
configure usage reporting in the client if supported. Missing usage is shown as unavailable.
API credentials pass through to the fixed upstream; MTop does not log or retain them in telemetry.
The listener is loopback-only. Do not expose it with port forwarding: it has no client authentication.

## Implemented

- Ratatui dashboard; selection, display freeze, clean terminal restoration, offline demo and JSON snapshot.
- Ollama loaded-model/VRAM polling; vLLM queue and cache gauges (maximum cache fraction across labels).
- Streaming proxy with unchanged response bytes, status and end-to-end headers; redirects disabled.
- Bounded SSE/NDJSON/JSON parsing for OpenAI Chat Completions, a subset of Responses events, Anthropic Messages and Ollama.
- Reported token/cache usage, first visible streamed-text latency, Ollama server generation speed, and tool-call counts.
- Optional exact-model pricing. Unknown usage or missing cache rates produce an unknown estimate, never a fabricated zero.
- 1,000 retained request rows by default; session totals survive row eviction.
- Linux/macOS CI workflow and protocol, state, UI and local mock-proxy tests.

## Explicit limits

No eBPF capture, PID attribution, full agent execution trees, tool arguments, prompt inspector, process termination,
Gemini parser on the proxy door (Gemini CLI is observed through `mtop setup` telemetry only), dedicated llama.cpp
collector, cost-rate chart or automatic pricing catalog yet.
Responses normalization is partial; tool counts are capped at 1,024 unique calls per request.
TTFT means time from forwarding start to first visible text event. Nonstreaming TTFT is unavailable.
No tokenization estimates are made. Polling cannot recover per-request usage.

Request bodies are capped at 4 MiB; parser lines/events at 256 KiB; upstream concurrency at 16;
backend response bodies at 2 MiB; retained model names at 120 characters.
Oversized parser records increase `Parse`; forwarding continues. Large prompts exceeding the request limit receive HTTP 413.
Requests time out after 600 seconds (body intake: 30 seconds). Change both with `--request-timeout <SECONDS>`
and `--body-timeout <SECONDS>`, each accepting 1 to 86400.
Only numeric metadata and bounded model/provider/status labels remain in the store. Parsing transiently touches plaintext;
this is not secure memory erasure or protection from OS swap/core dumps. Nothing is written to disk and no analytics
service is enabled unless `--history` is given, which records only those same numeric fields and bounded labels.
Closing MTop also closes its active proxy connections.

## Prices

Use `--prices path/to/prices.json`. The file contains a JSON array:

```json
[
  {
    "model": "YOUR_EXACT_MODEL_ID",
    "input_per_million": 2.0,
    "output_per_million": 8.0,
    "cache_read_per_million": 0.2,
    "cache_write_per_million": 2.5
  }
]
```

These numbers are examples, not provider prices. Supply rates applicable to your account and model.
Estimates exclude taxes, service/tool charges, tiered context rates, special cache TTL pricing, discounts and subscriptions.
Anthropic input excludes separately reported cache tokens; OpenAI cached tokens are a subset of input tokens.

## More

- [The refined specification](docs/SPEC.md)
- [The Mac/Codex handoff](docs/HANDOFF.md)
- [Validation notes](docs/VALIDATION.md)

## Status and roadmap

v0.1 works and is tested on Linux, macOS and Windows. Known gaps, roughly in
the order they matter:

- OpenAI Responses normalization is partial, so some reasoning-model usage
  reads as unknown when the API did report it.
- No Gemini parser on the proxy. Gemini CLI reports through `mtop setup`
  telemetry; a raw Gemini API client pointed at the proxy forwards but is not
  parsed.
- Pricing is manual. There is no bundled catalog yet, so cost is unknown
  until you supply a `--prices` file.
- No live totals while `run` has a child attached; the summary comes at exit.

## Contributing

Issues and pull requests are welcome. Before opening a PR:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

CI runs exactly those three on Linux, macOS and Windows, so a green local run
is a green CI run. Non-trivial logic should arrive with a test that fails
without the change.

Two rules specific to this project. Never report a number the provider did not
send: an unknown token count or missing price stays unknown and is excluded
from totals, never defaulted to zero. And never widen what MTop retains:
only numeric metadata and bounded labels reach the store, the JSON snapshot
or the history file.

## License

MIT. See [LICENSE](LICENSE). Copyright (c) 2026 Mariano Mattei.
