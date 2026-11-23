# WeChat Selkies Rev

Clean-room, **WeChat-only**, **Selkies 2.2+-only** containerized WeChat desktop built on Debian 13 (Trixie) for smooth GPU-accelerated browser streaming (Intel/AMD VA-API, NVIDIA NVENC, WebCodecs), reliable browser-side Chinese IME input, and ToS-safe distribution.

## Key Characteristics

- **WeChat-only & Selkies-only:** Built on `ghcr.io/linuxserver/baseimage-selkies:debiantrixie` (Selkies 2.2.1, `pixelflux 2.2.3`, `pcmflux 2.2.1`). Zero QQ, PCManFM, KasmVNC, DinD (`START_DOCKER=false`), or gamepad (`NO_GAMEPAD=true`) bloat.
- **ToS-safe binary handling:** Zero Tencent `.deb` binaries or icons are committed to Git or baked into published GHCR images. On first boot, `wechat-supervisor bootstrap` downloads the official `WeChatLinux_<arch>.deb` from `dldir1v6.qq.com`, verifies its SHA-256 against [`versions/wechat.json`](versions/wechat.json), caches it in `/config/.wechat-cache/`, installs it, and syncs the browser icon at runtime.
- **Hardware-accelerated streaming & WebCodecs defaults:** Defaults to `SELKIES_ENCODER=h264enc,vp9enc,h265enc,vp8enc` (omitting `jpeg` so the stream never silently degrades to software JPEG) with X11/DRI3 (`Xvfb -glamor -dri`) and WebSocket transport over a single HTTP/HTTPS port (`3000`/`3001`), compatible with Tailscale, built-in Basic Auth, Cloudflare Tunnel, or any TLS reverse proxy.
- **Reliable Chinese IME:** Uses Selkies 2.2.1's native XTEST Unicode keycode-overlay injector so the browser host's OS IME (Windows/macOS/Linux Pinyin) commits directly into WeChat without dropped or duplicated characters.
- **Clean-room Rust supervisor (`wechat-supervisor`):**
  - First-boot official `.deb` fetch, SHA-256 verification, caching, and install.
  - Subreaper + process-group supervision so stopping or restarting WeChat leaves zero orphan `WeChatAppEx` processes.
  - Pure-Rust X11 (`x11rb`) viewport auto-fit that resizes resizable main WeChat windows to match Selkies RandR viewport changes and centers fixed-size dialogs (such as the login QR window) without ever sending synthetic UI clicks.
  - Loopback Docker `HEALTHCHECK` endpoint (`127.0.0.1:8090/healthz`) and CLI probe (`wechat-supervisor healthcheck`).
  - Optional opt-in D-Bus notification webhook bridge (`WECHAT_NOTIFY_WEBHOOK_URL`, body redacted by default).

## Quick Start & Remote Access Options

Browsers require a secure context (`https://` or `localhost`) for WebCodecs hardware decoding, clipboard sync, microphone, and webcam. Never expose port `3000` directly to untrusted networks without authentication and TLS.

### Option 1: Tailscale sidecar (recommended when you have no public IP / VPS)

Works behind CGNAT with zero port forwarding, zero VPS cost, automatic Let's Encrypt HTTPS (`https://wechat.<tailnet>.ts.net`), and tailnet-level authentication:

1. Enable **MagicDNS** and **HTTPS Certificates** in your Tailscale admin console and create a reusable/ephemeral auth key.
2. Add `TS_AUTHKEY=tskey-auth-...` (and optional `TS_HOSTNAME=wechat`) to `.env`.
3. Start with the Tailscale overlay:

```bash
cp .env.example .env
docker compose -f docker-compose.yml -f compose.tailscale.yml up -d
```

No host ports are published; open `https://<TS_HOSTNAME>.<your-tailnet>.ts.net` from any device on your tailnet. Because Tailscale carries UDP directly between peers, you can also layer `-f compose.webrtc.yml` for direct low-latency WebRTC without a public TURN server.

### Option 2: Local LAN / Reverse Proxy / Cloudflare Tunnel

```bash
cp .env.example .env
docker compose up -d
```

By default, `docker-compose.yml` maps `${WECHAT_HTTP_PORT:-3011}:3000`. Protect it with whichever access layer fits your setup:

- **Any TLS reverse proxy with authentication** (Caddy, Nginx, Traefik, etc. + your preferred SSO / OAuth2 / Basic Auth / mTLS layer) forwarding HTTP + WebSockets to port `3011`.
- **Cloudflare Tunnel (`cloudflared`)** if you have no public IP and want access from browsers without Tailscale installed; place **Cloudflare Access** in front of the tunnel hostname.
- **Built-in Basic Auth & self-signed HTTPS:** Set `CUSTOM_USER` and `PASSWORD` in `.env` and publish container port `3001` (HTTPS).

### Opt-in WebRTC mode

Configure `SELKIES_WEBRTC_PUBLIC_IP` and STUN/TURN settings in `.env` (or pair with Tailscale), then overlay `compose.webrtc.yml`:

```bash
docker compose -f docker-compose.yml -f compose.webrtc.yml up -d
```

### Local build with optional pre-baked WeChat (private use only)

Published GHCR images never contain WeChat. For a private local image that skips first-boot download:

```bash
docker build --build-arg INSTALL_WECHAT_AT_BUILD=true -t local/wechat-selkies-rev:private .
```

## Documentation

- **GPU Acceleration:** See [`docs/gpu-acceleration.md`](docs/gpu-acceleration.md) for Intel VA-API (including Proxmox SR-IOV VFs), AMD VA-API, NVIDIA NVENC, software fallback, and verification commands.
- **Architecture:** See [`docs/architecture.md`](docs/architecture.md) for the base-image evaluation (`linuxserver/baseimage-selkies:debiantrixie` vs `selkies-project/selkies/base`), X11/DRI3 vs experimental Wayland Smithay tradeoffs, supervisor lifecycle, and CI/CD metadata tracking design.
