#!/usr/bin/env python3
"""mcp_cli.py — 轻量 MCP stdio 客户端：对 ser2mcp 执行一串工具调用并打印 JSON 结果。

用途：命令行/脚本化验证（如板端实测、回环测试），替代每次手动拼 JSON-RPC。

用法:
  python mcp_cli.py <ser2mcp 二进制> '<动作序列 JSON>'

动作序列是 JSON 数组，每项 {"tool": "...", "args": {...}}：
  [
    {"tool": "uart_open",     "args": {"port": "COM27", "baudrate": 115200}},
    {"tool": "uart_exchange", "args": {"port": "COM27", "data": "ls /", "mode": "text",
                                       "newline": "crlf", "read_mode": "text-escaped"}},
    {"tool": "uart_send_file","args": {"port": "COM27", "path": "C:/tmp/a.bin", "mode": "base64"}},
    ...
  ]

每个动作的完整 JSON 响应打印一行（{tool, ok, result|error}）。
进程在整个动作序列内保持（串口状态不丢失）。
"""
import json
import subprocess
import sys
import threading
import time


class McpCli:
    def __init__(self, binary: str):
        self.proc = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            bufsize=1,  # 行缓冲
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        self.next_id = 1
        self.pending = {}  # id -> threading.Event + result

    def _send(self, obj: dict) -> None:
        line = json.dumps(obj, ensure_ascii=False)
        self.proc.stdin.write(line + "\n")
        self.proc.stdin.flush()

    def _reader(self):
        for line in self.proc.stdout:
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            rid = msg.get("id")
            if rid is not None and rid in self.pending:
                ev, result = self.pending[rid]
                result["msg"] = msg
                ev.set()

    def initialize(self) -> dict:
        self._send({
            "jsonrpc": "2.0", "id": self.next_id, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "mcp-cli", "version": "0.1.0"},
            },
        })
        rid = self.next_id
        self.next_id += 1
        ev = threading.Event()
        result = {}
        self.pending[rid] = (ev, result)
        if not ev.wait(timeout=15):
            raise TimeoutError("initialize 超时")
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})
        return result["msg"]

    def call(self, tool: str, args: dict, timeout: float = 120.0) -> dict:
        self._send({
            "jsonrpc": "2.0", "id": self.next_id, "method": "tools/call",
            "params": {"name": tool, "arguments": args},
        })
        rid = self.next_id
        self.next_id += 1
        ev = threading.Event()
        result = {}
        self.pending[rid] = (ev, result)
        if not ev.wait(timeout=timeout):
            raise TimeoutError(f"{tool} 调用超时（{timeout}s）")
        msg = result["msg"]
        if "error" in msg:
            return {"ok": False, "error": msg["error"]}
        res = msg.get("result", {})
        if res.get("isError"):
            return {"ok": False, "error": res.get("structuredContent", res.get("content"))}
        return {"ok": True, "result": res.get("structuredContent", res)}

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()


def main():
    # Windows 控制台默认 GBK，板端输出可能含任意 UTF-8 字符，统一 UTF-8 输出。
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    if len(sys.argv) != 3:
        print(__doc__)
        sys.exit(2)
    binary, seq_json = sys.argv[1], sys.argv[2]
    actions = json.loads(seq_json)
    cli = McpCli(binary)
    threading.Thread(target=cli._reader, daemon=True).start()
    try:
        init = cli.initialize()
        print(json.dumps({"tool": "initialize", "ok": True,
                          "server": init.get("result", {}).get("serverInfo", {})},
                         ensure_ascii=False))
        for act in actions:
            tool = act["tool"]
            args = act.get("args", {})
            timeout = act.get("timeout", 120.0)
            try:
                out = cli.call(tool, args, timeout=timeout)
            except TimeoutError as e:
                out = {"ok": False, "error": str(e)}
            print(json.dumps({"tool": tool, **out}, ensure_ascii=False))
    finally:
        cli.close()


if __name__ == "__main__":
    main()
