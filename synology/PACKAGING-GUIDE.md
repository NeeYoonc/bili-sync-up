# 群晖 DSM 7 套件（SPK）打包指南

> 本文是 `bili-sync-up` 的 SPK 打包全过程说明，可直接交给其它 AI 作为「把某个上游项目的 Linux 二进制打成群晖套件」的参考范式。
> 所有结论都来自**真机（DSM 7）实测**，包含踩过的坑与验证命令。

---

## 0. 一句话概括

上游（Rust 项目）Release 里已经有 **Linux 静态 musl 二进制**，所以套件不需要编译 Rust：

```
上游 Release 资产 (bili-sync-rs-Linux-*-musl.tar.gz)
        + 静态 ffmpeg
        ↓  build-spk.sh 组装
SPK 骨架（INFO / scripts / conf / WIZARD_UIFILES / package.tgz）
        ↓  tar（未压缩、无 ./ 前缀）
bili-sync-up_<arch>-<version>.spk
        ↓  上传到同一个 GitHub Release
```

---

## 1. 仓库内目录结构

```
synology/
├── build-spk.sh                       # 唯一构建入口（本地 & CI 共用）
├── README.md                          # 打包说明（面向维护者）
├── PACKAGING-GUIDE.md                 # 本文
└── template/                          # SPK 骨架模板（不含二进制）
    ├── INFO.in                        # INFO 模板，@APP@/@VERSION@/@ARCH@ 占位
    ├── PACKAGE_ICON.PNG               # 72x72，放【SPK 根】
    ├── PACKAGE_ICON_256.PNG           # 256x256，放【SPK 根】
    ├── PACKAGE_ICON_64.png            # 门户图标，打进 package.tgz 的 ui/images/
    ├── conf/
    │   ├── privilege                  # {"defaults":{"run-as":"package"}}
    │   └── resource                   # data-share：自动建共享文件夹
    ├── scripts/
    │   ├── postinst                   # 安装后：预置配置 + 门户 + 权限 + 防火墙
    │   ├── preuninst                  # 卸载前：停服务 + 按向导删数据（软链坑见 §8）
    │   ├── postuninst                 # 卸载后：兜底删数据 + 删包用户
    │   ├── preupgrade                 # 升级前：停服务
    │   ├── postupgrade                # 升级后：chmod + 修权限
    │   └── start-stop-status          # 启停/状态（DSM 调用）
    └── WIZARD_UIFILES/
        ├── install_uifile             # 安装向导：端口 / Token / 防火墙
        ├── uninstall_uifile           # 卸载向导（英文）
        └── uninstall_uifile_chs       # 卸载向导（中文）
```

CI 侧：

```
.github/workflows/
├── build.yml          # 上游发版流程 + 追加的 spk job（tag v* 时自动出 SPK）
└── build-spk.yml      # 手动补包（workflow_dispatch，可为已有 Release 重打）
```

---

## 2. 打包前必须搞清楚的上游事实

以 `bili-sync-up` 为例（其它项目按同样方法调研）：

| 问题 | 结论 | 怎么查 |
|---|---|---|
| 有没有现成 Linux 二进制 | 有：`bili-sync-rs-Linux-{x86_64,aarch64,armv7l}-musl.tar.gz` | GitHub Release 资产列表 |
| 运行形态 | 常驻 HTTP 服务 + Web UI，默认 `0.0.0.0:12345` | README / Dockerfile |
| 端口能否改 | 不能通过 CLI/env；来自配置项 `bind_address` | 读 `config/clap.rs`（只有 `--scan-only`/`--log-level`）+ `config/mod.rs` 的 `default_bind_address()` |
| 配置存哪 | SQLite：`$CONFIG_DIR/data.sqlite` | `config/global.rs` 的 `CONFIG_DIR`（支持 `BILI_SYNC_CONFIG_DIR` 覆盖） |
| 配置表结构 | `config_items(key_name, value_json, updated_at)` | 装好后 `sqlite3 data.sqlite .schema config_items` |
| 配置能否预置 | 能，两种路径（见 §6.1） | 读 `config/manager.rs`：`legacy_config_exists()` / `load_legacy_config_value()` / `migrate_config_schema()` |
| 有没有鉴权 | 有：配置项 `auth_token`（默认 None），前端 `auth-login.svelte` | `config/mod.rs` 的 `default_auth_token()` |
| 运行时依赖 | **ffmpeg/ffprobe**（B 站 DASH 分离流合并必需）；可选 quickjs/node（yt-dlp） | Dockerfile 的 `apk add ffmpeg` |
| 进程行为 | **启动后会 fork**（父进程写 pid、子进程监听端口） | 真机 `ps` / `netstat` 对比 |
| 许可 | MIT（上游），内置 ffmpeg 为 GPL 静态构建 → 需在文档声明来源 | LICENSE / ffmpeg 构建来源 |

