# Architecture

## 1. Base Image Selection

Evaluated on 2026-11-23 from live GHCR manifests and upstream source:

| Dimension | `ghcr.io/linuxserver/baseimage-selkies:debiantrixie` (`f7632021-ls153`) | `ghcr.io/selkies-project/selkies/base:2.2.1-debiantrixie` | `ghcr.io/selkies-project/selkies/base:main-debiantrixie` |
| --- | --- | --- | --- |
| Compressed amd64 size | **867 MB** (14 layers) | 1301 MB (28 layers) | 625 MB (21 layers) |
| Architectures | `amd64`, `arm64` | `amd64`, `arm64` | `amd64`, `arm64` |
| Selkies stack | Stable **2.2.1** (`pixelflux 2.2.3`, `pcmflux 2.2.1`) | Stable **2.2.1** | Unreleased **2.3.0-dev** Rust server |
| User & `/dev/dri` | `PUID`/`PGID` remapping + automatic `/dev/dri` GID matching (`init-video`) | Fixed UID 1000 (`ubuntu`), `selkies-privileged-files` + host `--group-add` | Same as 2.2.1 |
| Volume & proxy port | `/config`, nginx HTTP `:3000` (auth off by default) / HTTPS `:3001` | `/home/ubuntu`, direct `:8080` (HTTPS + Basic auth on by default) | Same as 2.2.1 |
| Window manager | Openbox (X11) + patched `labwc` (Wayland) | None included in 2.2.1 | Built-in `SELKIES_WM` (pending 2.3.0 release) |
| WebRTC | Opt-in (`SELKIES_MODE=webrtc` or TURN/STUN env vars) | Dual-mode + embedded coTURN | Same |

**Decision:** Build on **`ghcr.io/linuxserver/baseimage-selkies:debiantrixie`** (digest-pinned in `Dockerfile`) with `START_DOCKER=false`, `NO_GAMEPAD=true`, and `SELKIES_PRINTING_ENABLED=false`.

- Smaller than `selkies/base:2.2.1-debiantrixie`, ships stable Selkies 2.2.1, handles `PUID`/`PGID` and `/dev/dri/renderD128` group permissions automatically, and matches standard `/config` + HTTP `:3000` / HTTPS `:3001` deployments (behind any reverse proxy with external auth, or standalone with `CUSTOM_USER`/`PASSWORD`).
- Because all WeChat logic lives in the standalone `wechat-supervisor` binary and a thin s6/autostart overlay, migrating to `ghcr.io/selkies-project/selkies/base:2.3.0-debiantrixie` once Selkies 2.3.0 releases a stable tag is a localized Dockerfile change.

## 2. Display Backend: X11/DRI3 Default vs Experimental Wayland

- **Default: X11 + DRI3 (`PIXELFLUX_WAYLAND=false`, `DISABLE_DRI3=false`) with Openbox.**
  - Uses XLibre `Xvfb -glamor -dri /dev/dri/renderD128` for GPU-accelerated rendering and Intel/AMD VA-API (or NVIDIA NVENC) for H.264/VP9 hardware encoding.
  - Selkies 2.2.1's XTEST Unicode keycode-overlay injector reliably delivers browser-side Chinese IME commits directly into WeChat 4.1.x with no in-container `fcitx` daemon required.
  - WeChat Linux 4.1.x (Qt/Chromium hybrid) natively targets X11; pure X11 lets `wechat-supervisor` read `WM_NORMAL_HINTS` and fit resizable windows to RandR viewport changes cleanly.
- **Opt-in experimental Wayland (`PIXELFLUX_WAYLAND=true`):**
  - Runs Selkies's Smithay headless Wayland capture compositor with nested `labwc`.
  - `root/defaults/autostart_wayland` sets `QT_QPA_PLATFORM=xcb` so WeChat runs over XWayland while Selkies captures DMA-BUF frames. Kept opt-in until WeChat native Wayland + `text-input-v3` and XWayland sub-256 overlay keycodes match X11 stability.

## 3. Video Codecs & Transport Modes

