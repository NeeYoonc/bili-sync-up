FROM alpine:3.20 AS unpack

ARG TARGETPLATFORM
ARG BILI_SYNC_RELEASE_CHANNEL=stable

WORKDIR /app

COPY ./bili-sync-rs-Linux-*.tar.gz ./

RUN if [ "$TARGETPLATFORM" = "linux/amd64" ]; then \
        tar xzvf ./bili-sync-rs-Linux-x86_64-musl.tar.gz; \
    elif [ "$TARGETPLATFORM" = "linux/arm64" ]; then \
        tar xzvf ./bili-sync-rs-Linux-aarch64-musl.tar.gz; \
    else \
        echo "Unsupported platform: $TARGETPLATFORM" && exit 1; \
    fi && \
    date -u +"%Y-%m-%dT%H:%M:%SZ" > /app/image-built-at.txt && \
    echo -n "$BILI_SYNC_RELEASE_CHANNEL" > /app/release-channel.txt && \
    chmod +x /app/bili-sync-rs

# Chromium、图形桌面和 HTTPS 登录入口与 bili-sync 放在同一个容器中。
# 用户只需要启动一个容器；Chromium 资料与 bili-sync 配置分别持久化。
FROM lscr.io/linuxserver/chromium:latest

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        ffmpeg \
        tzdata && \
    apt-get autoclean && \
    rm -rf /var/lib/apt/lists/* /var/tmp/* /tmp/*

WORKDIR /app

ENV TZ=Asia/Shanghai \
    BILI_SYNC_CONTAINER=1 \
    BILI_SYNC_YTDLP_RUNTIME_LIBC=glibc \
    BILI_SYNC_YOUTUBE_LOGIN_BROWSER_URL=http://127.0.0.1:9222 \
    BILI_SYNC_YOUTUBE_LOGIN_PUBLIC_URL=auto \
    BILI_SYNC_YOUTUBE_LOGIN_PUBLIC_PORT=3001 \
    RUST_BACKTRACE=1 \
    RUST_LOG=None,bili_sync=info \
    TITLE="Bili Sync YouTube Login" \
    CUSTOM_USER=bili-sync \
    PASSWORD=bili-sync \
    HARDEN_DESKTOP=true \
    CHROME_CLI="--remote-debugging-address=127.0.0.1 --remote-debugging-port=9222 --remote-allow-origins=* --user-data-dir=/config/youtube-profile --no-first-run --no-default-browser-check https://accounts.google.com/ServiceLogin?service=youtube"

COPY --from=unpack /app/bili-sync-rs /app/bili-sync-rs
COPY --from=unpack /app/image-built-at.txt /app/image-built-at.txt
COPY --from=unpack /app/release-channel.txt /app/release-channel.txt
COPY docker/bili-sync-service /custom-services.d/bili-sync
COPY docker/entrypoint /entrypoint
COPY docker/healthcheck /healthcheck

RUN chmod +x /app/bili-sync-rs /custom-services.d/bili-sync /entrypoint /healthcheck

ENTRYPOINT [ "/entrypoint" ]

HEALTHCHECK --interval=30s --timeout=15s --start-period=90s --retries=3 CMD [ "/healthcheck" ]

EXPOSE 12345 3001

VOLUME [ "/app/.config/bili-sync", "/config" ]
