# ser2mcp

[![CI](https://github.com/woooooooooolf/ser2mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/woooooooooolf/ser2mcp/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/woooooooooolf/ser2mcp?sort=semver)](https://github.com/woooooooooolf/ser2mcp/releases)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

**English** | [简体中文](README.md)

ser2mcp is a local UART serial-port MCP server. It exposes port discovery, configuration, I/O, output matching, and file sending as standard MCP tools for AI clients that support stdio MCP servers.

## Core Capabilities

- 14 `uart_*` tools for multiple ports, runtime reconfiguration, write-and-read exchanges, and pattern-based timing workflows
- Continuous background reading into a bounded ring buffer with `overflow_delta / overflow_total` loss reporting
- Hex, UTF-8 text, and receive-only text-escaped modes for binary protocols and terminal logs
- One-call local-file streaming through `uart_send_file`, with continuous base64, progress, estimation, and cancellation
- One executable per platform for Windows, Linux, and macOS; no Rust runtime installation required

## Install and Connect

Download a prebuilt package for your platform from [Releases](https://github.com/woooooooooolf/ser2mcp/releases), or build from source:

```bash
git clone https://github.com/woooooooooolf/ser2mcp.git
cd ser2mcp

# Debian/Ubuntu build dependency; skip on Windows/macOS
sudo apt-get install -y libudev-dev

cargo build --release
target/release/ser2mcp --list-ports
```

Running `ser2mcp` without arguments starts the MCP stdio server. Generic client configuration:

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

Windows path example: `"command": "C:\\tools\\ser2mcp.exe"`. Logs go to stderr; set `RUST_LOG` to adjust the level (default: `info`).

### Reasonix Plugin Install

The repository root contains `reasonix-plugin.json`; `bin/` contains prebuilt files for all three platforms plus a POSIX launcher. The manifest consistently uses `bin/ser2mcp`: Windows resolves the matching `ser2mcp.exe`, while Linux/macOS use the script to select their platform binary. Ask the Reasonix agent to run:

> Install the ser2mcp plugin package from https://github.com/woooooooooolf/ser2mcp. Use install_source with kind="auto" (or "plugin").

Verify the installation with `uart_list_ports`. An empty array still means the server is working; no serial ports are currently enumerable. For offline installation, download the complete source repository and pass the repository directory to `install_source` as a local path.

### DeepSeek-Harness Integration

Tell the Agent in DSH:

> Install ser2mcp into DSH from [https://github.com/woooooooooolf/ser2mcp](https://github.com/woooooooooolf/ser2mcp), following [docs/DSH_INTEGRATION.md](docs/DSH_INTEGRATION.md) to complete the deployment.

The Agent should follow the configuration and directory conventions of the installed DSH version, register the stdio MCP server, and install both SKILLs from this repository.

### ZCode Plugin Marketplace Install

The root `marketplace.json` exposes this repository as a ZCode plugin marketplace, with the plugin manifest at `.zcode-plugin/plugin.json`. In ZCode, open **Settings → Plugins**, choose **Create → Add Marketplace**, enter the full repository URL `https://github.com/woooooooooolf/ser2mcp`, then install and enable `ser2mcp`.

Installation loads all `uart_*` MCP tools and both repository SKILLs. Verify with `uart_list_ports`; an empty array still means the server is working, with no serial ports currently enumerable. For offline installation, add the repository directory as a local marketplace source.

## Tools

| Tool | Purpose |
|---|---|
| `uart_list_ports` | List port names, types, and USB descriptions |
| `uart_open` / `uart_configure` / `uart_close` | Open, reconfigure, and close a port |
| `uart_write` | Send data without waiting for a reply |
| `uart_read` | Pull ingress data until idle, byte limit, or total timeout |
| `uart_exchange` | Write a short command and perform idle-based reading in one I/O critical section |
| `uart_expect` | Optionally send data, then wait for an output pattern |
| `uart_expect_send` | Send a reply immediately after a pattern matches |
| `uart_available` / `uart_clear` | Query status, overflow, errors, and send progress; clear unread data |
| `uart_send_estimate` | Estimate file-send bytes and duration without opening a port |
| `uart_send_file` / `uart_send_cancel` | Stream a local file in one blocking call; request cancellation of an active transfer |

Every tool except `uart_list_ports` and `uart_send_estimate` requires `port`. The port name (for example, `COM3` or `/dev/ttyUSB0`) is the handle. Ordinary I/O, configuration, expect, and close operations share a global I/O lock; they queue during file sending, while `uart_available` / `uart_clear` remain concurrent. If the host permits concurrent calls, or a later task can still access the same service, `uart_send_cancel` can request cancellation of an active transfer.

## AI Usage Guides

The repository includes two portable Agent Skills:

- [`ser2mcp-usage`](skills/ser2mcp-usage/SKILL.md): complete interactive-tool parameters, selection, encoding, completion detection, buffering, and recovery
- [`ser2mcp-file-transfer`](skills/ser2mcp-file-transfer/SKILL.md): complete file-tool parameters, authorization, estimation, peer setup, EOF, cancellation, and end-to-end reconciliation

Reasonix and ZCode install both SKILLs with the plugin. Claude Code, Codex, and other agents can mount `skills/` into their respective skill directories.

Important semantic boundaries:

- `reason="idle"` only means that the byte stream became quiet; it does not mean that a command completed. Use `uart_expect` when a prompt or end marker is available.
- `uart_exchange` returns pre-existing buffered data, but idle or byte-limit completion is not allowed until at least one new ingress batch is observed after the current write.
- `new_data_observed` from `uart_read` / `uart_exchange` reports whether new ingress was observed after the call began; `pending` reports unread buffered data in the return-time snapshot. `pending=false` does not prove that the device will not produce later output or that the command completed.
- Only `reason="timeout"` may accompany `bytes=0` from `uart_read` / `uart_exchange`; a concurrent buffer clear no longer produces an empty idle/max_bytes result.
- `matched=true` only proves that the pattern matched within the selected scope under raw-byte or ANSI-ignoring semantics; it does not prove transaction success. Use prompts/output markers for terminals, status codes or transaction IDs for AT/no-echo MCUs, and frame fields plus validation for binary protocols.
- `uart_expect.match_scope="buffer"` (the default) includes historical unread data. Use `"new"` to wait only for bytes received after the call starts. On terminals with input echo, disable echo or use an output marker whose complete pattern does not occur contiguously in the command text.
- Patterns match raw bytes by default. On colorized terminals, explicitly set `ignore_ansi=true` to skip common CSI, OSC, and related ANSI control sequences while matching. This option does not modify the raw buffer or returned data; do not enable it for binary protocols.
- `uart_expect_send.newline` applies to `reply`. For a terminal reply, use `reply_mode="text"` with `newline="crlf"` instead of embedding the line ending in the reply text.
- By default, `uart_expect` consumes only through the end of the pattern. Follow it with `uart_read` when `pending=true` (equivalent to `buffered_bytes > 0`). `pending=false` is still only an instantaneous snapshot; continue waiting according to the device protocol when later output is required.
- Tools with arguments reject unknown fields instead of silently ignoring them. `buffer_size` can only be set by `uart_open`; close and reopen the port to change it.
- `overflow_delta > 0` means that ring-buffer data was overwritten, so the current read has a gap.
- The overflow fields from `uart_send_file` are return-time snapshots. Check the latest `overflow_total` with `uart_available` or `uart_read` afterward; zero is not final proof that no overflow occurred.
- `uart_send_file` blocks until it finishes by default. Optional `max_duration_ms` is an explicit automatic safeguard that returns `reason="duration_limit"`; normally, wait for the estimate-based transfer duration.
- `uart_send_file` returning `reason="completed"` only means that the server finished writing. Confirm end-to-end integrity with peer byte counts and a hash of the decoded content.

## Validation and Development

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo doc --no-deps
```

Real-hardware TX-RX loopback test:

```bash
cargo run --release --example loopback -- --list
cargo run --release --example loopback -- COM3 115200
```

If Linux reports permission denied for `/dev/ttyUSB0`, run `scripts/linux-serial-permissions.sh` as root, then log out and back in. If Windows cannot enumerate a port, check the USB-to-serial driver, such as CH340 or CP210x.

## Security

ser2mcp gives AI clients direct serial I/O and local-file sending capabilities. `uart_send_file` can read any regular file accessible to the ser2mcp process and transmit it over the serial port; the server does not restrict paths to an allowlist. Run ser2mcp with a least-privileged account, connect only trusted devices, and verify that both the file path and destination device are within the user's authorized scope before sending. See [SECURITY.md](SECURITY.md) for details.

## License

MIT OR Apache-2.0 (see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE))
