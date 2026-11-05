#!/usr/bin/env bash
#===============================================================================
# build-spk.sh —— 把 bili-sync-up 的 Linux 静态二进制打成群晖 DSM 7 套件 (.spk)
#
# 用法：
#   ./synology/build-spk.sh --tag v3.1.5 --arch x86_64
#   ./synology/build-spk.sh --tag v3.1.5 --arch armv8  --binary dist/bili-sync-rs-Linux-aarch64-musl.tar.gz
#   ./synology/build-spk.sh --tag v3.1.5 --arch x86_64 --ffmpeg-dir /tmp/ffmpeg   # 复用已下载的 ffmpeg
#
# 参数：
#   --tag <tag>          发布 tag（如 v3.1.5），用于拼版本号与默认下载地址
#   --arch <arch>        x86_64 | armv8（armv8 = 上游 aarch64 产物）
#   --binary <path>      本地二进制 tar.gz（不给则按 tag 从 GitHub Release 下载）
#   --ffmpeg-dir <dir>   本地已解压的 ffmpeg 目录（含 ffmpeg/ffprobe；不给则自动下载静态版）
#   --outdir <dir>       产物输出目录（默认 ./dist）
#   --keep              保留中间目录（排障用）
#
# 产物：<outdir>/bili-sync-up_<arch>-<version>.spk
#
# 依赖：bash、tar、gzip、xz、curl（CI 的 ubuntu runner 全都有）
#===============================================================================
set -euo pipefail

APP="bili-sync-up"
DISPLAY_NAME="Bili Sync Up"
REPO="${BILI_SYNC_REPO:-NeeYoonc/bili-sync-up}"

# ffmpeg 静态构建来源（Pinned 到 johnvansickle 的 release 通道；可用环境变量覆盖）
FFMPEG_AMD64_URL="${FFMPEG_AMD64_URL:-https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz}"
FFMPEG_ARM64_URL="${FFMPEG_ARM64_URL:-https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-arm64-static.tar.xz}"

TAG=""
ARCH=""
BINARY=""
FFMPEG_DIR=""
OUTDIR="./dist"
KEEP="no"

log() { printf '\033[36m▶ %s\033[0m\n' "$*"; }
die() { printf '\033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --tag)        TAG="${2:?--tag 需要值}"; shift 2 ;;
    --arch)       ARCH="${2:?--arch 需要值}"; shift 2 ;;
    --binary)     BINARY="${2:?--binary 需要值}"; shift 2 ;;
    --ffmpeg-dir) FFMPEG_DIR="${2:?--ffmpeg-dir 需要值}"; shift 2 ;;
    --outdir)     OUTDIR="${2:?--outdir 需要值}"; shift 2 ;;
    --keep)       KEEP="yes"; shift ;;
    -h|--help)    sed -n '2,25p' "$0"; exit 0 ;;
    *)            die "未知参数: $1" ;;
  esac
done

[ -n "$TAG" ]  || die "缺少 --tag（例：--tag v3.1.5）"
[ -n "$ARCH" ] || die "缺少 --arch（x86_64 或 armv8）"

# ── 架构映射：SPK arch / 上游产物 / ffmpeg 来源 ──────────────────────────────
case "$ARCH" in
  x86_64)
    ASSET="bili-sync-rs-Linux-x86_64-musl.tar.gz"
    FFMPEG_URL="$FFMPEG_AMD64_URL"
    ;;
  armv8)
    ASSET="bili-sync-rs-Linux-aarch64-musl.tar.gz"
    FFMPEG_URL="$FFMPEG_ARM64_URL"
    ;;
  *) die "不支持的 arch: $ARCH（仅 x86_64 / armv8）" ;;
esac

# ── 版本号：v3.1.5 → 3.1.5；DSM 要求纯数字点分（去掉可能的预发布后缀） ──────
VERSION="$(printf '%s' "$TAG" | sed -E 's/^[vV]//; s/[-+].*$//')"
case "$VERSION" in
  [0-9]*.[0-9]*) : ;;
  *) die "无法从 tag 解析版本号: $TAG（得到 '$VERSION'）" ;;
