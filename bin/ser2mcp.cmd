#!/bin/sh
# 2>NUL & @goto windows
# polyglot launcher: runs as POSIX sh on Linux/macOS, as a batch file on Windows.
DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
case "$(uname -s)" in
  Darwin*) exec "$DIR/ser2mcp-macos" "$@" ;;
  *)       exec "$DIR/ser2mcp" "$@" ;;
esac
exit $?

:windows
@echo off
rem -- launcher for the ser2mcp MCP server (Windows) --
"%~dp0ser2mcp.exe" %*
exit /b %errorlevel%
