# MTop in five minutes

MTop is a terminal screen for the AI coding tools on your machine: what is
installed, what is being watched, and every model call with its tokens,
timing and cost. This walk-through uses one Mac with Claude Code, Codex,
Ollama, Cursor and Gemini CLI installed. Every screen below is a real capture
from that machine. Your tools and numbers will differ; the shape will not.

## 1. Install

```sh
# macOS and Linux
curl -fsSL https://raw.githubusercontent.com/Mariano215/mtop/main/install.sh | sh
```

```powershell
# Windows
irm https://raw.githubusercontent.com/Mariano215/mtop/main/install.ps1 | iex
```

The script downloads the release binary for your machine, checks its SHA-256
against the published digest, and puts `mtop` on your PATH. It prints each
step. If it says to add a directory to your PATH, do that, then open a new
terminal.

```
mtop: checksum verified
mtop: installed v0.2.0 to /usr/local/bin/mtop
```

## 2. See the screen with no data

```sh
mtop --demo
```

One synthetic request and one synthetic backend, no network. The title says
`DEMO / SYNTHETIC` so it can never be mistaken for real numbers. Press `q` to
leave.

```text
┌ MTop 0.2.0 • DEMO / SYNTHETIC ─────────────────────────────────────────────────────────────────────────────────────────────────┐
│Completed 1  |  Priced estimate $0.020960  |  Unpriced 0  |  Evicted 0                                                          │
│Last 5 min: 1766 tokens/min  |  $0.2515/hour  |  0 tool calls  |  1 models  |  0 sessions                                       │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

The synthetic request is priced from the bundled table (below), using the
real `claude-sonnet-5` rate, so the number is a realistic example, not a
placeholder.

## 3. Find out what is on this machine

```sh
mtop scan
```

`scan` looks for each tool's config directory and command, then prints one
line per tool with what MTop does about it. It also lists provider keys set
in your shell, CLI versions, Claude Code's model and effort, and whether each
tool's telemetry export is on. It reads no key values and opens no file
except a few named settings keys.

```text
Tools found:
  Claude Code  ~/.claude/settings.json, ~/.claude.json, claude on PATH
             run `mtop setup`, or `mtop run -- claude`
  Codex        ~/.codex/config.toml, codex on PATH
             run `mtop setup`
  Ollama       ~/.ollama, ollama on PATH
             run `mtop run -- <your ollama client>`
  Cursor       ~/.cursor
             talks to Cursor's own backend; not observable locally
  Gemini CLI   ~/.gemini, gemini on PATH
             run `mtop setup`

Environment:
  Claude Code version          2.1.263 (Claude Code)
  Codex version                codex-cli 0.153.4
  Claude Code model            claude-fable-5-1[1m]
  Claude Code effort           medium
  Claude Code telemetry        off (run `mtop setup`)
  Codex telemetry              off (run `mtop setup`)
```

Three kinds of tool appear:

- **Claude Code and Codex** write transcripts to disk. MTop reads them with
  no setup at all, so the next step already shows their calls.
- **Tools with a telemetry export** (Claude Code, Codex, Gemini CLI) can push
  each call to MTop directly. That needs one setup step, next.
- **Cursor and the desktop chat apps** talk to their vendor's own servers.
  Nothing on your machine carries their numbers, so MTop says so and moves on.

## 4. Turn on telemetry, once

```sh
mtop setup
```

For each tool that has an export, `setup` prints the file it would change
and the exact change, then asks one question for the whole plan. Nothing is
written before you answer `y`. Each file is copied to `<file>.mtop.bak`
first, written through a temp file so a crash cannot leave it torn, and a
new file is created owner-only because these files sit beside credentials.

```text
Claude Code  ~/.claude/settings.json
             set "env" telemetry keys, endpoint http://127.0.0.1:4318
Codex        ~/.codex/config.toml
             append an [otel] block exporting logs to http://127.0.0.1:4318/v1/logs
Gemini CLI   ~/.gemini/settings.json
             set "telemetry" to a local OTLP target at http://127.0.0.1:4318