esac
# 四段版本（如 v3.1.4.1）→ DSM 的 build 形式 3.1.4-1，避免 INFO version 不合法
if printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$'; then
  VERSION="$(printf '%s' "$VERSION" | sed -E 's/^([0-9]+\.[0-9]+\.[0-9]+)\.([0-9]+)$/\1-\2/')"
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(mktemp -d)"
trap '[ "$KEEP" = "yes" ] || rm -rf "$WORK"' EXIT
mkdir -p "$OUTDIR"

log "构建 $APP $VERSION ($ARCH)  repo=$REPO tag=$TAG"

# ── 1. 取 bili-sync-rs 二进制 ─────────────────────────────────────────────────
if [ -n "$BINARY" ]; then
  [ -f "$BINARY" ] || die "找不到 --binary 指定的文件: $BINARY"
  cp "$BINARY" "$WORK/$ASSET"
else
  URL="https://github.com/$REPO/releases/download/$TAG/$ASSET"
  log "下载二进制: $URL"
  curl -fsSL --retry 3 --connect-timeout 15 -o "$WORK/$ASSET" "$URL" \
    || die "下载失败（确认该 tag 的 Release 有 $ASSET）"
fi

mkdir -p "$WORK/app/bin"
tar -xzf "$WORK/$ASSET" -C "$WORK/app/bin"
[ -f "$WORK/app/bin/bili-sync-rs" ] || die "压缩包里没有 bili-sync-rs"
chmod +x "$WORK/app/bin/bili-sync-rs"
log "二进制: $(du -h "$WORK/app/bin/bili-sync-rs" | cut -f1)"

# ── 2. 取 ffmpeg / ffprobe（B 站音视频分离流合并必需） ────────────────────────
if [ -n "$FFMPEG_DIR" ]; then
  [ -x "$FFMPEG_DIR/ffmpeg" ] || die "$FFMPEG_DIR 下没有可执行 ffmpeg"
  cp "$FFMPEG_DIR/ffmpeg" "$WORK/app/bin/ffmpeg"
  [ -x "$FFMPEG_DIR/ffprobe" ] && cp "$FFMPEG_DIR/ffprobe" "$WORK/app/bin/ffprobe" || true
else
  log "下载静态 ffmpeg: $FFMPEG_URL"
  curl -fsSL --retry 3 --connect-timeout 20 -o "$WORK/ffmpeg.tar.xz" "$FFMPEG_URL" \
    || die "ffmpeg 下载失败"
  mkdir -p "$WORK/ffmpeg"
  tar -xJf "$WORK/ffmpeg.tar.xz" -C "$WORK/ffmpeg" --strip-components=1
  [ -f "$WORK/ffmpeg/ffmpeg" ] || die "解压后找不到 ffmpeg"
  cp "$WORK/ffmpeg/ffmpeg" "$WORK/app/bin/ffmpeg"
  [ -f "$WORK/ffmpeg/ffprobe" ] && cp "$WORK/ffmpeg/ffprobe" "$WORK/app/bin/ffprobe" || true
fi
chmod +x "$WORK/app/bin/"ffmpeg* 2>/dev/null || true
log "ffmpeg: $(du -h "$WORK/app/bin/ffmpeg" | cut -f1)"

# ── 3. 应用体内的启动包装（供 start-stop-status 与 SSH 手动启动复用） ────────
cat > "$WORK/app/bin/start.sh" <<'START'
#!/bin/sh
# 手动/服务启动统一入口：ffmpeg 走包内 bin，配置走 $BILI_SYNC_CONFIG_DIR
set -e
BIN_DIR="$(cd "$(dirname "$0")" && pwd)"
export PATH="$BIN_DIR:$PATH"
export HOME="${HOME:-/var/packages/bili-sync-up/var/home}"
export BILI_SYNC_CONFIG_DIR="${BILI_SYNC_CONFIG_DIR:-$HOME/.config/bili-sync}"
export TZ="${TZ:-Asia/Shanghai}"
export RUST_LOG="${RUST_LOG:-None,bili_sync=info}"
mkdir -p "$HOME" "$BILI_SYNC_CONFIG_DIR"
exec "$BIN_DIR/bili-sync-rs" "$@"
START
chmod +x "$WORK/app/bin/start.sh"

