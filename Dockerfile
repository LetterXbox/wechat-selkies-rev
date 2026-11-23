# syntax=docker/dockerfile:1

FROM --platform=$BUILDPLATFORM rust:1-slim-trixie AS supervisor-builder

ARG TARGETARCH

WORKDIR /build

RUN if [ "${TARGETARCH}" = "arm64" ] && [ "$(uname -m)" != "aarch64" ]; then \
      apt-get update && \
      DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        gcc-aarch64-linux-gnu \
        libc6-dev-arm64-cross && \
      rm -rf /var/lib/apt/lists/* && \
      rustup target add aarch64-unknown-linux-gnu; \
    fi

COPY crates/wechat-supervisor/Cargo.toml crates/wechat-supervisor/Cargo.lock ./
COPY crates/wechat-supervisor/src ./src

RUN if [ "${TARGETARCH}" = "arm64" ] && [ "$(uname -m)" != "aarch64" ]; then \
      CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
      CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
      AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
      cargo build --release --locked --target aarch64-unknown-linux-gnu && \
      cp target/aarch64-unknown-linux-gnu/release/wechat-supervisor /build/wechat-supervisor; \
    else \
      cargo build --release --locked && \
      cp target/release/wechat-supervisor /build/wechat-supervisor; \
    fi

FROM ghcr.io/linuxserver/baseimage-selkies:debiantrixie@sha256:793fc3628ad6e089aecfd9f862de3ad5d9df05f1db783363c812f77d6541ccc3

ARG TARGETARCH
ARG INSTALL_WECHAT_AT_BUILD=false

LABEL org.opencontainers.image.title="WeChat Selkies Rev" \
      org.opencontainers.image.description="Clean-room WeChat-only Selkies 2.2+ container with VA-API defaults and ToS-safe first-boot WeChat verification" \
      org.opencontainers.image.source="https://github.com/LetterXbox/wechat-selkies-rev" \
      org.opencontainers.image.licenses="MIT"

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      desktop-file-utils \
      libasound2t64 \
      libatomic1 \
      libcups2t64 \
      libnspr4 \
      libpangocairo-1.0-0 \
      libx11-xcb1 \
      libxcb-glx0 \
      libxcb-randr0 \
      libxcb-render0 \
      libxcb-shape0 \
      libxcb-shm0 \
      libxcb-sync1 \
      libxcb-util1 \
      libxcb-xfixes0 \
      libxcb-xkb1 \
      libxcomposite1 \
      libxdamage1 \
      libxi6 \
      libxrandr2 \
      libxrender1 \
      libxss1 \
      shared-mime-info \
      stalonetray && \
    apt-get autoclean && \
    rm -rf /var/lib/apt/lists/* /var/tmp/* /tmp/*

# Allow Openbox dock to avoid reserving full-height struts for stalonetray
RUN sed -i '/<dock>/,/<\/dock>/s/<noStrut>no<\/noStrut>/<noStrut>yes<\/noStrut>/' /etc/xdg/openbox/rc.xml

COPY --from=supervisor-builder /build/wechat-supervisor /usr/local/bin/wechat-supervisor
COPY versions/wechat.json /etc/wechat-selkies/wechat.json
COPY root/ /

RUN chmod 0755 \
      /usr/local/bin/wechat-supervisor \
      /etc/s6-overlay/s6-rc.d/init-wechat-bootstrap/run \
      /defaults/autostart \
      /defaults/autostart_wayland && \
    chmod 0644 \
      /etc/wechat-selkies/wechat.json \
      /defaults/menu.xml \
      /defaults/menu_wayland.xml

# Optional local-only build path; published GHCR builds keep INSTALL_WECHAT_AT_BUILD=false
RUN if [ "${INSTALL_WECHAT_AT_BUILD}" = "true" ]; then \
      /usr/local/bin/wechat-supervisor bootstrap \
        --cache-dir /tmp/wechat-build-cache \
        --manifest /etc/wechat-selkies/wechat.json && \
      rm -rf /tmp/wechat-build-cache; \
    fi

ENV TITLE="WeChat" \
    TZ="Asia/Shanghai" \
    LC_ALL="zh_CN.UTF-8" \
    START_DOCKER="false" \
    NO_GAMEPAD="true" \
    SELKIES_PRINTING_ENABLED="false" \
    DISABLE_DRI3="false" \
    PIXELFLUX_WAYLAND="false" \
    SELKIES_ENCODER="h264enc,vp9enc,h265enc,vp8enc" \
    SELKIES_RATE_CONTROL_MODE="crf,cbr" \
    SELKIES_VIDEO_CRF="25" \
    AUTO_START_WECHAT="true" \
    WECHAT_WINDOW_AUTOFIT="true" \
    WECHAT_SHOW_TRAY="true" \
    WECHAT_HEALTH_PORT="8090" \
    WECHAT_MANIFEST_PATH="/etc/wechat-selkies/wechat.json" \
    WECHAT_CACHE_DIR="/config/.wechat-cache"

HEALTHCHECK --interval=30s --timeout=5s --start-period=90s --retries=3 \
    CMD ["/usr/local/bin/wechat-supervisor", "healthcheck"]

EXPOSE 3000 3001
VOLUME /config
