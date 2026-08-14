# 将 ser2mcp 部署到 DSH（DeepSeek-Harness）

> DSH 的配置语法与技能目录约定可能随版本变化，以实际所用版本的 DSH 文档为准。
> `<dshHome>` 表示 DSH 配置根目录（通常为 `~/.dsh`）。

## 安装

### MCP 服务器

1. 取得与操作系统和架构匹配的二进制：
   - 仓库 `bin/`：
     - Windows X64 使用 `ser2mcp.exe`；
     - Linux X64 使用 `ser2mcp-linux`；
     - macOS ARM64 使用 `ser2mcp-macos`；
     - 校验值见同目录 `SHA256SUMS`。
   - Release 包：`ser2mcp-windows-x64-*.zip` / `ser2mcp-linux-x64-*.tar.gz` / `ser2mcp-macos-arm64-*.tar.gz`（配套 `.sha256`）。
   - 其它架构需要从源码构建。
2. 先校验下载内容。Release 的 `.sha256` 校验对应压缩包；仓库 `bin/SHA256SUMS` 校验对应预编译二进制。
3. 如果当前 DSH 有明确的 MCP 安装目录，遵照执行；否则安装到 `<dshHome>/bin/`（不存在则创建）。Linux/macOS 还需确保二进制具有可执行权限。
4. 在 DSH 中注册一个名为 `ser2mcp` 的 stdio MCP 服务器实例，命令使用对应平台二进制的绝对路径且参数留空。不要添加 `--list-ports`，否则进程会完成一次命令行枚举后退出，而不会进入 MCP stdio 服务模式。

仓库中的 `bin/ser2mcp` 是 Linux/macOS 统一入口文件。直接使用该入口时，必须把它与对应的 `ser2mcp-linux` 或 `ser2mcp-macos` 放在同一目录并保持可执行权限；也可以直接注册平台二进制而不使用统一入口。

### SKILLs

1. 来源：仓库或 Release 包中的 `skills/` 目录。
2. 如果当前 DSH 有明确的 skills 安装目录，遵照执行；否则使用 `<dshHome>/skills/`（不存在则创建）。
3. 将 `skills/ser2mcp-usage/` 和 `skills/ser2mcp-file-transfer/` 复制为 DSH skills 安装目录的直接子目录，保留各自的 `SKILL.md`。

## 部署后检查

1. 按当前 DSH 版本支持的方式重新加载 MCP 服务器与 SKILL。
2. 让 Agent 调用 `uart_list_ports`。返回空数组也表示 ser2mcp 已正常响应，只是当前没有可枚举串口。
3. Linux 无权访问 `/dev/ttyUSB*` 或 `/dev/ttyACM*` 时，参照仓库 `scripts/linux-serial-permissions.sh` 配置串口权限；Windows 枚举不到设备时检查对应 USB 转串口驱动。

## 卸载

1. 移除 DSH 配置中的 ser2mcp 实例。
2. 删除 DSH skills 安装目录下的 `ser2mcp-usage/` 和 `ser2mcp-file-transfer/`。
3. 删除本次安装到 DSH MCP 安装目录的 ser2mcp 可执行文件；使用统一入口时同时删除对应的平台二进制。

注意：总是先移除配置实例（结束 ser2mcp 的 MCP 进程），而后再执行删除操作。
