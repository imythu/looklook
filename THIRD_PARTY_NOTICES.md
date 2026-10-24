# Third-party notices

Release packages bundle these programs and fonts. They are downloaded or built by `client/scripts/fetch-vendor.sh` and `client/scripts/build-mux.sh` and are not stored in this repository.

| Component | Version | License | Source |
| --- | --- | --- | --- |
| ttyd | 1.7.7 | MIT | https://github.com/tsl0922/ttyd |
| tmux (shipped as `looklook-mux`) | 3.7c | ISC | https://github.com/tmux/tmux |
| libevent (linked into tmux) | see `build-mux.sh` | BSD-3-Clause | https://github.com/libevent/libevent |
| utf8proc (linked into tmux) | see `build-mux.sh` | MIT | https://github.com/JuliaStrings/utf8proc |
| trzsz (file transfer helper) | 1.2.0 | MIT | https://github.com/trzsz/trzsz-go |
| psmux (Windows) | 3.3.8 | MIT | https://github.com/psmux/psmux |
| JetBrains Mono | 2.304 | SIL OFL 1.1 | https://github.com/JetBrains/JetBrainsMono |
| LXGW WenKai Mono (subset) | 1.522 | SIL OFL 1.1 | https://github.com/lxgw/LxgwWenKai |

The Rust crate rathole 0.5.0 (Apache-2.0, https://github.com/rapiz1/rathole) is vendored with small patches in `client/third_party/rathole/`; its license is in that directory and the changes are listed in `README.looklook.md` there.

Parts of the chat view (`client/src-ui/pages/ChatView.jsx`) are adapted from CloudCLI UI (https://github.com/siteboon/claudecodeui), Copyright Siteboon AI B.V. and contributors, licensed under AGPL-3.0-or-later with additional terms under section 7. The adapted parts are marked at the top of the file. This is a modified version and is not the original CloudCLI UI.

Each upstream project's own license text and the libraries they bundle apply to those components. Rust and npm dependencies are listed in `client/Cargo.lock` and `client/package-lock.json` and keep their own licenses.
