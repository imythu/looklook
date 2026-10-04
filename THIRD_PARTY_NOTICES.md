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

Each upstream project's own license text and the libraries they bundle apply to those components. Rust and npm dependencies are listed in `client/Cargo.lock` and `client/package-lock.json` and keep their own licenses.
