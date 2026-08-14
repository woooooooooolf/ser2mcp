# ser2mcp 版本发布流程

本文固定 ser2mcp 的版本发布、GitHub Release 生成和仓库预编译二进制同步流程，供后续发布直接复用。

`.github/workflows/ci.yml` 与 `.github/workflows/release.yml` 是自动化行为的事实来源。二者发生影响发布的变更时，必须在同一提交中同步本文。

## 1. 核心原则

发布分为两个独立阶段：

```text
发布准备提交 ── annotated tag vX.Y.Z ── GitHub Release 与远端构建产物
      │
      └── main 后续提交：同步 Reasonix 版本和 bin/ 预编译二进制，不再打 tag
```

- 只有得到明确发布授权后，才能创建 tag 或 GitHub Release。
- tag 必须指向“发布准备提交”，不能指向后续的二进制同步提交。
- `reasonix-plugin.json` 和 `bin/` 在远端 Release 成功前保持上一版本。
- 仓库 `bin/` 只能使用对应 tag 的 GitHub Release 资产；本地 `target/release/` 不能替代远端产物。
- 下载的压缩包必须先通过配套 `.sha256` 校验，再提取和复制二进制。
- `bin/ser2mcp` 是多平台入口文件，发布同步时不得替换或修改。
- 任一步骤校验失败即停止，不带着已知失败继续创建 tag、替换二进制或报告发布完成。

## 2. 版本与产物命名

设纯版本号为 `X.Y.Z`：

| 对象 | 格式 | 示例 |
| --- | --- | --- |
| Cargo / Reasonix 版本 | `X.Y.Z` | `0.8.6` |
| Git tag | `vX.Y.Z`，小写 `v` | `v0.8.6` |
| Release 标题 | `ser2mcp VX.Y.Z` | `ser2mcp V0.8.6` |
| 包内版本前缀 | `VX.Y.Z`，大写 `V` | `V0.8.6` |

Release workflow 当前生成以下六个自定义资产：

| 平台 | Runner 架构 | 压缩包 | 校验文件 |
| --- | --- | --- | --- |
| Windows | X64 | `ser2mcp-windows-x64-VX.Y.Z.zip` | 同名加 `.sha256` |
| Linux | X64 | `ser2mcp-linux-x64-VX.Y.Z.tar.gz` | 同名加 `.sha256` |
| macOS | ARM64 | `ser2mcp-macos-arm64-VX.Y.Z.tar.gz` | 同名加 `.sha256` |

工作流会在构建前核对 `RUNNER_ARCH`。架构与名称不一致时必须失败，不能发布错误标注的产物。

每个压缩包包含平台二进制、README、许可证、`skills/` 和 Rust 文档。Release 包不包含 `reasonix-plugin.json` 或仓库 `bin/` 目录。

## 3. 发布前置检查

1. 确认本次工作已得到创建 tag 和 Release 的明确授权。
2. 确认位于 `main`，工作区干净，且本地与远端没有未知分歧：

   ```powershell
   git fetch origin main --tags
   git status --short
   git rev-list --left-right --count origin/main...HEAD
   ```

   期望工作区无输出，分歧为 `0 0`。`fetch`、凭据或网络失败时停止，不使用归档下载等方式替代正常 Git 历史。

3. 确认目标版本尚不存在：

   ```powershell
   $Version = "X.Y.Z"
   $Tag = "v$Version"
   git tag --list $Tag
   ```

4. 检查当前 Release 和 CI workflow，确认平台矩阵、架构、资产命名与本文一致。

## 4. 阶段一：准备并发布源码版本

### 4.1 更新会进入 Release 的版本文件

只更新以下文件：

- `Cargo.toml`：`package.version`
- `Cargo.lock`：根包 `ser2mcp` 的版本
- `CHANGELOG.md`：把 `[Unreleased]` 内容归入 `## [X.Y.Z] - YYYY-MM-DD`，并更新底部版本链接

发布说明由 workflow 从 `CHANGELOG.md` 的对应版本段自动提取，因此标题格式必须精确为：

```markdown
## [X.Y.Z] - YYYY-MM-DD
```

此时不得提前修改：

- `reasonix-plugin.json`
- `bin/ser2mcp.exe`
- `bin/ser2mcp-linux`
- `bin/ser2mcp-macos`
- `bin/SHA256SUMS`

修改 `Cargo.toml` 后运行一次 Cargo 命令刷新并核对 `Cargo.lock`；不要用无关依赖升级扩大版本提交范围。

### 4.2 本地发布门禁

在仓库根目录依次执行：

```powershell
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo doc --no-deps
cargo build --release
.\target\release\ser2mcp.exe --version
git diff --check
```

Linux/macOS 使用：

```bash
./target/release/ser2mcp --version
```

输出版本必须为目标版本。

如果本次变更影响串口读写、缓冲、超时、取消或文件传输语义，还必须执行真实 TX-RX 回环测试：