> 调研方法：优先读 `Dockerfile` / `docker-compose.yml`（运行参数、卷、端口、依赖都在里面），再读 `config/` 与 `main.rs`。

---

## 3. SPK 文件格式规范（最容易踩坑的部分）

### 3.1 外层 `.spk` = **未压缩 tar**，条目**不能带 `./` 前缀**

```bash
# ✅ 正确：显式列条目、cd 到组装目录
( cd "$SPK" && tar --owner=0 --group=0 -cf "$OUT" \
    INFO PACKAGE_ICON.PNG PACKAGE_ICON_256.PNG conf scripts WIZARD_UIFILES package.tgz )

# ❌ 错误：条目会变成 ./INFO、./conf/... → DSM 报「套件文件格式不正确。请联系套件开发人员。」
( cd "$SPK" && tar -cf "$OUT" . )
```

### 3.2 目录布局

```
xxx.spk (tar)
├── INFO                        # 元数据，key="value" 每行一个
├── PACKAGE_ICON.PNG            # 72x72  —— 放【根】，不是 conf/
├── PACKAGE_ICON_256.PNG        # 256x256
├── conf/
│   ├── privilege               # {"defaults":{"run-as":"package"}}
│   └── resource                # data-share 等平台资源
├── scripts/                    # DSM 生命周期脚本（755）
├── WIZARD_UIFILES/             # 安装/卸载向导 JSON
└── package.tgz                 # 应用载荷（gzip tar），解到 /var/packages/<pkg>/target
```

### 3.3 `package.tgz` 载荷

```
bin/bili-sync-rs        # 上游二进制（必须 755）
bin/ffmpeg              # 静态 ffmpeg
bin/ffprobe
bin/start.sh            # 启动包装：PATH/HOME/BILI_SYNC_CONFIG_DIR
ui/images/icon_64.png   # DSM 桌面门户图标
ui/images/icon_256.png
```

打包命令（**必须强制 755**，见 §8 坑 2）：

```bash
tar -czf "$WORK/package.tgz" --mode='u+rwx,go+rx' -C "$WORK/app" .
```

### 3.4 `INFO` 字段（本包实际使用）

```ini
package="bili-sync-up"                 # 包名（小写，与目录/用户同名）
version="3.1.6"                        # 只允许数字点分，或 a.b.c-build 形式
description="..."                      # 套件中心标题下描述（可较长）
arch="x86_64"                          # x86_64 | armv8（DSM 7 平台名）
maintainer="NeeYoonc"
maintainer_url="https://github.com/NeeYoonc/bili-sync-up"
distributor="NeeYoonc"
distributor_url="https://github.com/NeeYoonc/bili-sync-up"
os_min_ver="7.0-40851"
displayname="Bili Sync Up"
dsmuidir="ui"                          # 有门户才写
dsmappname="SYNO.SDS.BiliSyncUp.Application"   # 必须与 ui/config 的 key 逐字一致
checksum=""
changelog="..."                        # 套件中心「版本说明」，可写长文本
```

> 版本号规范化：`v3.1.5` → `3.1.5`；四段 `v3.1.4.1` → `3.1.4-1`（INFO 的 version 不接受四段点分）。

---

## 4. `build-spk.sh` 构建流程（逐段）

```bash
./synology/build-spk.sh --tag v3.1.6 --arch x86_64 [--outdir dist]
                        [--binary <本地 tar.gz>] [--ffmpeg-dir <已解压目录>] [--keep]
```

