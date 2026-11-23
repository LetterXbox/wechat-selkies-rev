# WeChat Selkies Rev

Run the official Linux WeChat desktop client in a browser over [Selkies](https://github.com/selkies-project/selkies) 2.2+ on Debian 13 (Trixie).

This is a clean-room, WeChat-only container image. Published images contain no Tencent binaries, libraries, or icons. On first container start, `wechat-supervisor` downloads the official `WeChatLinux_<arch>.deb` from Tencent (`dldir1v6.qq.com`), verifies its SHA-256 digest against [`versions/wechat.json`](versions/wechat.json), caches the package in `/config/.wechat-cache/`, and installs it.

## How it works

The container layers a single Rust supervisor (`wechat-supervisor`) and WeChat's Debian runtime libraries on top of `ghcr.io/linuxserver/baseimage-selkies:debiantrixie` (Selkies 2.2.1, `pixelflux` 2.2.3, and `pcmflux` 2.2.1). Unused base services (`START_DOCKER`, gamepads, and CUPS printing) are disabled by default.

- **First-boot verification and caching.** An s6 oneshot (`init-wechat-bootstrap`) runs before the desktop starts. It persists a stable `/etc/machine-id` in `/config/.wechat-cache/machine-id` (so WeChat's built-in auto-login survives container upgrades alongside a fixed `hostname`), verifies any cached `.deb` in `/config/.wechat-cache/`, downloads from Tencent only when the cache is missing or mismatched, refuses to install on a SHA-256 mismatch, and copies the installed WeChat icon into Selkies's web directory at runtime. Warm restarts skip both download and `dpkg`.
- **Hardware video encoding.** The default encoder list is `h264enc,vp9enc,h265enc,vp8enc` with `crf,cbr` rate control and CRF 25. `jpeg` is omitted from the default list so sessions do not fall back to CPU JPEG striping. X11 runs XLibre `Xvfb` with Glamor and DRI3, and browsers decode frames through WebCodecs.
- **Browser-side Chinese IME.** Selkies 2.2.1 injects committed Unicode characters through XTEST keycode overlays rather than shelling out to `xdotool`. You type with your local operating system's Pinyin or CJK input method directly in the browser without installing `fcitx` inside the container.
- **Process supervision and window fitting.** `wechat-supervisor` sets `PR_SET_CHILD_SUBREAPER` and launches `/usr/bin/wechat` in its own session (`setsid`) so stopping or restarting WeChat also reaps `WeChatAppEx` helper processes. Its X11 watcher (`x11rb`) resizes resizable WeChat windows to match Selkies RandR viewport changes and centers fixed-size dialogs such as the QR login window. It never injects synthetic mouse clicks or keystrokes.
- **Health endpoint and optional notifications.** The supervisor serves `/healthz` and `/readyz` on `127.0.0.1:8090` for Docker `HEALTHCHECK` (`wechat-supervisor healthcheck`). Setting `WECHAT_NOTIFY_WEBHOOK_URL` enables a session D-Bus listener that posts notification summaries as JSON, with message bodies redacted by default (`WECHAT_NOTIFY_REDACT_BODY=true`).

## Run with Tailscale (no public IP required)

Browsers require HTTPS (or `localhost`) for WebCodecs decoding, clipboard sync, microphone access, and webcam access. If your host is behind CGNAT or has no public IP, use the Tailscale sidecar to get automatic Let's Encrypt HTTPS and tailnet authentication with no open host ports.

1. In the Tailscale admin console, enable **MagicDNS** and **HTTPS Certificates**, then create an auth key.
2. Copy the environment template and set `TS_AUTHKEY`:

   ```bash
   cp .env.example .env
   ```

   Edit `.env` to set `TS_AUTHKEY=tskey-auth-...` and optionally `TS_HOSTNAME=wechat`.

3. Start the stack with the Tailscale override:

   ```bash
   docker compose -f docker-compose.yml -f compose.tailscale.yml up -d
   ```

4. Wait for the container health check to report `healthy`, then open `https://<TS_HOSTNAME>.<your-tailnet>.ts.net` from a device on your tailnet.

Because Tailscale carries peer-to-peer UDP, you can also add `-f compose.webrtc.yml` to use WebRTC without a public TURN server.

## Run behind a reverse proxy, Cloudflare Tunnel, or LAN HTTPS

Do not expose port `3000` to untrusted networks without TLS and authentication.

1. Copy the environment template and adjust ports, GPU device paths, or locale settings:

   ```bash
   cp .env.example .env
   ```

2. Start the container:

   ```bash
   docker compose up -d
   ```

   `docker-compose.yml` maps host port `3011` (`WECHAT_HTTP_PORT`) to container port `3000` so it does not collide with services already bound to port `3000`.

3. Put an authenticated TLS front end in front of the container:
   - **Reverse proxy.** Forward HTTP and WebSocket connections from a TLS reverse proxy (such as Caddy, Nginx, or Traefik) protected by your authentication layer (SSO, OAuth2 proxy, Basic Auth, or client certificates) to port `3011`.
   - **Cloudflare Tunnel.** Point `cloudflared` at `http://127.0.0.1:3011` and require login through Cloudflare Access if you have no public IP and want access from browsers without Tailscale.
   - **Built-in Basic Auth.** Set `CUSTOM_USER` and `PASSWORD` in `.env` and publish container port `3001` for self-signed HTTPS.

## Enable WebRTC transport

WebSockets over a single TCP port are enabled by default. To opt into UDP WebRTC transport, set `SELKIES_WEBRTC_PUBLIC_IP` and your STUN or TURN variables in `.env`, then layer `compose.webrtc.yml`:

```bash
docker compose -f docker-compose.yml -f compose.webrtc.yml up -d
```

## Build a private local image with WeChat pre-installed

Published GHCR images never contain WeChat. For a private build on your own machine that skips first-boot download, pass `INSTALL_WECHAT_AT_BUILD=true`:

```bash
docker build --build-arg INSTALL_WECHAT_AT_BUILD=true -t local/wechat-selkies-rev:private .
```

Do not push images built with `INSTALL_WECHAT_AT_BUILD=true` to public registries.

## Configuration reference

Set these variables in `.env` or your Compose file. See [`.env.example`](.env.example) for the complete list.

| Variable | Default | Purpose |
| --- | --- | --- |
| `WECHAT_HOSTNAME` | `wechat-selkies` | Stable container hostname so WeChat's built-in auto-login survives image upgrades. |
| `PUID`, `PGID` | `1000` | Owner UID and GID for `/config`. |
| `TZ`, `LC_ALL` | `Asia/Shanghai`, `zh_CN.UTF-8` | Container timezone and locale. |
| `DRINODE`, `DRI_NODE` | `/dev/dri/renderD128` | DRM render nodes for Xvfb Glamor rendering and VA-API encoding. |
| `DISABLE_DRI3` | `false` | Set `true` to disable Xvfb Glamor/DRI3 rendering while troubleshooting a GPU. |
| `PIXELFLUX_WAYLAND` | `false` | Set `true` to test the experimental Smithay and `labwc` Wayland backend. |
| `SELKIES_ENCODER` | `h264enc,vp9enc,h265enc,vp8enc` | Allowed video encoders in preference order. |
| `AUTO_START_WECHAT` | `true` | Launch and supervise WeChat on session start. |
| `WECHAT_WINDOW_AUTOFIT` | `true` | Fit resizable WeChat windows to the browser viewport and center fixed-size dialogs. |
| `WECHAT_SHOW_TRAY` | `true` | Run `stalonetray` in the bottom-right dock corner for restoring WeChat from tray. |
| `WECHAT_DEB_URL`, `WECHAT_DEB_SHA256` | From `versions/wechat.json` | Override the official `.deb` download URL or expected SHA-256 digest. |
| `WECHAT_NOTIFY_WEBHOOK_URL` | Empty (disabled) | Webhook endpoint for D-Bus desktop notification forwarding. |
| `WECHAT_NOTIFY_REDACT_BODY` | `true` | Omit message bodies from forwarded notification payloads. |

## Documentation

- [GPU acceleration guide](docs/gpu-acceleration.md): Intel VA-API (including Proxmox SR-IOV VFs), AMD VA-API, NVIDIA NVENC, software fallback, and verification commands.
- [Architecture notes](docs/architecture.md): base-image comparison, X11/DRI3 versus experimental Wayland tradeoffs, supervisor design, and CI checks.

## Credits

This project was inspired by [nickrunning/wechat-selkies](https://github.com/nickrunning/wechat-selkies), which pioneered running WeChat for Linux in a browser over Selkies. All scripts, the Rust supervisor, container configuration, and documentation in this repository were written from scratch for a WeChat-only, Selkies 2.2+, ToS-safe workflow.

Built on top of:

- [Selkies](https://github.com/selkies-project/selkies) (`selkies`, `pixelflux`, and `pcmflux`)
- [LinuxServer.io `baseimage-selkies`](https://github.com/linuxserver/docker-baseimage-selkies)

## License

MIT. See [`LICENSE`](LICENSE). WeChat is proprietary software owned by Tencent and is downloaded directly from Tencent's official servers by the user at runtime.
