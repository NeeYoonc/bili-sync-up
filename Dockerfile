FROM alpine:3.22 AS base

ARG TARGETPLATFORM
ARG BILI_SYNC_RELEASE_CHANNEL=stable

WORKDIR /app

RUN apk update && apk add --no-cache \
    ca-certificates \
    tzdata \
    ffmpeg \
    quickjs

# 复制所有Linux二进制文件
COPY ./bili-sync-rs-Linux-*.tar.gz ./

# 根据目标平台解压对应的二进制文件
RUN if [ "$TARGETPLATFORM" = "linux/amd64" ]; then \
    tar xzvf ./bili-sync-rs-Linux-x86_64-musl.tar.gz; \
    elif [ "$TARGETPLATFORM" = "linux/arm64" ]; then \
    tar xzvf ./bili-sync-rs-Linux-aarch64-musl.tar.gz; \
    else \
    echo "Unsupported platform: $TARGETPLATFORM" && exit 1; \
    fi

# 写入镜像构建时间（用于 /api/updates/beta 的本地时间对比，避免“编译时间 < 推送时间”导致误判）
RUN date -u +"%Y-%m-%dT%H:%M:%SZ" > /app/image-built-at.txt
RUN echo -n "$BILI_SYNC_RELEASE_CHANNEL" > /app/release-channel.txt

# 清理压缩文件并设置权限
RUN rm -f ./bili-sync-rs-Linux-*.tar.gz && \
    chmod +x ./bili-sync-rs

FROM scratch

WORKDIR /app

ENV LANG=zh_CN.UTF-8 \
    TZ=Asia/Shanghai \
    HOME=/app \
    BILI_SYNC_CONTAINER=1 \
    BILI_SYNC_YTDLP_JS_RUNTIME=/usr/bin/qjs \
    RUST_BACKTRACE=1 \
    RUST_LOG=None,bili_sync=info

COPY --from=base / /

ENTRYPOINT [ "/app/bili-sync-rs" ]

VOLUME [ "/app/.config/bili-sync" ]