| 步骤 | 做什么 | 关键点 |
|---|---|---|
| 1 | 解析参数、架构映射 | `x86_64` → 资产 `bili-sync-rs-Linux-x86_64-musl.tar.gz` + ffmpeg amd64；`armv8` → `...aarch64...` + ffmpeg arm64 |
| 2 | 取二进制 | 优先 `--binary`（CI 用下载好的 artifact），否则从 Release 按 tag 下载 |
| 3 | 取 ffmpeg | 默认从 johnvansickle 下载静态构建（`ffmpeg-release-{amd64,arm64}-static.tar.xz`），只取 `ffmpeg`/`ffprobe` |
| 3b | 门户图标 | 把 64/256 图标拷进 `app/ui/images/` |
| 4 | 打 `package.tgz` | `tar -czf ... --mode='u+rwx,go+rx' -C app .` |
| 5 | 组装 SPK 骨架 | 拷 INFO（替换 `@APP@`/`@VERSION@`/`@ARCH@`/`@REPO@`）、scripts（替换占位符）、conf、WIZARD_UIFILES、图标 |
| 6 | 打外层 SPK | **未压缩 tar + 显式条目 + 无 `./`** |
| 7 | 自检 | 断言条目齐全、**无 `./` 前缀**、package.tgz 里有 `bin/bili-sync-rs` 与 `bin/ffmpeg` |

产物命名：`bili-sync-up_<arch>-<version>.spk`（`x86_64` / `armv8`）。

---

## 5. DSM 平台机制（写脚本必须知道）

### 5.1 包用户

- `conf/privilege` 写 `{"defaults":{"run-as":"package"}}`
- DSM 7 会**自动创建与包同名的专用用户** `bili-sync-up`，生命周期脚本/服务以它运行
- **DSM 7 不支持「用现有个人账号运行套件」**；只有 `package` 或 `root` 两种选择
- 后果：访问用户已有的共享文件夹需要在「控制面板 → 共享文件夹 → 权限」给 `bili-sync-up` 授权

### 5.2 路径映射（重要：都是软链接）

```
/var/packages/<pkg>/target → /volumeX/@appstore/<pkg>     # 应用载荷
/var/packages/<pkg>/var    → /volumeX/@appdata/<pkg>      # 数据（配置/DB/日志）
/var/packages/<pkg>/etc    → /volumeX/@appconf/<pkg>
/var/packages/<pkg>/home   → /volumeX/@apphome/<pkg>
```

### 5.3 生命周期脚本

| 脚本 | 时机 | 职责 |
|---|---|---|
| `postinst` | 安装后 | 建目录、预置配置、生成门户、放行防火墙 |
| `preuninst` | 卸载前 | 停服务、按 `wizard_delete_data` 删数据 |
| `postuninst` | 卸载后 | 兜底删数据、删包用户 |
| `preupgrade` / `postupgrade` | 升级前后 | 停服务 / 修权限与执行位 |
| `start-stop-status` | 启停/状态 | `start|stop|status|restart`，status 返回 0=运行 3=未运行 |

### 5.4 向导值如何传入

`WIZARD_UIFILES/*_uifile` 里每个 `subitems[].key` 会成为**环境变量**传给生命周期脚本：

```json
{ "key": "wizard_port", "desc": "Web 访问端口", "defaultValue": "12345" }
```

→ 脚本里读 `${wizard_port}`；卸载向导的 `wizard_delete_data` / `wizard_keep_data` 同理。

---

## 6. 本包的两个核心实现

### 6.1 安装时预置「端口 / Token」（应用配置在 SQLite 里）

应用的 `config.toml` **不会被读取**（那只用于旧版迁移）。真正生效的两条路：

```sh
# A) 全新安装（还没有数据库）：
#    造一个 legacy `config` 表，应用首启 migrate_config_schema 会把它迁移进正式配置
sqlite3 "$CONFIG_DIR/data.sqlite" \
  "CREATE TABLE config (data TEXT);
   INSERT INTO config (data) VALUES ('{\"bind_address\":\"0.0.0.0:33345\",\"auth_token\":\"888888888\"}');"

# B) 重装/升级（数据库已存在，且已迁移过）：
#    直接 upsert 正式表
sqlite3 "$CONFIG_DIR/data.sqlite" \
  "INSERT OR REPLACE INTO config_items (key_name,value_json,updated_at)
   VALUES ('bind_address','\"0.0.0.0:33345\"',datetime('now'));"
sqlite3 "$CONFIG_DIR/data.sqlite" \
  "INSERT OR REPLACE INTO config_items (key_name,value_json,updated_at)
   VALUES ('auth_token','\"888888888\"',datetime('now'));"
```

启动日志的验收标志：

```
检测到配置迁移待处理: 当前版本=0, 最新版本=1, legacy_detected=true
配置迁移报告：已映射字段(2) = bind_address(服务监听地址), auth_token(管理页访问密钥)
开始运行管理页: http://0.0.0.0:33345
```

