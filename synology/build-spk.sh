#!/usr/bin/env bash
#===============================================================================
# build-spk.sh —— 把 bili-sync-up 的 Linux 静态二进制打成群晖 DSM 7 套件 (.spk)
#
# 依赖策略（2026-11-14 起）：**不再内置 ffmpeg**，改为强依赖套件源里的 FFmpeg 套件
#   · INFO 里 depends="ffmpeg7"（可用环境变量 FFMPEG_DEP 覆盖，如 ffmpeg8）
#   · postinst 探测已安装的 FFmpeg 套件目录，写入应用配置项 ffmpeg_path
#   · start.sh 再把 ffmpeg 套件的 bin 加进 PATH 作为兜底
#   → 套件体积从 ~73MB 降到 ~20MB
#
# 用法：
#   ./synology/build-spk.sh --tag v3.1.6 --arch x86_64
#   ./synology/build-spk.sh --tag v3.1.6 --arch armv8 --binary dist/bili-sync-rs-Linux-aarch64-musl.tar.gz
#   FFMPEG_DEP=ffmpeg8 ./synology/build-spk.sh --tag v3.1.6 --arch x86_64
#
# 参数：
#   --tag <tag>        发布 tag（如 v3.1.6），用于拼版本号与默认下载地址
#   --arch <arch>      x86_64 | armv8（armv8 = 上游 aarch64 产物）
#   --binary <path>    本地二进制 tar.gz（不给则按 tag 从 GitHub Release 下载）
#   --outdir <dir>     产物输出目录（默认 ./dist）
#   --keep             保留中间目录（排障用）
#
# 产物：<outdir>/bili-sync-up_<arch>-<version>.spk
#===============================================================================
set -euo pipefail

APP="bili-sync-up"
DISPLAY_NAME="Bili Sync Up"
REPO="${BILI_SYNC_REPO:-NeeYoonc/bili-sync-up}"
# 强依赖的 FFmpeg 套件名（套件源里为 ffmpeg / ffmpeg5 / ffmpeg6 / ffmpeg7 / ffmpeg8）
FFMPEG_DEP="${FFMPEG_DEP:-ffmpeg7}"

TAG=""
ARCH=""
BINARY=""
OUTDIR="./dist"
KEEP="no"

log() { printf '\033[36m▶ %s\033[0m\n' "$*"; }
die() { printf '\033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --tag)     TAG="${2:?--tag 需要值}"; shift 2 ;;
    --arch)    ARCH="${2:?--arch 需要值}"; shift 2 ;;
    --binary)  BINARY="${2:?--binary 需要值}"; shift 2 ;;
    --outdir)  OUTDIR="${2:?--outdir 需要值}"; shift 2 ;;
    --keep)    KEEP="yes"; shift ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *)         die "未知参数: $1" ;;
  esac
done

[ -n "$TAG" ]  || die "缺少 --tag（例：--tag v3.1.6）"
[ -n "$ARCH" ] || die "缺少 --arch（x86_64 或 armv8）"

# ── 架构映射：SPK arch / 上游产物 ──────────────────────────────────────────
case "$ARCH" in
  x86_64) ASSET="bili-sync-rs-Linux-x86_64-musl.tar.gz" ;;
  armv8)  ASSET="bili-sync-rs-Linux-aarch64-musl.tar.gz" ;;
  *) die "不支持的 arch: $ARCH（仅 x86_64 / armv8）" ;;
esac

# ── 版本号：v3.1.6 → 3.1.6；四段 → a.b.c-d ─────────────────────────────────
VERSION="$(printf '%s' "$TAG" | sed -E 's/^[vV]//; s/[-+].*$//')"
case "$VERSION" in
  [0-9]*.[0-9]*) : ;;
  *) die "无法从 tag 解析版本号: $TAG（得到 '$VERSION'）" ;;
esac
if printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$'; then
  VERSION="$(printf '%s' "$VERSION" | sed -E 's/^([0-9]+\.[0-9]+\.[0-9]+)\.([0-9]+)$/\1-\2/')"
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(mktemp -d)"
trap '[ "$KEEP" = "yes" ] || rm -rf "$WORK"' EXIT
mkdir -p "$OUTDIR"

log "构建 $APP $VERSION ($ARCH)  repo=$REPO tag=$TAG  依赖=$FFMPEG_DEP"

# ── 1. 取 bili-sync-rs 二进制 ───────────────────────────────────────────────
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

# ── 2. 启动包装（ffmpeg 由外部套件提供，这里只做探测与 PATH 兜底） ──────────
cat > "$WORK/app/bin/start.sh" <<'START'
#!/bin/sh
# 手动/服务启动统一入口
#   · 配置目录：$BILI_SYNC_CONFIG_DIR（默认 $HOME/.config/bili-sync）
#   · ffmpeg：由套件源里的 FFmpeg 套件提供，这里把它的 bin 加进 PATH 兜底
set -e
BIN_DIR="$(cd "$(dirname "$0")" && pwd)"
export PATH="$BIN_DIR:$PATH"
for _v in 8 7 6 5 ""; do
  _d="/var/packages/ffmpeg${_v}/target/bin"
  if [ -x "$_d/ffmpeg" ]; then
    export PATH="$_d:$PATH"
    export FFMPEG_PATH="$_d/ffmpeg"
    break
  fi
