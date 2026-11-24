# Deployment guide

Browsers enable WebCodecs hardware decoding, clipboard sync, microphone, and webcam only on HTTPS or `localhost` origins. A plain `http://<LAN-IP>:3011` session works, but without those features. Choose one of the HTTPS options below.

Neither port `3000` (host `3011`) nor port `3001` (host `3012`) requires authentication by default. Never expose them to untrusted networks without TLS and authentication.

All options start from a clone of this repository and a copy of the environment template:

```bash
git clone https://github.com/LetterXbox/wechat-selkies-rev.git
cd wechat-selkies-rev
cp .env.example .env
```

If your host has no `/dev/dri` GPU, remove the `devices` block and unset `DRINODE` and `DRI_NODE` before starting; see the [GPU acceleration guide](gpu-acceleration.md#4-software-fallback-no-gpu-or-troubleshooting).

## Tailscale (no public IP required)

The Tailscale sidecar gives you automatic Let's Encrypt HTTPS and tailnet-only access with no published host ports. It works behind CGNAT and requires Docker Compose v2.24 or newer.

1. In the Tailscale admin console, enable **MagicDNS** and **HTTPS Certificates**, then create an auth key.
2. In `.env`, set `TS_AUTHKEY=tskey-auth-...` and optionally `TS_HOSTNAME=wechat`.
3. Start the stack with the Tailscale override:

   ```bash
   docker compose -f docker-compose.yml -f compose.tailscale.yml up -d
   ```

4. Wait for `docker compose ps` to report `healthy`, then open `https://<TS_HOSTNAME>.<your-tailnet>.ts.net` from a device on your tailnet.

`compose.tailscale.yml` runs Tailscale in userspace mode (`TS_USERSPACE=true`) and proxies HTTPS over TCP through `tailscale/serve.json`. To combine Tailscale with peer-to-peer UDP [WebRTC](#webrtc-transport), switch the sidecar to kernel networking (`TS_USERSPACE=false` with `/dev/net/tun` and `NET_ADMIN`) and add `-f compose.webrtc.yml`.

## Reverse proxy, Cloudflare Tunnel, or built-in HTTPS

Start the container:

```bash
docker compose up -d
```

`docker-compose.yml` maps host port `3011` (`WECHAT_HTTP_PORT`) to container HTTP port `3000`, and host port `3012` (`WECHAT_HTTPS_PORT`) to container HTTPS port `3001`. Put one of these in front of it:

- **Reverse proxy.** Forward HTTP and WebSocket connections from a TLS reverse proxy (Caddy, Nginx, or Traefik) to port `3011`, protected by an authentication layer such as SSO, an OAuth2 proxy, Basic Auth, or client certificates.
- **Cloudflare Tunnel.** Point `cloudflared` at `http://127.0.0.1:3011` and require login through Cloudflare Access.
- **Built-in HTTPS.** Open `https://<host>:3012`, which serves a self-signed certificate. Set `PASSWORD` (and optionally `CUSTOM_USER`, which defaults to `abc`) in `.env` to require Basic Auth.

## WebRTC transport

WebSockets over a single TCP port are the default. To opt into UDP WebRTC transport, set `SELKIES_WEBRTC_PUBLIC_IP` and your STUN or TURN variables in `.env`, then layer `compose.webrtc.yml`:

```bash
docker compose -f docker-compose.yml -f compose.webrtc.yml up -d
```

## Private image with WeChat pre-installed

Published GHCR images never contain WeChat. For a private build on your own machine that skips the first-boot download, pass `INSTALL_WECHAT_AT_BUILD=true`:

```bash
docker build --build-arg INSTALL_WECHAT_AT_BUILD=true -t local/wechat-selkies-rev:private .
```

Do not push images built with `INSTALL_WECHAT_AT_BUILD=true` to public registries.

## Configuration

Set these variables in `.env`; `docker-compose.yml` forwards them to the container. See [`.env.example`](../.env.example) for the complete `.env` template, including Tailscale and WebRTC settings.

| Variable | Default | Purpose |
| --- | --- | --- |
| `WECHAT_HOSTNAME` | `wechat-selkies` | Stable container hostname so WeChat's built-in auto-login survives image upgrades. |
| `WECHAT_HTTP_PORT`, `WECHAT_HTTPS_PORT` | `3011`, `3012` | Host ports for container HTTP `3000` and HTTPS `3001`. |
| `CUSTOM_USER`, `PASSWORD` | `abc`, empty | Enable Basic Auth on the container's built-in nginx proxy when `PASSWORD` is set. |
| `PUID`, `PGID` | `1000` | Owner UID and GID for `/config`. |
| `TZ`, `LC_ALL` | `Asia/Shanghai`, `zh_CN.UTF-8` | Container timezone and locale. |
| `DRINODE`, `DRI_NODE` | `/dev/dri/renderD128` | DRM render nodes for Xvfb Glamor rendering and VA-API encoding. |
| `DISABLE_DRI3` | `false` | Set `true` to disable Xvfb Glamor/DRI3 rendering while troubleshooting a GPU. |
| `PIXELFLUX_WAYLAND` | `false` | Set `true` to test the experimental Smithay and `labwc` Wayland backend. |
| `SELKIES_ENCODER` | `h264enc,vp9enc,h265enc,vp8enc` | Allowed video encoders in preference order. |
| `AUTO_START_WECHAT` | `true` | Launch and supervise WeChat on session start. |
| `WECHAT_WINDOW_AUTOFIT` | `true` | Fit resizable WeChat windows to the browser viewport and center fixed-size dialogs. |
| `WECHAT_SHOW_TRAY` | `true` | Run `stalonetray` in the top-left dock corner for restoring WeChat from the tray. |
| `WECHAT_DEB_URL`, `WECHAT_DEB_SHA256` | From `versions/wechat.json` | Override the official `.deb` download URL or expected SHA-256 digest. |
| `WECHAT_ALLOW_UNVERIFIED_LATEST` | `false` | Allow installing a `.deb` whose SHA-256 does not match `versions/wechat.json`. |
| `WECHAT_NOTIFY_WEBHOOK_URL` | Empty (disabled) | Webhook URL that receives desktop notification summaries as JSON. |
| `WECHAT_NOTIFY_REDACT_BODY` | `true` | Omit message bodies from forwarded notification payloads. |

For GPU device setup, see the [GPU acceleration guide](gpu-acceleration.md).
