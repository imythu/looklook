# Looklook

**Your dev machine, in any browser.** Run Claude Code, Codex or any CLI on your laptop, Linux server or homelab, then open those terminals from your phone. Close the tab and the work keeps going. No VPN, no port forwarding, no phone app.

**电脑上的 AI 助手，手机上看看就行。** Claude Code、Codex、Kimi、Qwen Code 或任何命令行，跑在你自己的电脑、服务器或 NAS 上，用手机浏览器随时打开；关掉网页，任务照样运行。[中文说明 ↓](#中文)

[Website](https://looklook.dev) · [Download](https://looklook.dev/download) · [Docs](https://looklook.dev/docs) · [Roadmap](https://looklook.dev/roadmap) · [Releases](https://github.com/imythu/looklook/releases)

[![Looklook in 36 seconds](https://looklook.dev/media/looklook-promo-en.jpg)](https://looklook.dev/media/looklook-promo-en.webm)

## Quick start

```bash
curl -fsSL https://looklook.dev/install.sh | sh   # Linux / macOS: installs a background service, verifies SHA-256
looklook login                                    # approve the sign-in from your phone
```

Then open your personal address (shown after you sign in) on your phone. On Windows and macOS you can use the [desktop installer](https://looklook.dev/download) instead.

## What it does

- **Every agent, any auth.** Looklook works at the terminal level. Claude Code and Codex start in one tap, and anything else (Gemini CLI, OpenCode, Kimi, Qwen Code, htop, vim, `docker logs`) runs as is, whether it signs in with a subscription, an API key, Bedrock, Vertex or your own gateway.
- **Sessions that survive.** A bundled tmux (`looklook-mux`; `psmux` on Windows) keeps terminals alive through closed tabs, dropped connections and app restarts.
- **Up to 7 machines on one page.** Every terminal on every signed-in computer, grouped by machine.
- **Local pages.** Put `localhost` dev servers under your address and open them anywhere. Codex and Claude Code can add them themselves through a local MCP server.
- **Images and files for your agent.** Paste or upload screenshots and they reach Codex and Claude Code as images. Upload into the terminal's folder; `rz`/`sz` and `trzsz` work too.
- **Built for headless boxes.** It runs as a systemd user service or launchd agent, updates itself when idle, and has a System page for CPU, memory, disk and network.
- **Phone-friendly terminal.** A key bar for Esc, Tab, arrows and Ctrl combos, plus OSC 52 copy to the browser clipboard.

## How it works

```
phone / any browser ──HTTPS──▶ Looklook relay ──Noise tunnel──▶ this app ──▶ ttyd + tmux on 127.0.0.1
                                     ▲                               │
                                     └── platform: accounts, routing, short-lived access tokens
```

- The app opens an outbound, Noise-encrypted tunnel (rathole) to a relay, so no inbound ports are needed.
- Each remote request carries a short-lived token signed by the platform. **This app verifies it** before anything reaches a terminal ([`src/gateway/`](client/src/gateway)). Terminals listen only on `127.0.0.1` with random credentials.
- The local console listens on `0.0.0.0:1234` but only allows this machine by default. LAN access and an IP allowlist are opt-in, and an access code is optional ([`src/gateway/access.rs`](client/src/gateway/access.rs)).

## Security, plainly

- **Encrypted on every hop:** HTTPS from the browser to the relay, then Noise from the relay to your machine.
- **Relays decrypt in order to forward.** Like any HTTPS reverse proxy, a relay can technically see the traffic it forwards. Relays don't store or log terminal content. **End-to-end encryption**, where the browser and this app agree on keys and relays see only ciphertext, is [on the roadmap](https://looklook.dev/roadmap).
- **Your data stays local.** Sessions, files and settings live on your machine; nothing is synced to the cloud.
- **Releases are built here.** The `client-release` workflow in this repo builds every package and publishes it with `SHA256SUMS`. The installer and the in-app updater verify the checksum. Signed update manifests and code-signed or notarized installers are next on the roadmap. Today, Windows SmartScreen and macOS Gatekeeper will warn on first run.

Found a vulnerability? See [SECURITY.md](SECURITY.md).

## What's open and what's hosted

This repository is the **desktop app / client** (`client/`) and the **client ↔ server protocol** (`protocol/`), under MIT. Remote access goes through the hosted Looklook service (accounts, relays, personal addresses) and needs a Looklook account. The first year is currently free.

## Platforms

| Platform | Package | Notes |
| --- | --- | --- |
| Linux x86_64 / arm64 | `.tar.gz` + `install.sh` (systemd user service) | Works headless |
| macOS (Apple silicon) | `.dmg` desktop app, or `.tar.gz` + `install.sh` | Not notarized yet |
| Windows x86_64 | `-setup.exe` desktop app, or `.zip` | **Beta**; not code-signed yet |

## Roadmap

Next up: end-to-end encryption, signed updates, an agent board across all your machines (running / waiting / done), notifications (Web Push, Slack, Telegram, ntfy…), start tasks and review diffs from your phone, shareable previews, and more relay regions. See the [full roadmap](https://looklook.dev/roadmap). Feature requests and upvotes in [Issues](https://github.com/imythu/looklook/issues) shape the order.

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

## 中文

看看（Looklook）让你在手机或任何浏览器里打开自己电脑上的终端：看 AI 助手干到哪了、回一句话、发张截图，关掉网页任务照样跑。

- **任何 AI 助手都能跑**：工作在终端这一层，Claude Code、Codex 一键启动，Kimi、Qwen Code、OpenCode 或任何命令行照常运行，用官方订阅、API Key 还是国产 Coding Plan 都可以。
- **不用公网 IP、不改路由器**：客户端主动连出，手机不用装 App。
- **会话保持、多台电脑、本机网页、传文件和图片**，服务器和 NAS 一行命令安装。
- **安全**：每一段都加密（浏览器→中转 HTTPS，中转→电脑 Noise）；中转要解开加密才能转发，但不保存、不记录终端内容；端到端加密在[路线图](https://looklook.dev/roadmap)上。

开始使用：`curl -fsSL https://looklook.dev/install.sh | sh`，然后 `looklook login` 用手机批准。Windows / macOS 请到[下载页](https://looklook.dev/download)。反馈：QQ 群 1029871265，或在 [Issues](https://github.com/imythu/looklook/issues) 提建议。

## License

[MIT](LICENSE). Bundled third-party components are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). The license covers the code only; the Looklook / 看看 name and logo are not licensed for use by forks.