- **Encoder defaults:** `SELKIES_ENCODER="h264enc,vp9enc,h265enc,vp8enc"`, `SELKIES_RATE_CONTROL_MODE="crf,cbr"`, `SELKIES_VIDEO_CRF="25"`. Omitting `jpeg` prevents silent fallback to CPU-striped JPEG; hardware VA-API/NVENC encodes H.264/VP9 and clients decode via WebCodecs. See [`gpu-acceleration.md`](gpu-acceleration.md) for host setup and verification.
- **WebSocket default:** Single HTTP port `3000` (and HTTPS `3001`), zero UDP ports required behind a TLS reverse proxy with external authentication or using built-in `CUSTOM_USER`/`PASSWORD` Basic Auth.
- **Opt-in WebRTC:** Enabled via `compose.webrtc.yml` (`SELKIES_MODE=webrtc`, `SELKIES_ENABLE_DUAL_MODE=true`) with external STUN/TURN or `SELKIES_WEBRTC_PUBLIC_IP`.

## 4. ToS-Safe First-Boot Bootstrap & Rust Supervisor

`crates/wechat-supervisor` compiles to one lean synchronous Rust binary (`ureq` with `rustls`, `sha2`, `x11rb`, `clap`, `libc`; no Tokio runtime) and replaces legacy bash/Python wrapper scripts:

1. **`init-wechat-bootstrap` (s6-rc oneshot, root, after `init-selkies-end` and before `svc-de`):**
   - Runs `wechat-supervisor bootstrap`.
   - Persists a stable `/etc/machine-id` in `/config/.wechat-cache/machine-id`. Together with a fixed container `hostname`, this lets WeChat's built-in auto-login survive image upgrades.
   - Reads `/etc/wechat-selkies/wechat.json` (or `WECHAT_DEB_URL` / `WECHAT_DEB_SHA256` overrides).
   - Verifies `/config/.wechat-cache/wechat-<arch>-<sha256_prefix>.deb` locally if present; otherwise streams from `dldir1v6.qq.com` to a temp file, hashes on the fly, and atomically moves into `/config/.wechat-cache/` only on exact SHA-256 match (fail-closed unless `WECHAT_ALLOW_UNVERIFIED_LATEST=true` is explicitly set).
   - Skips `dpkg -i` when `/usr/bin/wechat` and `/var/lib/wechat-selkies/installed.sha256` already match.
   - Copies WeChat's installed `wechat.png` to `/usr/share/selkies/web/icon.png` and `favicon.ico` at runtime so no Tencent artwork is baked into the published image.
2. **`wechat-supervisor run` (session user `abc`, launched from Openbox/labwc autostart):**
   - Calls `prctl(PR_SET_CHILD_SUBREAPER, 1)` and spawns `/usr/bin/wechat` in its own process group (`setsid`), ensuring clean teardown (`SIGTERM` -> grace period -> `SIGKILL` to process group + reaped descendants) with zero orphan `WeChatAppEx` helpers.
   - Watches X11 RandR + `_NET_CLIENT_LIST` via `x11rb` to fit resizable main WeChat windows to the browser viewport and center fixed-size dialogs (like the QR login window). Never sends synthetic clicks or keystrokes.
   - Serves `127.0.0.1:${WECHAT_HEALTH_PORT}/healthz` and `/readyz` for Docker `HEALTHCHECK` (`wechat-supervisor healthcheck`).
   - Optionally runs a session D-Bus `org.freedesktop.Notifications` bridge when `WECHAT_NOTIFY_WEBHOOK_URL` is configured (message body redacted by default via `WECHAT_NOTIFY_REDACT_BODY=true`).

## 5. CI/CD & Upstream Tracking

- **`.github/workflows/build-publish.yml`:** Runs `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test`; builds multi-arch (`linux/amd64`, `linux/arm64`) images via Docker Buildx; verifies the built image contains no `/usr/bin/wechat` or `/opt/wechat` binary; pushes to GHCR on `main` and version tags.
- **`.github/workflows/track-wechat.yml`:** Runs daily (and on manual dispatch), executes `scripts/check-upstream-wechat.py` against Tencent's official URLs, and opens a pull request updating `versions/wechat.json` whenever upstream ships a new version or SHA-256.