write 3 files? [y/N] y
Claude Code  backup ~/.claude/settings.json.mtop.bak
Claude Code  written ~/.claude/settings.json
Codex        backup ~/.codex/config.toml.mtop.bak
Codex        written ~/.codex/config.toml
Gemini CLI   written ~/.gemini/settings.json
```

If a tool already exports somewhere else, the line says `(currently
http://...)` so you know what you are replacing. `mtop setup --remove` puts
everything back. Prompt and response text are never exported: MTop does not
set the flags that would include them, and its receiver reads only the model
name and the numbers.

## 5. Watch

```sh
mtop
```

Now use Claude Code, Codex or Gemini CLI in any other terminal, the normal
way. This is the live screen on the demo machine a few seconds after
starting, before any new call was made: the last hour of Claude Code and
Codex transcripts is already on it.

```text
┌ MTop 0.2.0 • LIVE / METRICS ONLY ──────────────────────────────────────────────────────────────────────────────────────────────┐
│Completed 448  |  Priced estimate $9.184000  |  Unpriced 210 (model not in the bundled or --prices table)  |  Evicted 0         │
│Last 5 min: 0 tokens/min  |  — /hour  |  453 tool calls  |  6 models  |  25 sessions                                            │
│telemetry receiver http://127.0.0.1:4318 (mtop setup)                                                                           │
│Claude Code  watching ~/.claude/projects, 1 active                                                                              │
│Codex        watching ~/.codex/sessions, 0 active                                                                               │
│Ollama       installed; run `mtop run -- <your ollama client>`                                                                  │
│Continue     installed; set apiBase per model in ~/.continue/config.json                                                        │
│Cursor       installed; talks to Cursor's own backend; not observable locally                                                   │
│Gemini CLI   installed; run `mtop setup`                                                                                        │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
┌ Backends • polling does not observe individual requests • Tab for more ────────────────────────────────────────────────────────┐
│Source    Status        Model                                                            VRAM         Running  Waiting  KV max  │
│ollama    idle                                                                           —            —        —        —       │
│                                                                                                                                │
│                                                                                                                                │
│                                                                                                                                │
│                                                                                                                                │
│                                                                                                                                │
└──────────────────────────────────────────────────────────────────────────��─────────────────────────────────────────────────────
┌ Observed requests • sort: newest • Hit = cache read share • Ctx = context used • — means unavailable ──────────────────────────┐
│Provider    Model                    Status    Src  Dur ms   Input   Cache   Hit  Output  Reason  Tools HTTP                    │
│claude-code claude-opus-4-7          tool_use  main 2532.0   1       52038   97%  69      —       1     —                       │
│claude-code claude-opus-4-7          tool_use  main 17686.0  1       50285   92%  1679    1093    0     —                       │
│claude-code claude-opus-4-7          tool_use  main 53893.0  1       46401   92%  3755    3568    0     —                       │
│claude-code claude-opus-4-7          tool_use  main 2491.0   1       42570   80%  89      —       1     —                       │
│claude-code claude-opus-4-7          tool_use  main 5245.0   1       34258   85%  89      —       1     —                       │
│claude-code claude-opus-4-7          tool_use  main 2362.0   6       28983   74%  125     34      0     —                       │
│claude-code claude-sonnet-5          logged    suba 3098.0   2       65174   99%  1       —       0     —                       │
│claude-code claude-sonnet-5          logged    suba 1578.0   2       64366   97%  6       —       0     —                       │
│claude-code claude-sonnet-5          logged    suba 9763.0   2       62344   99%  2       —       0     —                       │
│claude-code claude-sonnet-5          logged    suba 1551.0   2       61461   93%  3       —       0     —                       │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
┌────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│↑/↓ j/k select • Enter detail • Tab panel: backends, tools, models, projects, sessions, environment • s sort • Space freeze • q │
└────────��───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
```

How to read it, top to bottom:

- **Header, line 1.** Requests seen, the priced estimate, and how many had no
  price. MTop ships a [bundled price table](../README.md#prices) for current
  Claude, GPT and Gemini models, applied by default; older or unlisted models
  stay unpriced. With nothing priced at all the estimate shows `—`, never a
  fake zero. `Evicted` turns red if the table dropped rows.
- **Header, line 2.** Tokens per minute and dollars per hour over the last
  five minutes, tool calls, models and sessions seen. History read at startup
  is not counted in the rate.
- **Tool lines.** Green is being watched right now, with the number of
  transcripts written to in the last two minutes. Yellow names the one step
  that would observe it. Gray cannot be observed.
- **Limit lines** (Codex). Percent of the rate limit used, the window, and
  when it resets. Yellow past 50 percent, red past 80.
- **Middle box.** Local backends by default: Ollama's loaded models and VRAM.
  `Tab` cycles it, next section.
- **Requests.** One row per API call, newest first. `Input` is fresh input,
  `Cache` is what came from the prompt cache, `Hit` is the cache share,
  `Ctx` is how full the model's context window is, `Reason` is reasoning
  tokens, `Dur ms` is the turn time. A column that no row can fill is not
  shown. Rows with an API error are red and carry the HTTP status.

## 6. Dig in

Press `Tab` to cycle the middle box.

**Tools.** Every tool the agents called, with calls, failures, average and
total time. The slow one and the failing one are at a glance.

```text
┌ Tools • calls, failures, average and total time ───────────────────────────────────────────────────────────────────────────────┐
│Tool                                                                                        Calls   Failed   Avg ms    Total s  │
│Bash                                                                                        287     6        8681.7    2491.6   │
│Read                                                                                        68      0        14.4      1.0      │
│Agent                                                                                       25      0        621.6     15.5     │
│SendMessage                                                                                 20      2        20.9      0.4      │
│ToolSearch                                                                                  16      0        10.8      0.2      │
│StructuredOutput                                                                            14      3        7.5       0.1      │
```

**Totals by model.** Then by project (working directory name), then by
session. This is where "which project is burning the tokens" lives.

```text
┌ Totals by model ───────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│Model                                                            Reqs   Input     Cache     Output    Reason   Cost       Errors│
│claude-fable-5-1                                                 171    4340      31760804  194494    29556    —          0     │
│claude-opus-5                                                    72     144       10383270  31694     7510     —          0     │
│claude-sonnet-5                                                  141    282       7883140   2238      176      —          0     │
│claude-opus-4-7                                                  57     142       2227189   64283     51155    —          0     │
│claude-haiku-4-5-20251001                                        5      50        125305    611       558      —          0     │
│<synthetic>                                                      2      0         0         0         0        —          2     │
```

**Environment.** Versions, Claude Code model and effort, Codex approval and
sandbox policy, whether each tool's export is on, and counters the tools
publish such as active time and lines of code changed.

```text
┌ Environment • versions, policies, telemetry state, exported counters ──────────────────────────────────────────────────────────┐
│Claude Code version          2.1.263 (Claude Code)                                                                              │
│Codex version                codex-cli 0.153.4                                                                                  │
│Gemini CLI version           0.1.14                                                                                             │
│Ollama version               0.33.0                                                                                             │
│Claude Code model            claude-fable-5-1[1m]                                                                               │
│Claude Code effort           medium                                                                                             │
│Claude Code telemetry        on, http://localhost:8765                                                                          │
```

Press `Enter` on any request row to see every field MTop holds for it.

```text
┌ Request detail • Tab or Esc to close ──────────────────────────────────────────────────────────────────────────────────────────┐
│provider / model                                 claude-code / claude-opus-4-7                                                  │
│status / http / attempt                          tool_use / — / —                                                               │
│tokens in / cache read / cache write / out / reasoning 1 / 50285 / 1753 / 69 / —                                                │
│cache hit / context used / window                97% / — / —                                                                    │
│ttft / duration / tokens per s                   — ms / 2532.0 ms / —                                                           │
│session / project / source / agent / tier        023b55b8 / mtop-work / main /  /                                               │
│tool calls / est. USD / parse errors             1 / — / 0                                                                      │
```

Other keys: `s` cycles the row order (newest, slowest turn, most tokens),
`Space` freezes the screen while collection continues, `j`/`k` or the arrows
move the selection, `q` quits.

## 7. Keep the numbers

```sh
mtop --history            # record to ~/.local/share/mtop/history.db while running
mtop history --days 7     # per-model summary, later
```

Only numbers and short labels are stored: never prompts, responses, headers
or keys. Several MTop instances can record to the same file.

## 8. Put a price on it

Cost shows once you give MTop your rates:

```sh
mtop --prices prices.json
```

`prices.json` is a JSON array of exact model ids and per-million rates; see
the README's Prices section. Claude Code sends its own cost estimate over
telemetry, so those rows are priced even without a file. Anything unpriced
stays `—` and is left out of the total.

## When something is missing

- **Empty request table.** No transcript newer than an hour and no telemetry
  yet. Make one call in any watched tool and it appears within a second.
- **A tool is yellow.** Do the one step on its line. For key-based tools
  like Aider, that is `mtop run -- aider`, which points that one process at
  MTop's proxy.
- **Telemetry receiver "not started".** Another MTop holds port 4318. Both
  keep running; only one receives pushes. Pass `--otlp 127.0.0.1:4319` to
  the second and re-run `setup` if you want it to receive instead.
- **A tool is gray.** It cannot be observed from your machine, by MTop or by
  anything else. That is a property of how the tool is built, not a gap in
  MTop.
