# WeChat Selkies Rev

Clean-room, **WeChat-only**, **Selkies 2.2+-only** containerized WeChat desktop designed for smooth GPU-accelerated browser streaming, reliable Chinese IME input, and ToS-safe distribution. These rules apply to every contributor, including coding agents.

## Language and License

- Write all repository docs, comments, commit messages, and user-facing strings in **English**.
- License: **MIT** ([`LICENSE`](LICENSE)).

## Core Scope & Guardrails

- **WeChat-only & Selkies-only:** Do not add QQ, PCManFM, KasmVNC, or multi-app desktop bloat. Keep DinD (`START_DOCKER=false`), gamepads (`NO_GAMEPAD=true`), and unused printing daemons off by default.
- **ToS-safe (no Tencent redistribution):** Never commit or publish Tencent's `WeChatLinux_*.deb` binary, extracted binaries, or icons into Git or public container images. Fetch the official `.deb` directly from `https://dldir1v6.qq.com/weixin/Universal/Linux/WeChatLinux_<arch>.deb` at first-boot runtime (cached in `/config/.wechat-cache/`) or during opt-in local-only builds (`INSTALL_WECHAT_AT_BUILD=true`), with strict SHA-256 verification against [`versions/wechat.json`](versions/wechat.json).
- **No WeChat automation or hooking:** Never inject hooks into WeChat, scrape its databases, or automate UI clicks (no scripted auto-login clicking). X11 window management is strictly passive geometry fitting (`WM_NORMAL_HINTS`-aware).
- **Clean-room implementation:** Build all scripts, supervisor code, and documentation cleanly from scratch.
- **Public repository hygiene:** Keep machine-specific paths, hostnames, private LAN IP addresses, and local tool configuration out of tracked files, commit messages, PR bodies, and release notes.

## Building & Testing

- Rust supervisor (`crates/wechat-supervisor`): must pass `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets`, and `cargo build --release --locked`.
- Container CI (`.github/workflows/build-publish.yml`): must verify `INSTALL_WECHAT_AT_BUILD=false` produces an image with zero WeChat binaries (`/usr/bin/wechat`, `/opt/wechat`, `wechat.png`) before publishing multi-arch (`linux/amd64`, `linux/arm64`) images to GHCR.
- Upstream WeChat metadata tracking (`.github/workflows/track-wechat.yml`): refreshes `versions/wechat.json` only — never commits `.deb` files.
