#!/bin/sh
# 10-cdp-socat.sh — 把容器内 127.0.0.1:9222 的 Chrome DevTools(CDP) 转发到 0.0.0.0:9223
#
# 为什么需要它：Chromium M113+ 移除了 --remote-debugging-address，CDP 固定只
# 监听 127.0.0.1。容器端口映射(9222:9222)只能到达容器 eth0 的 9222，而进程
# 监听在 loopback 上，外部连接会被直接 RST。用 socat 在容器内做一次转发：
#   外部 -> 容器 eth0:9223 -> 127.0.0.1:9222(CDP)

# 1) 安装 socat（兼容 Alpine 的 apk 与 Debian/Ubuntu 的 apt-get）
if ! command -v socat >/dev/null 2>&1; then
  if command -v apk >/dev/null 2>&1; then
    apk add --no-cache socat >/dev/null 2>&1
  elif command -v apt-get >/dev/null 2>&1; then
    apt-get update -qq >/dev/null 2>&1
    apt-get install -y --no-install-recommends socat >/dev/null 2>&1
  fi
fi

# 2) 清理可能残留的旧转发进程（容器重启时）
pkill -f "socat.*TCP-LISTEN:9223" >/dev/null 2>&1 || true

# 3) 启动转发。socat 会在有连接到达时才去连 127.0.0.1:9222，
#    所以即使 chromium 还没起来也不影响监听。
if command -v socat >/dev/null 2>&1; then
  nohup socat TCP-LISTEN:9223,fork,reuseaddr TCP:127.0.0.1:9222 >/dev/null 2>&1 &
  disown 2>/dev/null || true
fi