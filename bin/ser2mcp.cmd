@echo off
goto windows
#!/bin/sh
# polyglot launcher: runs as POSIX sh on Linux/macOS, as a batch file on Windows.
# The two lines above are required so cmd.exe never echoes anything to stdout
# before @echo off takes effect (MCP is a stdio protocol: stray stdout lines
# break the handshake). POSIX sh sees them as harmless errors and continues
# here. Unix users may prefer configuring bin/ser2mcp (or bin/ser2mcp-macos)
# directly to avoid those stderr lines.
# NOTE: keep this file ASCII-only; non-ASCII bytes break cmd.exe parsing
# on non-UTF-8 Windows code pages (e.g. GBK) and make "goto" fail.
DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
case "$(uname -s)" in
  Darwin*) exec "$DIR/ser2mcp-macos" "$@" ;;
  *)       exec "$DIR/ser2mcp" "$@" ;;
esac
exit $?

:windows
@echo off
rem launcher for the ser2mcp MCP server on Windows
"%~dp0ser2mcp.exe" %*
exit /b %errorlevel%
