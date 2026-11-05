# 群晖 DSM 套件（SPK）打包

把 `bili-sync-up` 的 Linux 静态二进制 + 静态 ffmpeg 打成群晖 DSM 7 套件 `.spk`。

- **不编译 Rust**：直接消费本仓 Release 里的 `bili-sync-rs-Linux-{x86_64,aarch64}-musl.tar.gz`。
- **内置 ffmpeg**：B 站基本是 DASH 分离流，没有 ffmpeg 下载出来没声音/合不了；包内自带 `ffmpeg`/`ffprobe`，启动时通过 `PATH` 优先命中。
- **自动构建**：`.github/workflows/build.yml` 里追加了 `spk` job —— 打 `v*` tag 发版时，Release 发布成功后自动产出两个架构的 SPK 并上传到同一个 Release。

## 产物

```
bili-sync-up_x86_64-<version>.spk     # 群晖 x86_64
bili-sync-up_armv8-<version>.spk      # 群晖 armv8（对应上游 aarch64 产物）
```

## 本地构建

```bash
# x86_64（自动从 GitHub Release 下载二进制 + 从 johnvansickle 下载静态 ffmpeg）
./synology/build-spk.sh --tag v3.1.5 --arch x86_64

# armv8
./synology/build-spk.sh --tag v3.1.5 --arch armv8

# 复用已下载的二进制 / ffmpeg（离线或反复打包时快很多）
./synology/build-spk.sh --tag v3.1.5 --arch x86_64 \
  --binary dist/bili-sync-rs-Linux-x86_64-musl.tar.gz \
  --ffmpeg-dir /tmp/ffmpeg-static

# 保留中间目录排障
./synology/build-spk.sh --tag v3.1.5 --arch x86_64 --keep
```

参数：`--tag`（必填）、`--arch`（`x86_64` / `armv8`）、`--binary`、`--ffmpeg-dir`、`--outdir`（默认 `./dist`）、`--keep`。

## 安装后的行为

| 项 | 值 |
|---|---|
| 套件名 / 包名 | `bili-sync-up`（DSM 显示名 `Bili Sync Up`） |
| 访问入口 | `http://<NAS-IP>:<安装向导里填的端口>`（默认 12345） |
| 配置 / 数据库 | `/var/packages/bili-sync-up/var/home/.config/bili-sync`（`HOME` 指向包数据目录） |
| 运行日志 | `/var/packages/bili-sync-up/var/bili-sync.log` |
| 下载目录 | 由 `conf/resource` 自动创建共享文件夹 `bili-sync-up`（`/volumeX/bili-sync-up`），也可在 Web 界面改成任意有权限的路径 |
| 运行用户 | 包用户 `bili-sync-up`（`conf/privilege` 的 `run-as: package`） |
| 启停 | `synopkg start|stop|restart bili-sync-up`；服务脚本 `scripts/start-stop-status` |

安装后首次打开会进入设置向导：配置 B 站 Cookie、下载路径、视频源。

## 安装 / 卸载向导

**安装**（`WIZARD_UIFILES/install_uifile`）：

| 项 | 作用 |
|---|---|
| **Web 访问端口** | 默认 `12345`；写入 `var/port`，并预置到应用配置的 `bind_address` |
| **访问 Token** | 预置到应用配置的 `auth_token`；留空 = 不启用鉴权 |
| **防火墙放行** | 勾选后用 `synofirewall --add-rule` 放行该端口 |

**端口/Token 是怎么写进去的**：应用把配置存在 SQLite（`var/config/data.sqlite`），
且启动时会把「legacy `config` 表」自动迁移成正式配置（`migrate_config_schema`）。
所以 `postinst` 直接用 `sqlite3` 造这张表并塞入 JSON：

```sql
CREATE TABLE config (data TEXT);
INSERT INTO config (data) VALUES ('{"bind_address":"0.0.0.0:22345","auth_token":"..."}');
```

首次启动日志会出现 `legacy_detected=true` 与
`配置迁移报告：已映射字段(2) = bind_address(...), auth_token(...)`，随即生效。

**卸载**（`WIZARD_UIFILES/uninstall_uifile`）：

| 选项 | 行为 |
|---|---|
| 仅卸载 | 保留 `var`（配置/数据库/日志），重装直接恢复 |
| 删除套件所有数据文件 | `preuninst`/`postuninst` 删除 `/volumeX/@appdata|@appconf|@appshare|@appstore|@apptemp/<pkg>` 并清理包用户（**下载共享文件夹保留**） |

> ⚠️ `/var/packages/<pkg>/var` 是**软链接**，`rm -rf` 软链只会删链接不删目标。
> 卸载脚本必须先用 `cd "$path" && pwd -P` 解析真实路径再删（`readlink -f` 在部分环境不可靠）。

## 套件中心「打开」按钮

`INFO` 带 `dsmuidir="ui"` + `dsmappname="SYNO.SDS.BiliSyncUp.Application"`，
`postinst` 按实际端口生成 `target/ui/config` 门户配置，图标在 `target/ui/images/`。
所以套件中心会出现「打开」，点击直达 `http://<NAS-IP>:<端口>/`。

## 已知限制

1. **端口只能在安装时/配置文件里改**：上游二进制没有 CLI 参数可改端口（只有 `--scan-only` / `--log-level`），端口来自配置项 `bind_address`。安装向导只在安装时出现一次，之后要改就改 `config.toml` 再重启服务。
2. **ffmpeg 为 GPL 静态构建**（johnvansickle），随包分发属 GPL 再分发：源码见 <https://johnvansickle.com/ffmpeg/>。如不希望引入 GPL，可把 `build-spk.sh` 里的 `FFMPEG_*_URL` 换成 LGPL 构建（如 BtbN 的 lgpl 变体），或在套件里改为依赖系统 ffmpeg。
3. **四段版本号**（如 `v3.1.4.1`）会转成 DSM 的 build 形式 `3.1.4-1`，因为 INFO 的 `version` 不接受四段点分。
4. **包体较大**：内置 ffmpeg 后 x86_64 约 73 MB、armv8 约 64 MB。

## 打包规范（踩过的坑）

外层 `.spk` 必须是**未压缩 tar**、且条目**不能带 `./` 前缀**，否则 DSM 安装时报
「套件文件格式不正确。请联系套件开发人员。」

```bash
# ✅ 正确
( cd "$SPK" && tar -cf out.spk INFO PACKAGE_ICON.PNG PACKAGE_ICON_256.PNG conf scripts package.tgz )
# ❌ 错误（条目变成 ./INFO、./conf/... → DSM 直接拒收）
( cd "$SPK" && tar -cf out.spk . )
```

图标 `PACKAGE_ICON.PNG`（72×72）/ `PACKAGE_ICON_256.PNG`（256×256）放**包根**，不是 `conf/`。

## 排障

```bash
# 服务状态
synopkg status bili-sync-up
/var/packages/bili-sync-up/scripts/start-stop-status status

# 日志
tail -f /var/packages/bili-sync-up/var/bili-sync.log

# 端口
netstat -ltn | grep 12345

# 手动跑一次（前台，看真实报错）
sudo -u bili-sync-up /var/packages/bili-sync-up/target/bin/start.sh
```