# ── 4. package.tgz（装到 /var/packages/<pkg>/target） ─────────────────────────
tar -czf "$WORK/package.tgz" -C "$WORK/app" .
log "package.tgz: $(du -h "$WORK/package.tgz" | cut -f1)"

# ── 5. SPK 骨架：INFO / scripts / conf ───────────────────────────────────────
SPK="$WORK/spk"
mkdir -p "$SPK/scripts" "$SPK/conf"
cp "$SCRIPT_DIR/template/INFO.in" "$SPK/INFO"
cp "$SCRIPT_DIR/template/scripts/"* "$SPK/scripts/"
cp "$SCRIPT_DIR/template/conf/privilege" "$SPK/conf/privilege"
cp "$SCRIPT_DIR/template/conf/resource"  "$SPK/conf/resource"
# 安装向导（端口 / Token / 防火墙）
cp -r "$SCRIPT_DIR/template/WIZARD_UIFILES" "$SPK/WIZARD_UIFILES"
# 图标放【包根】而非 conf/ —— 与 DSM 规范及已验证可装的 dsh 包一致
cp "$SCRIPT_DIR/template/PACKAGE_ICON.PNG"     "$SPK/PACKAGE_ICON.PNG"
cp "$SCRIPT_DIR/template/PACKAGE_ICON_256.PNG" "$SPK/PACKAGE_ICON_256.PNG"
cp "$WORK/package.tgz" "$SPK/package.tgz"
chmod 755 "$SPK/scripts/"*
chmod 644 "$SPK/INFO" "$SPK/conf/privilege" "$SPK/conf/resource" "$SPK/WIZARD_UIFILES/install_uifile"

# INFO 占位符替换
sed -i \
  -e "s|@APP@|$APP|g" \
  -e "s|@DISPLAY_NAME@|$DISPLAY_NAME|g" \
  -e "s|@VERSION@|$VERSION|g" \
  -e "s|@ARCH@|$ARCH|g" \
  -e "s|@REPO@|$REPO|g" \
  "$SPK/INFO"

# scripts 里的占位符替换（应用名/端口等）
for f in "$SPK/scripts/"*; do
  sed -i -e "s|@APP@|$APP|g" -e "s|@VERSION@|$VERSION|g" "$f"
done

# ── 6. 打 SPK ────────────────────────────────────────────────────────────────
# ★ 外层必须是【未压缩 tar】且【条目不带 ./ 前缀】（DSM 规范）。
#   写成 `tar -cf x.spk .` 会得到 ./INFO、./conf/... → DSM 直接判
#   「套件文件格式不正确。请联系套件开发人员。」（2026-11-05 实测踩过）
OUT="$OUTDIR/${APP}_${ARCH}-${VERSION}.spk"
( cd "$SPK" && tar --owner=0 --group=0 -cf "$WORK/out.spk" \
    INFO PACKAGE_ICON.PNG PACKAGE_ICON_256.PNG conf scripts WIZARD_UIFILES package.tgz )
mv "$WORK/out.spk" "$OUT"

# ── 7. 自检 ─────────────────────────────────────────────────────────────────
log "自检"
_refs="$(tar -tf "$OUT")"
for f in INFO PACKAGE_ICON.PNG PACKAGE_ICON_256.PNG package.tgz \
         scripts/start-stop-status scripts/postinst conf/privilege conf/resource \
         WIZARD_UIFILES/install_uifile; do
  printf '%s\n' "$_refs" | grep -qx "$f" || die "SPK 缺少 $f"
done
printf '%s\n' "$_refs" | grep -q '^\./' && die "SPK 条目带了 ./ 前缀（DSM 会判格式错误）"
_pkg="$(tar -xOf "$OUT" package.tgz | tar -tzf -)"
printf '%s\n' "$_pkg" | grep -qx "./bin/bili-sync-rs" || die "package.tgz 里没有 bin/bili-sync-rs"
printf '%s\n' "$_pkg" | grep -qx "./bin/ffmpeg"      || die "package.tgz 里没有 bin/ffmpeg"

log "完成: $OUT（$(du -h "$OUT" | cut -f1)）"
echo "$OUT"