```powershell
$env:SER2MCP_LOOPBACK_PORT = "COM10"
cargo test --test loopback -- --ignored --nocapture
```

端口名按实际环境调整。测试前确认端口未被其它进程占用，测试后确认串口已关闭、临时文件已清理。涉及真实终端交互时，再按变更范围补充设备端登录和命令测试。

### 4.3 创建发布准备提交并等待 CI

确认差异仅包含预期版本和变更说明后：

```powershell
git add -- Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "发布：准备 VX.Y.Z"
git push origin main
$ReleaseCommit = git rev-parse HEAD
```

等待 `CI` workflow 针对 `$ReleaseCommit` 的 Ubuntu、Windows、macOS 三个 job 全部成功。必须核对精确 commit SHA，不能用分支上其它成功记录代替。

可使用 GitHub Actions 页面，或在 GitHub CLI 可用时执行：

```powershell
gh run list --workflow CI --commit $ReleaseCommit --limit 1
gh run watch <run-id> --exit-status
```

### 4.4 创建并核对 annotated tag

仅在发布准备提交的 CI 成功后执行：

```powershell
git tag -a $Tag -m "ser2mcp V$Version"
git cat-file -t $Tag
git for-each-ref --format="%(objectname) %(objecttype) %(*objectname) %(*objecttype)" "refs/tags/$Tag"
git push origin $Tag
```

核对要求：

- `git cat-file -t` 输出 `tag`，证明它是 annotated tag。
- `for-each-ref` 的 peeled object 是 commit。
- peeled commit 必须精确等于 `$ReleaseCommit`。

tag 已推送后不得自行移动、删除或重建；需要改写 tag 时必须再次取得明确授权。

## 5. 等待并验证 GitHub Release

tag push 会触发 `Release` workflow。必须等待三个 build job 和 publish job 全部成功。

Release 验收条件：

- tag 为 `vX.Y.Z`
- 标题为 `ser2mcp VX.Y.Z`
- 非 draft、非 prerelease
- 发布说明完整来自 `CHANGELOG.md` 的目标版本段
- 三个平台压缩包和三个 `.sha256` 共六个自定义资产齐全
- Windows/Linux/macOS 资产名称与第 2 节完全一致

可使用：

```powershell
gh run list --workflow Release --limit 10
gh run watch <release-run-id> --exit-status
gh release view $Tag --json tagName,name,isDraft,isPrerelease,assets,url
```

Release workflow 失败、资产缺失、说明错误或架构名称不一致时，停止在当前状态；不要开始更新仓库 `bin/`。

## 6. 阶段二：用远端资产同步仓库 bin

### 6.1 下载到隔离目录

在仓库内创建唯一临时目录，不与已有文件混用：

```powershell
$DownloadDir = Join-Path $PWD ".tmp-release-$Tag"
if (Test-Path -LiteralPath $DownloadDir) {
  throw "临时目录已存在：$DownloadDir"
}
New-Item -ItemType Directory -Path $DownloadDir | Out-Null
gh release download $Tag --dir $DownloadDir
```

下载工具可以更换，但来源必须是同一 GitHub Release。GitHub CLI 或网络失败时不得回退到本地 `target/release/`。

### 6.2 校验压缩包 sidecar SHA-256

对三个 `.sha256` 逐一执行：

```powershell
$Sidecars = @(Get-ChildItem -LiteralPath $DownloadDir -Filter '*.sha256')
if ($Sidecars.Count -ne 3) {
  throw "期望 3 个 sidecar，实际为 $($Sidecars.Count)"
}
$Sidecars | ForEach-Object {
  $Line = (Get-Content -LiteralPath $_.FullName -Raw).Trim()
  if ($Line -notmatch '^([0-9a-fA-F]{64})\s+\*?(.+)$') {
    throw "非法 sidecar：$($_.Name)"
  }
  $Expected = $Matches[1].ToLowerInvariant()
  $AssetName = $Matches[2].Trim()
  $AssetPath = Join-Path $DownloadDir $AssetName
  if (-not (Test-Path -LiteralPath $AssetPath -PathType Leaf)) {
    throw "sidecar 对应资产不存在：$AssetName"
  }
  $Actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $AssetPath).Hash.ToLowerInvariant()
  if ($Actual -ne $Expected) {
    throw "SHA-256 不一致：$AssetName"
  }
}
```

校验的是 Release 压缩包与其 sidecar。后续 `bin/SHA256SUMS` 则记录提取后二进制的哈希，两者不能混用。

### 6.3 提取并映射二进制

| Release 包内文件 | 仓库目标 |
| --- | --- |
| Windows 包根目录 `ser2mcp.exe` | `bin/ser2mcp.exe` |
| Linux 包根目录 `ser2mcp` | `bin/ser2mcp-linux` |
| macOS ARM64 包根目录 `ser2mcp` | `bin/ser2mcp-macos` |

复制前记录入口文件哈希：