> ⚠️ **升级/重装绝不能默认覆盖**（2026-11-05 真机 bug）：
> 向导的端口字段默认值必须留空，`postinst` 里按「向导填了才覆盖，否则保持库里现值」处理；
> 否则「更新套件」时向导不传值 → 默认 12345 会把用户设的 33345 冲掉（而 Token 因「空值不覆盖」反而保留，造成「端口变了、Token 没变」的诡异现象）。
>
> 决策顺序：`向导填了 → 用它` / `库里有 → 保持` / `都没有 → 12345`。

### 6.2 套件中心「打开」按钮

需要三样东西同时具备：

1. `INFO`：`dsmuidir="ui"` + `dsmappname="SYNO.SDS.<APP_ID>.Application"`
2. `target/ui/images/icon_64.png`、`icon_256.png`
3. `target/ui/config`（`postinst` 按实际端口生成）：

```json
{
  ".url": {
    "SYNO.SDS.BiliSyncUp.Application": {
      "type": "url",
      "icon": "images/icon_{0}.png",
      "title": "Bili Sync Up",
      "desc": "哔哩哔哩同步工具",
      "protocol": "http",
      "port": "33345",
      "allUsers": false
    }
  }
}
```

---

## 7. CI 集成

### 7.1 发版自动出 SPK（追加在 `build.yml` 末尾）

```yaml
  spk:
    name: Build SPK (${{ matrix.arch }})
    needs: release                              # 等 Release 建好
    if: startsWith(github.ref, 'refs/tags/v')   # 只有 v* tag
    runs-on: ubuntu-24.04
    permissions: { contents: write }
    strategy:
      fail-fast: false
      matrix:
        include:
          - arch: x86_64
            asset: bili-sync-rs-Linux-x86_64-musl.tar.gz
          - arch: armv8
            asset: bili-sync-rs-Linux-aarch64-musl.tar.gz
    steps:
      - uses: actions/checkout@v6
      - uses: actions/download-artifact@v8
        with: { name: "${{ matrix.asset }}", path: dist }
      - run: |
          chmod +x synology/build-spk.sh
          ./synology/build-spk.sh --tag "$GITHUB_REF_NAME" --arch "${{ matrix.arch }}" \
            --binary "dist/${{ matrix.asset }}" --outdir dist/spk
      - env: { GH_TOKEN: "${{ secrets.GITHUB_TOKEN }}" }
        run: gh release upload "$GITHUB_REF_NAME" dist/spk/*.spk --clobber
      - uses: actions/upload-artifact@v6
        with: { name: "spk-${{ matrix.arch }}", path: dist/spk/*.spk, if-no-files-found: error }
```

要点：直接用 `build` job 已产出的二进制 artifact，**不重复编译**；`--clobber` 保证重跑覆盖。

### 7.2 手动补包（`build-spk.yml`）

`workflow_dispatch` 入参：

| 入参 | 说明 |
|---|---|
| `tag` | 目标 Release tag；留空 = 最新 Release |
| `upload` | `true` 上传到 Release；`false` 只出 workflow artifact |

### 7.3 避免文档改动触发全量编译

```yaml
on:
  push:
    branches: [main, master]
    tags: ['v*']
    paths-ignore:            # GitHub 的 path filters 对 tag 推送不生效，发版不受影响
      - 'synology/**'
      - 'docs/**'
      - '**.md'
```

---

## 8. 踩坑清单（真机实测，按发生顺序）

