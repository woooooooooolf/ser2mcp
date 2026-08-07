# ser2mcp

[![CI](https://github.com/woooooooooolf/ser2mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/woooooooooolf/ser2mcp/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/woooooooooolf/ser2mcp?sort=semver)](https://github.com/woooooooooolf/ser2mcp/releases)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![Stars](https://img.shields.io/github/stars/woooooooooolf/ser2mcp)](https://github.com/woooooooooolf/ser2mcp)
[![Forks](https://img.shields.io/github/forks/woooooooooolf/ser2mcp)](https://github.com/woooooooooolf/ser2mcp/fork)
[![Last commit](https://img.shields.io/github/last-commit/woooooooooolf/ser2mcp)](https://github.com/woooooooooolf/ser2mcp/commits/main)
[![Downloads](https://img.shields.io/github/downloads/woooooooooolf/ser2mcp/total)](https://github.com/woooooooooolf/ser2mcp/releases)

[![Windows](https://img.shields.io/badge/Windows-0078D6?logo=windows&logoColor=white)](https://github.com/woooooooooolf/ser2mcp/releases)
[![Linux](https://img.shields.io/badge/Linux-FCC624?logo=linux&logoColor=black)](https://github.com/woooooooooolf/ser2mcp/releases)
[![macOS](https://img.shields.io/badge/macOS-000000?logo=apple&logoColor=white)](https://github.com/woooooooooolf/ser2mcp/releases)
[![GitHub Actions](https://img.shields.io/badge/GitHub_Actions-2088FF?logo=githubactions&logoColor=white)](https://github.com/woooooooooolf/ser2mcp/actions)

![Rust](https://img.shields.io/badge/Rust-000000?logo=rust&logoColor=white)
![Rust (top language)](https://img.shields.io/github/languages/top/woooooooooolf/ser2mcp)
![Edition 2024](https://img.shields.io/badge/Edition-2024-000000)
![MSRV 1.85](https://img.shields.io/badge/MSRV-1.85-orange)
![tokio](https://img.shields.io/badge/tokio-runtime-2CA5E0?logo=tokio)
![rmcp](https://img.shields.io/badge/rmcp-MCP_SDK-4B32C3)
![serialport](https://img.shields.io/badge/serialport-4.9-2C3E50)

![MCP](https://img.shields.io/badge/MCP-Model_Context_Protocol-000000?logo=modelcontextprotocol&logoColor=white)
![UART](https://img.shields.io/badge/UART-Serial_Port-007EC6)
![hex/text](https://img.shields.io/badge/hex%2Ftext-binary_safe-00ADD8)
![unsafe](https://img.shields.io/badge/unsafe-none-2EA043)
![single binary](https://img.shields.io/badge/single_binary-no_runtime-512BD4)

**English** | [简体中文](README.md)

**UART serial port MCP server**: exposes local serial port devices as standard **MCP (Model Context Protocol)** tools, so AI assistants (Claude Desktop, Cursor, or any MCP client) can read and write serial ports directly.

```mermaid
flowchart LR
    client["MCP client<br>（AI agent）"]
    server["ser2mcp<br>（reader thread + ring buffer）"]
    uart["UART device<br>（TX-RX）"]

    client <==>|"JSON-RPC over stdio"| server
    server <==>|"UART"| uart
```

## Features

- **14 MCP tools**: list ports, open, runtime re-configuration, write, read, write+read, expect output, send-on-match, status, clear buffer, close, file send (estimate / send / cancel)
- **Streaming file send**: `uart_send_file` sends a local file to the serial port in rate-limited chunks in one call (raw `text` / `base64` with automatic line wrapping), replacing per-chunk `uart_write` calls by the model; with `uart_send_estimate` for time estimation and three-level abort via `uart_send_cancel` / `uart_close` / client cancel notification; progress is queryable while sending
- **Full serial parameter control**: baudrate / data bits (5-8) / parity (none/even/odd) / stop bits (1,2) / flow control (none/software/hardware) / read timeout — all settable via `uart_open` / `uart_configure`
- **Configurable internal parameters**: ring buffer size `buffer_size` (default 1 MiB), idle detection `idle_ms`, per-call fetch cap `max_bytes`, total timeout `timeout_ms`, reader timeout `read_timeout_ms` (default 500ms; safety cap only, does not affect latency)
- **Event-driven / non-blocking reader thread (platform adaptation layer)**: Unix (Linux/macOS) uses `poll(2)` + self-pipe events; Windows uses 1ms polling + `bytes_to_read()` gating + `timeBeginPeriod(1)`, calling `read()` only when data is ready, so read/write latency is decoupled from the read-timeout parameter
- **Continuous ingress buffering**: the event-driven reader thread continuously buffers serial data into a ring buffer; when full, the oldest data is overwritten and an **overflow counter** is incremented. Return values carry `overflow_delta / overflow_total`, so data gaps are detectable
- **Binary-safe**: data travels as hex strings (e.g. `"41 54 0D 0A"`); `mode="text"` switches to UTF-8 text; `read_mode="text-escaped"` keeps text primary and escapes non-text bytes as `\xNN` (no fallback for terminal/log scenarios)
- **Single-binary delivery**: `cargo build --release` produces one executable; Windows / Linux / macOS — no extra runtime required

## Quick Install

> You can also download prebuilt binaries for your platform (Windows / Linux / macOS) from the [Releases](https://github.com/woooooooooolf/ser2mcp/releases) page.

```bash
# 1. Clone the repository
git clone https://github.com/woooooooooolf/ser2mcp.git
cd ser2mcp

# 2. Linux system dependency (Debian/Ubuntu only; skip on macOS/Windows)
sudo apt-get install -y libudev-dev

# 3. Build the release binary
cargo build --release
# Artifact: target/release/ser2mcp (ser2mcp.exe on Windows)

# 4. Sanity check (optional): list local serial ports
target/release/ser2mcp --list-ports

# 5. Register as an MCP server (see "Connecting to MCP Clients" below)
```

**Verify a successful install**: after registration, calling `uart_list_ports` should return the local port list (possibly an empty array); with TX-RX loopback hardware, data sent via `uart_exchange` should come back unchanged.

## Build & Test

> **Linux users**: `serialport` needs `libudev` to enumerate USB port information.
> Debian/Ubuntu: `sudo apt-get install -y libudev-dev`

```bash
cargo build --release   # build
cargo test              # unit + end-to-end MCP protocol tests (no serial hardware needed)
cargo doc --no-deps     # generate Rust docs
```

## Command Line

After downloading a prebuilt binary or building from source, you can run:

```bash
ser2mcp --list-ports   # enumerate local serial ports
ser2mcp --version      # show version
ser2mcp --help         # show help
```

Running without arguments starts the MCP server in stdio mode (for AI assistants).

## Connecting to MCP Clients

MCP clients launch the server as a stdio subprocess. Generic configuration (`.mcp.json`, Claude Desktop, etc.):

```json
{
  "mcpServers": {
    "ser2mcp": {
      "command": "/absolute/path/to/ser2mcp",
      "args": []
    }
  }
}
```

Windows example: `"command": "C:\\tools\\ser2mcp.exe"`.

### Install as a Reasonix plugin package (recommended, zero manual config)

In Reasonix, install the repository as a plugin package:

> Install the ser2mcp plugin package from https://github.com/woooooooooolf/ser2mcp. Use install_source with kind="auto" (or "plugin").

The root `reasonix-plugin.json` declares ser2mcp as a standard MCP server (`bin/` ships prebuilt Windows / Linux / macOS binaries plus a cross-platform launcher script):

1. Run `install_source` in Reasonix: **point the source at the repository URL** `https://github.com/woooooooooolf/ser2mcp`; use kind `auto` (detected as a plugin package) or explicit `plugin`; scope defaults to `global`
2. Reasonix copies the whole repository into its global plugin directory (Windows: `%APPDATA%\reasonix\plugins\ser2mcp`); the manifest's `command` (relative path `bin/ser2mcp.cmd`) resolves against the plugin package root — **no manual path edits needed**
3. After install, a server named `ser2mcp` is registered; tools are exposed as `mcp__ser2mcp__uart_*`
4. **Verify**: call `uart_list_ports` after registration; it should return the local serial port list (possibly an empty array)

> `bin/ser2mcp.cmd` is the cross-platform launcher (Unix picks `ser2mcp` / `ser2mcp-macos` via `uname`, Windows calls `ser2mcp.exe`; keep it pure ASCII — cmd.exe misparses non-ASCII bytes on non-UTF-8 code pages).
>
> Offline install: point `install_source` at a local path (a local clone or an extracted release package) — it installs the same plugin-package way.

Environment variables (optional):

| Variable   | Default | Description                                                      |
|------------|---------|------------------------------------------------------------------|
| `RUST_LOG` | `info`  | Log level (logs go to **stderr**, never polluting the stdio protocol channel) |

## Tool Reference

| Tool | Description |
|---|---|
| `uart_list_ports` | Enumerate local serial ports (name / type / USB description) |
| `uart_open` | Open a port and start the background reader thread (`port` required; all serial parameters + internal params like `buffer_size`) |
| `uart_configure` | Re-configure a running port (`port` required; only updates passed fields) |
| `uart_write` | Send data, return immediately (`port` required; no reply waiting) |
| `uart_read` | Pull buffered ingress data (`port` required) |
| `uart_exchange` | Send + read in one step (`port` required; most common) |
| `uart_expect` | Wait for a matching output: block until a specified pattern appears on the port or until timeout (`port`, `pattern` required; optional `data` for one-step "send + wait") |
| `uart_expect_send` | Send on match: wait for a pattern, then send `reply` in the same critical section (`port`, `pattern`, `reply` required) |
| `uart_available` | Status snapshot: config, buffered bytes, total overflow, reader-thread errors, file-send progress (`port` required) |
| `uart_clear` | Clear unread buffered data (`port` required) |
| `uart_close` | Close the port and release the handle (`port` required; an in-flight file send is interrupted) |
| `uart_send_estimate` | Estimate file-send bytes and duration (`path` required; no port needed, `baudrate` defaults to 115200) |
| `uart_send_file` | Streaming file send: send a local file to the port in rate-limited chunks, one call (`port`, `path` required) |
| `uart_send_cancel` | Abort an in-flight `uart_send_file` (`port` required; no-op when idle) |

> **Multi-port & pass-through**: multiple ports can be open at the same time; the port name (e.g. `COM3`, `/dev/ttyUSB0`) is the handle, and every tool except `uart_list_ports` requires a `port` argument. The byte stream is passed through **as-is**: ser2mcp does not parse or filter content (`uart_expect` / `uart_expect_send` only search the buffer conditionally without modifying data), so unexpected data is returned unchanged for the AI / upper layer to interpret.


### Usage Guide (for AI Agents)

The full usage guide ships as SKILLs bundled with the plugin, loaded on demand by the agent (see "AI Agent Compatibility" below):

- `ser2mcp-usage` — tool reference, data representation & encoding choices, read/expect semantics, command-completion detection, troubleshooting
- `ser2mcp-file-transfer` — full streaming file-send workflow (estimate / send / EOF / reconcile, peer-tty notes)

**Quick points** (authoritative semantics live in the SKILLs):

- Encoding: `hex` (default, binary-safe) / `text` (UTF-8) / `text-escaped` (return side only; control bytes escaped as `\xNN`). Terminal commands **must** carry a line terminator via `newline="crlf"` — otherwise the command stays in the device line buffer and can be concatenated with the next one
- Read: `uart_read` / `uart_exchange` return on idle (`idle_ms`, default 300ms — keep above the device response gap), max size (`max_bytes`, default 64 KiB), or total timeout (`timeout_ms`, default 5000ms); `overflow_delta > 0` means the ring buffer overflowed and data was lost
- Completion: prefer `uart_expect` output anchors (e.g. `"# "`, `"Zynq>"`) — return is millisecond-fast on match; do not sleep-poll or inflate timeouts. Prompts vary by device; when unavailable (echo off / prompt-less device) use a command-specific end marker instead. For long commands (wget/tar unpack etc.), `uart_expect` is idle-independent and its `timeout_ms` is only a fallback cap (5 min max, returns early on match) — it can be raised to cover the whole command duration
- Large files: `uart_send_estimate` → `uart_send_file` in one call (never chunk with `uart_write`), then reconcile with the peer's `wc -c` / `md5sum`

**Common examples**:

```
uart_exchange {port: "COM3", data: "ls /", mode: "text", newline: "crlf", read_mode: "text-escaped"}  # terminal command
uart_exchange {port: "COM3", data: "AT\r\n", mode: "text"}                                            # AT command
uart_exchange {port: "COM3", data: "AA 55 01 00 0D 0A", mode: "hex"}                                   # binary frame
uart_expect    {port: "COM3", data: "ls /", mode: "text", newline: "crlf", pattern: "# ", pattern_mode: "text", read_mode: "text-escaped"}  # send + wait for prompt, one step
uart_expect    {port: "COM3", pattern: "Zynq>", pattern_mode: "text"}                                 # wait for prompt
uart_expect_send {port: "COM3", pattern: "Hit any key", reply: "\n", pattern_mode: "text"}           # press key on match
```

### AI Agent Compatibility

The SKILLs use the generic Agent Skills format (`SKILL.md` with `name` / `description` frontmatter) and work across tools:

- **Reasonix**: installed with the plugin; invoke as `/ser2mcp:ser2mcp-usage` / `/ser2mcp:ser2mcp-file-transfer`, or let the agent auto-select by `description`
- **Claude Code / Codex**: mount the repository `skills/` directory as `.claude/skills/` (or `.codex/skills/`) and use it directly

### Typical Usage (AI Agent Perspective)

```
1. uart_list_ports                      → find "COM3"
2. uart_open {port: "COM3", baudrate: 115200}
3. uart_exchange {port: "COM3", data: "41 54 0D 0A"}  → AT command (hex)
4. uart_exchange {port: "COM3", data: "ls /", mode: "text", newline: "crlf", read_mode: "text-escaped"}  → terminal command
5. uart_expect {port: "COM3", pattern: "Zynq>", pattern_mode: "text"}  → wait for prompt (timing)
6. uart_expect_send {port: "COM3", pattern: "Hit any key", reply: "\n", pattern_mode: "text"}  → press key on match (bootdelay window)
7. uart_configure {port: "COM3", baudrate: 9600}      → re-configure after device baudrate switch
8. uart_close {port: "COM3"}
```

File transfer scenario (large files: `uart_send_file` in one call; full workflow in the `ser2mcp-file-transfer` SKILL):

```
uart_send_estimate {path: "C:/tmp/fw.bin", mode: "base64"}            → estimate duration first
uart_exchange {port: "COM3", data: "stty -echo; cat > /tmp/f.b64", mode: "text", newline: "lf"}  → peer starts receiving
uart_send_file {port: "COM3", path: "C:/tmp/fw.bin", mode: "base64"}  → send in one call
uart_write {port: "COM3", data: "04"}                                  → send \x04 to end the peer's cat (EOF)
uart_exchange {port: "COM3", data: "wc -c /tmp/f.b64; md5sum /tmp/f.b64", mode: "text", newline: "lf"}  → reconcile
```
## Loopback Self-Test (Real Hardware, TX-RX Jumpered)

Built-in one-command self-test: enumerate ports + run a full loopback verification on a given port (sends the full 0x00-0xFF byte sequence and verifies it comes back unchanged):

```bash
cargo run --release --example loopback -- --list      # enumerate local ports
cargo run --release --example loopback -- COM3 115200 # loopback test
```

## Module Layout

```
src/
├── main.rs      # entry point: stdio transport
├── lib.rs       # crate docs & module declarations
├── hex.rs       # hex encode/decode (hex/text/text-escaped triple mode)
├── ring.rs      # bounded ring buffer (overwrite-oldest + overflow counter + Notify + pattern search)
├── sendfile.rs  # streaming file send (chunked read + base64 encoding + time estimation)
├── manager.rs   # serial manager (open / re-configure / reader thread / write / pull / expect / file send)
├── reader.rs    # event-driven / non-blocking reader thread (platform adaptation layer)
└── server.rs    # MCP tool layer (14 tools + ServerHandler)
tests/
├── e2e.rs       # end-to-end MCP protocol tests (real subprocess handshake, no hardware)
└── loopback.rs  # real-hardware loopback tests (#[ignore]; SER2MCP_LOOPBACK_PORT selects the port)
scripts/
└── mcp_cli.py   # lightweight MCP stdio CLI client (batch calls from a JSON action sequence)
skills/
├── ser2mcp-usage/         # AI usage SKILL: tool reference / encoding / read semantics / troubleshooting
└── ser2mcp-file-transfer/ # streaming file-send SKILL: estimate / send / EOF / reconcile / peer-tty notes
examples/
├── loopback.rs      # loopback self-test tool
└── latency_probe.rs # latency probe (bench/benchw, real-hardware load test)
```

## Tech Stack

- [rmcp](https://github.com/modelcontextprotocol/rust-sdk) (official Rust MCP SDK)
- [serialport](https://crates.io/crates/serialport)
- tokio / serde / schemars

## Security Notice

ser2mcp gives AI assistants direct read/write access to your serial ports: any authorized MCP client (and the model behind it) can send arbitrary bytes to connected devices. Only connect devices you trust, make sure your MCP client and model are from reliable sources, and do not use this tool with devices that could be damaged by incorrect commands.

## Troubleshooting

- **Permission denied / cannot open `/dev/ttyUSB0` on Linux**: your user is not in the `dialout` (or `uucp`) group. Run `scripts/linux-serial-permissions.sh` as root, then log out and back in.
- **Port open failure / port busy**: make sure no other serial terminal or MCP instance is using the port.
- **No ports found on Windows**: check that the USB-to-serial driver (CH340 / CP210x, etc.) is installed.
- **High tool-call latency**: the fixed wait per read/write round-trip comes mainly from `idle_ms` (default 300ms); lower it to match the device response rhythm (e.g. 50ms). `read_timeout_ms` (default 500ms) is only a safety cap and does not affect latency.
- **Missing / incomplete data**: `overflow_delta > 0` means data was dropped due to buffer overflow; increase `buffer_size` or pull data more frequently.

## License

MIT OR Apache-2.0 (see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE))
