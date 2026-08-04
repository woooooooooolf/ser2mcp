# 贡献指南

欢迎为 ser2mcp 贡献代码、文档或 issue！

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
