# Looklook client（看看客户端）

Run terminals and AI coding agents (Codex, Claude Code, any shell) on your own computer, and open them from your phone or any browser. Closing the page doesn't stop the task.

在自己的电脑上运行终端和 AI 编程助手（Codex、Claude Code、任意 shell），用手机或任何浏览器随时打开；关掉网页，任务照样运行。

Remote access goes through the Looklook service and needs a Looklook account. This repository contains the client and the shared protocol crate.

## Build

Requires Rust, Node.js 20+, python3 + fontTools (fonts), and zig 0.15 (only for building the bundled tmux/ttyd from source).

```bash
cd client
npm ci && npm run build                        # web console → static/
scripts/fetch-vendor.sh fonts linux-x86_64     # ttyd + fonts → vendor/ (SHA-256 checked)
cargo build --release                          # → target/release/looklook
```

Checks: `cargo test`, `cargo clippy --all-targets`, `npm run check:locales`.

| Directory | Contents |
| --- | --- |
| `client/` | The client: Rust service + React console (`src-ui/`), packaging scripts |
| `protocol/` | Client ↔ server protocol: request signing, gateway tokens, DTOs, error codes |

## License

[MIT](LICENSE). Bundled third-party components are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). The license covers the code only; the Looklook / 看看 name and logo are not licensed for use by forks.