done
export HOME="${HOME:-/var/packages/bili-sync-up/var/home}"
export BILI_SYNC_CONFIG_DIR="${BILI_SYNC_CONFIG_DIR:-$HOME/.config/bili-sync}"
export TZ="${TZ:-Asia/Shanghai}"
export RUST_LOG="${RUST_LOG:-None,bili_sync=info}"
[ -x "$(command -v ffmpeg 2>/dev/null)" ] || echo "[warn] 未找到 ffmpeg，视频合并会失败（请先安装套件源里的 FFmpeg 套件）"
mkdir -p "$HOME" "$BILI_SYNC_CONFIG_DIR"
exec "$BIN_DIR/bili-sync-rs" "$@"
START
chmod +x "$WORK/app/bin/start.sh"

# ── 3. DSM 门户图标（套件中心「打开」按钮用；ui/config 由 postinst 按实际端口生成）
mkdir -p "$WORK/app/ui/images"
cp "$SCRIPT_DIR/template/PACKAGE_ICON_64.png"  "$WORK/app/ui/images/icon_64.png"
cp "$SCRIPT_DIR/template/PACKAGE_ICON_256.PNG" "$WORK/app/ui/images/icon_256.png"

# ── 4. package.tgz ──────────────────────────────────────────────────────────
# ★ 必须用 --mode 强制 755：Windows(MSYS) 上构建时 chmod +x 不会写进 tar，
#   产物里二进制会变 644 → 群晖上「无法启动」（2026-11-05 真机踩过）
tar -czf "$WORK/package.tgz" --mode='u+rwx,go+rx' -C "$WORK/app" .
log "package.tgz: $(du -h "$WORK/package.tgz" | cut -f1)"

# ── 5. SPK 骨架 ─────────────────────────────────────────────────────────────
SPK="$WORK/spk"
mkdir -p "$SPK/scripts" "$SPK/conf"
cp "$SCRIPT_DIR/template/INFO.in" "$SPK/INFO"
cp "$SCRIPT_DIR/template/scripts/"* "$SPK/scripts/"
cp "$SCRIPT_DIR/template/conf/privilege" "$SPK/conf/privilege"
cp "$SCRIPT_DIR/template/conf/resource"  "$SPK/conf/resource"
cp -r "$SCRIPT_DIR/template/WIZARD_UIFILES" "$SPK/WIZARD_UIFILES"
# 图标放【包根】而非 conf/ —— 与 DSM 规范一致
cp "$SCRIPT_DIR/template/PACKAGE_ICON.PNG"     "$SPK/PACKAGE_ICON.PNG"
cp "$SCRIPT_DIR/template/PACKAGE_ICON_256.PNG" "$SPK/PACKAGE_ICON_256.PNG"
cp "$WORK/package.tgz" "$SPK/package.tgz"
chmod 755 "$SPK/scripts/"*
chmod 644 "$SPK/INFO" "$SPK/conf/privilege" "$SPK/conf/resource" \
          "$SPK/WIZARD_UIFILES/install_uifile" \
          "$SPK/WIZARD_UIFILES/uninstall_uifile" \
          "$SPK/WIZARD_UIFILES/uninstall_uifile_chs"

# 占位符替换
sed -i \
  -e "s|@APP@|$APP|g" \
  -e "s|@DISPLAY_NAME@|$DISPLAY_NAME|g" \
  -e "s|@VERSION@|$VERSION|g" \
  -e "s|@ARCH@|$ARCH|g" \
  -e "s|@REPO@|$REPO|g" \
  -e "s|@FFMPEG_DEP@|$FFMPEG_DEP|g" \
  "$SPK/INFO"
for f in "$SPK/scripts/"*; do
  sed -i -e "s|@APP@|$APP|g" -e "s|@VERSION@|$VERSION|g" "$f"
done

# ── 6. 打 SPK ───────────────────────────────────────────────────────────────
# ★ 外层必须是【未压缩 tar】且【条目不带 ./ 前缀】（DSM 规范）；
#   写成 `tar -cf x.spk .` 会得到 ./INFO、./conf/... → DSM 判「套件文件格式不正确」
OUT="$OUTDIR/${APP}_${ARCH}-${VERSION}.spk"
( cd "$SPK" && tar --owner=0 --group=0 -cf "$WORK/out.spk" \
    INFO PACKAGE_ICON.PNG PACKAGE_ICON_256.PNG conf scripts WIZARD_UIFILES package.tgz )
mv "$WORK/out.spk" "$OUT"

# ── 7. 自检 ─────────────────────────────────────────────────────────────────
log "自检"
_refs="$(tar -tf "$OUT")"
for f in INFO PACKAGE_ICON.PNG PACKAGE_ICON_256.PNG package.tgz \
         scripts/start-stop-status scripts/postinst conf/privilege conf/resource \
         WIZARD_UIFILES/install_uifile WIZARD_UIFILES/uninstall_uifile; do
  printf '%s\n' "$_refs" | grep -qx "$f" || die "SPK 缺少 $f"
done
printf '%s\n' "$_refs" | grep -q '^\./' && die "SPK 条目带了 ./ 前缀（DSM 会判格式错误）"
_pkg="$(tar -xOf "$OUT" package.tgz | tar -tzf -)"
printf '%s\n' "$_pkg" | grep -qx "./bin/bili-sync-rs" || die "package.tgz 里没有 bin/bili-sync-rs"
printf '%s\n' "$_pkg" | grep -qx "./bin/ffmpeg" && die "不要再内置 ffmpeg（已改为强依赖 FFmpeg 套件）"
grep -q '^depends="' "$SPK/INFO" || die "INFO 缺少 depends（FFmpeg 强依赖）"

log "完成: $OUT（$(du -h "$OUT" | cut -f1)）"
echo "$OUT"
