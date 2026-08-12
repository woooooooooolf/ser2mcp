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

The repository root contains `reasonix-plugin.json`; `bin/` contains prebuilt files for all three platforms plus the cross-platform launcher. Ask the Reasonix agent to run:

> Install the ser2mcp plugin package from https://github.com/woooooooooolf/ser2mcp. Use install_source with kind="auto" (or "plugin").

Verify the installation with `uart_list_ports`. An empty array still means the server is working; no serial ports are currently enumerable. For offline installation, download the complete source repository and pass the repository directory to `install_source` as a local path.

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
| `uart_send_file` / `uart_send_cancel` | Stream a local file in one call; request cancellation |

Every tool except `uart_list_ports` and `uart_send_estimate` requires `port`. The port name (for example, `COM3` or `/dev/ttyUSB0`) is the handle. Ordinary I/O, configuration, expect, and close operations share a global I/O lock; they queue during file sending, while `uart_available` / `uart_clear` remain concurrent.

## AI Usage Guides

The repository includes two portable Agent Skills:

- [`ser2mcp-usage`](skills/ser2mcp-usage/SKILL.md): tool selection, encoding, command-completion detection, buffering, and recovery
- [`ser2mcp-file-transfer`](skills/ser2mcp-file-transfer/SKILL.md): authorization, estimation, peer setup, EOF, cancellation, and end-to-end reconciliation

Reasonix installs both SKILLs with the plugin. Claude Code, Codex, and other agents can mount `skills/` into their respective skill directories.

Important semantic boundaries:

- `reason="idle"` only means that the byte stream became quiet; it does not mean that a command completed. Use `uart_expect` when a prompt or end marker is available.
- When terminal input echo is enabled, a pattern contained in the command can make `uart_expect` match early. Disable echo or use an output marker whose complete pattern does not occur contiguously in the command text.
- `overflow_delta > 0` means that ring-buffer data was overwritten, so the current read has a gap.
- The overflow fields from `uart_send_file` are return-time snapshots. Check the latest `overflow_total` with `uart_available` or `uart_read` afterward; zero is not final proof that no overflow occurred.
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
