# 参与说明

ser2mcp 最初是个人使用的小工具，让本地串口能被 AI 助手直接读写。项目保持开放，希望能给有类似需求的人一些帮助；如果你也在使用它，并且有想改进的地方，可以按下面的方式参与。

> 说明：如果改动只是满足个人需求，直接 fork 使用即可，不必提 PR。项目的维护以实际使用需求为主，功能取舍以"对自己有用、对使用者有用"为准，不追求功能数量或贡献规模。

## 环境要求

- Rust 1.85+（edition 2024）
- Linux 构建需要 `libudev-dev`（Debian/Ubuntu：`sudo apt-get install -y libudev-dev`）

## 开发流程

1. Fork 本仓库并 clone 到本地
2. 创建功能分支：`git checkout -b feature/xxx`
3. 提交前确保以下检查全部通过：
   - `cargo fmt --all --check`
   - `cargo clippy --all-targets --all-features -- -D warnings`
   - `cargo test --all-features`
4. 提交并 push，然后创建 Pull Request
5. 在 PR 描述中说明改动动机、实现方式与测试情况

## 提交规范

- 提交信息用简洁的中文或英文，说明「做了什么」与「为什么」
- 涉及用户可见行为变化时，同步更新 README 与 CHANGELOG

## 行为准则

请参阅 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)。
