# WeChat Selkies Rev

Run the official Linux WeChat desktop client in your browser over [Selkies](https://github.com/selkies-project/selkies) 2.2+ with GPU acceleration.

Published images contain no Tencent binaries, libraries, or icons. On first start, the container downloads the official `.deb` from Tencent (`dldir1v6.qq.com`), checks its SHA-256 digest against [`versions/wechat.json`](versions/wechat.json), caches it in `/config/.wechat-cache/`, and installs it.

## Features

WeChat is the only application in the container; Docker-in-Docker, gamepads, and printing are off by default.

- **Hardware video encoding.** Encodes H.264, VP9, and H.265 with Intel or AMD VA-API, or NVIDIA NVENC, and decodes in the browser through WebCodecs.
- **Native Chinese IME.** Uses your device's local Pinyin or CJK input method directly in the browser, with no `fcitx` inside the container.
- **Viewport window fitting.** Resizes WeChat to match your browser tab and centers fixed-size dialogs such as the QR login window.
- **Persistent auto-login.** Preserves `/etc/machine-id` and the container hostname so WeChat stays logged in across image upgrades.
- **Rust process supervisor.** Restarts WeChat on exit with backoff, reaps `WeChatAppEx` helper processes, and serves local health checks.
- **Notification webhook.** Forwards desktop notifications as JSON when configured, with message bodies redacted by default.

## Quick start

```bash
git clone https://github.com/LetterXbox/wechat-selkies-rev.git
cd wechat-selkies-rev
cp .env.example .env
docker compose up -d
```

First start downloads and verifies the WeChat package. Once `docker compose ps` shows the container as `healthy`, open `http://localhost:3011` and scan the QR code with WeChat on your phone.

> [!IMPORTANT]
> Browsers disable WebCodecs hardware decoding, clipboard sync, microphone, and webcam on plain `http://` LAN addresses, and neither port `3011` nor `3012` has authentication enabled by default. For access from other devices, use one of the HTTPS setups in the [deployment guide](docs/deployment.md).

## Requirements

- Linux on `amd64` or `arm64` with Docker Engine and Docker Compose v2 (v2.24 or newer for `compose.tailscale.yml`)
- 2 GiB of RAM (a logged-in session typically uses 450 MiB to 1.2 GiB) and 4 GiB of free disk space
- Outbound HTTPS access to `dldir1v6.qq.com` on first start
- An Intel or AMD GPU at `/dev/dri/renderD128`, or an NVIDIA GPU with the NVIDIA Container Toolkit (optional). Without a GPU, remove `/dev/dri`, `DRINODE`, and `DRI_NODE` from `docker-compose.yml` to encode on the CPU (2 or more cores recommended); see the [GPU acceleration guide](docs/gpu-acceleration.md#4-software-fallback-no-gpu-or-troubleshooting)

## Documentation

- [Deployment guide](docs/deployment.md) covers Tailscale, reverse proxy, Cloudflare Tunnel, built-in HTTPS, WebRTC, private builds, and configuration.
- [GPU acceleration guide](docs/gpu-acceleration.md) covers Intel, AMD, and NVIDIA setup, software fallback, and verification commands.
- [Architecture](docs/architecture.md) covers base image selection, X11 versus Wayland, supervisor design, and CI checks.

## Credits

Inspired by [nickrunning/wechat-selkies](https://github.com/nickrunning/wechat-selkies), which first ran WeChat for Linux in a browser over Selkies. This project is a clean-room rewrite built on [Selkies](https://github.com/selkies-project/selkies) and [LinuxServer.io `baseimage-selkies`](https://github.com/linuxserver/docker-baseimage-selkies).

## License

MIT. See [`LICENSE`](LICENSE). WeChat is proprietary software owned by Tencent and is downloaded directly from Tencent's servers at runtime.