```powershell
$LauncherHashBefore = (Get-FileHash -Algorithm SHA256 -LiteralPath 'bin/ser2mcp').Hash
```

提取压缩包后，将三个平台二进制复制到上表目标，并执行以下更新：

1. 把 `reasonix-plugin.json` 的 `version` 更新为 `X.Y.Z`。
2. 重建 `bin/SHA256SUMS`，内容顺序和格式保持为：

   ```text
   <sha256> *ser2mcp-linux
   <sha256> *ser2mcp.exe
   <sha256> *ser2mcp-macos
   ```

3. 再次计算 `bin/ser2mcp` 哈希，必须与 `$LauncherHashBefore` 相同。

Windows 可能因正在运行的 `bin/ser2mcp.exe` 而拒绝覆盖。此时先精确确认占用进程的 `ExecutablePath`：

- 只处理确实来自当前仓库 `bin/ser2mcp.exe` 的实例。
- 不得误停 Reasonix 安装目录或其它仓库中的 ser2mcp。
- 不得在未确认串口会话状态时强杀进程；需要中断活动实例时先取得确认。
- 最终不得在仓库留下 `.old`、`.new` 或临时下载文件。

### 6.4 同步后本地验证

至少完成：

- `reasonix-plugin.json` 可解析，版本为 `X.Y.Z`，command 仍为 `bin/ser2mcp`
- `bin/SHA256SUMS` 与三个实际二进制一致
- Windows 二进制执行 `--version` 输出目标版本
- Linux 文件为 ELF64 X64，macOS 文件为 Mach-O ARM64，Windows 文件为 PE X64
- 三个仓库二进制与刚从 Release 解压的源文件哈希一致
- `bin/ser2mcp` 内容和可执行位不变
- `git diff --check` 通过
- fmt、clippy、全部 feature 测试再次通过

期望此阶段仅修改五个文件：

```text
reasonix-plugin.json
bin/SHA256SUMS
bin/ser2mcp.exe
bin/ser2mcp-linux
bin/ser2mcp-macos
```

### 6.5 创建二进制同步提交并等待 CI

```powershell
git add -- reasonix-plugin.json bin/SHA256SUMS bin/ser2mcp.exe bin/ser2mcp-linux bin/ser2mcp-macos
git commit -m "构建：更新 VX.Y.Z 预编译二进制"
git push origin main
$BinaryCommit = git rev-parse HEAD
```

不为该提交创建 tag。等待 `CI` workflow 针对 `$BinaryCommit` 的三个平台 job 全部成功。

## 7. 最终远端闭环

```powershell
git fetch origin main --tags --prune
git status --short
git rev-list --left-right --count origin/main...HEAD
git for-each-ref --format="%(objectname) %(objecttype) %(*objectname) %(*objecttype)" "refs/tags/$Tag"
```

最终必须确认：

- 工作区干净，无临时下载、解压目录或备份二进制
- `main` 与 `origin/main` 分歧为 `0 0`
- tag 仍指向发布准备提交，不是二进制同步提交
- Release workflow 和二进制同步后的 CI 均成功
- Release 非 draft、非 prerelease，六个资产与说明完整
- 当前仓库 Reasonix 版本、三个 `bin` 二进制和 `bin/SHA256SUMS` 一致

## 8. 必须停止并报告的情况

- Git fetch、push、凭据或网络失败
- 本地或远端出现未知提交、未知工作区修改或非 `0 0` 分歧
- 任一 fmt、clippy、test、doc、build、硬件门禁失败
- 发布准备 CI 或 Release workflow 失败
- tag 类型或 peeled commit 不正确
- Release 为 draft/prerelease、说明错误或资产不完整
- sidecar 格式错误或 SHA-256 不一致
- 产物实际架构与名称不一致
- Windows 二进制被无法安全识别或协调的进程占用

出现上述情况时保留证据和当前状态，等待处理；不得改写 tag、替换下载来源、使用本地构建物冒充 Release 产物或宣称发布完成。

## 9. 发布检查清单

- [ ] 已取得创建 tag 和 Release 的明确授权
- [ ] `main` 干净且与 `origin/main` 分歧 `0 0`
- [ ] Cargo/lock/CHANGELOG 已更新，Reasonix 与 `bin` 尚未提前更新
- [ ] 本地发布门禁通过；相关串口改动已完成硬件验证
- [ ] 发布准备提交已推送，精确提交的三平台 CI 成功
- [ ] annotated tag 已推送，peeled commit 正确
- [ ] Release workflow 成功，说明和六个资产通过验收
- [ ] 三个远端压缩包的 sidecar SHA-256 校验通过
- [ ] Reasonix 版本和三个 `bin` 二进制已用远端产物更新
- [ ] `bin/SHA256SUMS` 已重建，`bin/ser2mcp` 未改变
- [ ] 二进制同步提交已推送，精确提交的三平台 CI 成功
- [ ] 最终工作区干净，远端分歧 `0 0`