| # | 现象 | 根因 | 修法 |
|---|---|---|---|
| 1 | 安装报 **「套件文件格式不正确。请联系套件开发人员。」** | 外层 tar 用了 `tar -cf x.spk .`，条目变成 `./INFO`、`./conf/...` | 显式列条目、`cd` 到组装目录，**不带 `./`** |
| 2 | 安装成功但**启动失败 / 无法启动** | `package.tgz` 里 `bili-sync-rs` 是 `644`（在 Windows/MSYS 上构建时 `chmod +x` 不会写进 tar） | 打包用 `tar --mode='u+rwx,go+rx'`；`postinst` 再 `chmod 755 target/bin/*` 兜底 |
| 3 | 向导里设的**端口/Token 不生效**（仍是 12345 / 空） | ① 应用只读 SQLite，不读 `config.toml`；② 卸载保留数据时旧库还在，`postinst` 的「仅首次预置」被跳过 | 见 §6.1 两条路径（legacy `config` 表 / `config_items` upsert） |
| 4 | DSM 显示**「已手动停止」**，点启动没反应 | 进程被启动后 fork（父 pid 写进 pidfile、子进程监听）；按 pid 停不掉，残留进程占端口 → 新实例 `Address in use` 退出 | `stop` 里按二进制路径 `pkill -f "$PKG_DIR/bin/bili-sync-rs"` 兜底；`start` 前若端口被本包残留占用先清理 |
| 5 | 卸载选**「删除所有数据」但 `/volumeX/@appdata/<pkg>` 仍在** | `/var/packages/<pkg>/var` 是**软链接**，`rm -rf` 只删链接不删目标 | 先解析真实路径再删：`real="$( cd "$p" && pwd -P )"`；再加 `/volume*/@appdata|@appconf|@appshare|@appstore|@apptemp/<pkg>` 兜底 |
| 6 | 套件中心**没有「打开」按钮** | 缺门户配置 | `INFO` 加 `dsmuidir`/`dsmappname` + `target/ui/config` + `ui/images/icon_{64,256}.png` |
| 7 | **卸载没有「保留/删除数据」选项** | 缺 `uninstall_uifile` | 加 `WIZARD_UIFILES/uninstall_uifile(_chs)`，脚本读 `wizard_delete_data` |
| 8 | `readlink -f` 返回空 | 部分环境（busybox/MSYS）不可靠 | 用 `cd + pwd -P` 解析物理路径 |
| 9 | 图标不是官方的 | 临时生成的占位图 | 用上游 `web/static/icon-512.png` 等比缩放生成 72/256/64 |

---

## 9. 验证清单

### 9.1 本地（不装 NAS 也能验）

```bash
SPK=dist/bili-sync-up_x86_64-3.1.6.spk

# 外层格式
tar -tf "$SPK" | grep -c '^\./'          # 必须为 0
tar -tf "$SPK"                            # INFO/PACKAGE_ICON*/conf/scripts/WIZARD_UIFILES/package.tgz

# INFO
tar -xOf "$SPK" INFO

# 载荷与执行位
tar -xOf "$SPK" package.tgz | tar -tzvf - | grep bili-sync-rs   # 必须是 -rwxr-xr-x
tar -xOf "$SPK" package.tgz | tar -tzf - | grep -E 'bin/|ui/'

# 脚本语法
tar -xOf "$SPK" scripts/start-stop-status | sh -n
```

### 9.2 NAS 上

```bash
synopkg status bili-sync-up
ls -l /var/packages/bili-sync-up/target/bin/
cat /var/packages/bili-sync-up/var/access-info.txt
tail -40 /var/packages/bili-sync-up/var/start.log
tail -40 /var/packages/bili-sync-up/var/bili-sync.log
netstat -ltnp | grep bili-sync-rs
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:<端口>/
```

---

## 10. 移植到其它项目（通用步骤）

1. **调研**：读 `Dockerfile`/`docker-compose` → 得到运行命令、端口、卷（配置/下载目录）、系统依赖
2. **确认产物**：Release 里有没有 Linux x86_64/arm64 静态二进制；没有就得在 CI 里自己交叉编译
3. **定配置落点**：能否用环境变量指定配置目录（如 `BILI_SYNC_CONFIG_DIR`）；否则用 `HOME`
4. **写 `build-spk.sh`**：下载二进制 + 依赖（ffmpeg 等）→ `package.tgz`（**--mode 强制 755**）→ SPK 骨架 → 外层 tar（**无 `./`**）→ 自检
5. **写生命周期脚本**：`start-stop-status`（端口就绪探测、fork 兜底 kill）、`postinst`（配置/门户/权限）、`preuninst`/`postuninst`（**软链解析后再删**）
6. **加向导**：`install_uifile`（端口/Token/目录）、`uninstall_uifile`（保留/删除数据）
7. **接 CI**：在发版 workflow 末尾追加 `spk` job（`needs: release`、`if: tags v*`）+ 一个手动补包 workflow
8. **真机验证**：按 §9 逐项过，重点盯「执行位 / 端口生效 / 卸载删除 / 打开按钮」

---

## 11. 参考

- 上游项目：<https://github.com/NeeYoonc/bili-sync-up>
- 套件源：<https://ssp.dnof.net/>
- 静态 ffmpeg：<https://johnvansickle.com/ffmpeg/>（GPL，随包分发需注明来源）
