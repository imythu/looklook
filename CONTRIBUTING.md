# Contributing to Looklook

Thanks for helping out. Bug reports, feature ideas and pull requests are all welcome. 中文反馈也完全没问题。

## Reporting bugs and asking for features

- **Bugs:** open an issue with the bug template. Include your OS, the Looklook version (Settings page or `looklook --version`), and which agent or command you were running.
- **Features:** open an issue with the feature template, or 👍 an existing one. Votes decide what moves up the [roadmap](https://looklook.dev/roadmap).
- **Security problems:** don't open a public issue. See [SECURITY.md](SECURITY.md).

## Pull requests

1. Open an issue first for anything bigger than a small fix, so we can agree on the approach.
2. Build and check locally (see [Build](README.md#build)):
   ```bash
   cd client
   npm ci && npm run build
   cargo test
   cargo clippy --all-targets
   npm run check:locales
   ```
3. UI text lives in `client/src-ui/locales/`. Add new strings to both `en-US` and `zh-CN`; `npm run check:locales` fails if one is missing.
4. Keep the pull request focused on one change and describe what you tested.

By contributing you agree that your contribution is licensed under the repository's licenses: AGPL-3.0-or-later for `client/` and MIT for `protocol/`.
