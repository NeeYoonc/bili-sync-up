# .sspks —— 套件源索引器（vendored）

从 [`sspks-cf`](https://github.com/NeeYoonc/sspks-cf) 复制过来的单文件索引器，只做一件事：
把本仓 Release 里的 `.spk` 解析成 `sspks-index.json`（含内嵌图标与 md5），
作为 Release 资产发布，供 Cloudflare Worker 定时同步到 R2，DSM 套件中心即可看到新版本。

- 上游：`sspks-cf/tools/index.py`（GPL-3.0，衍生自 jdel/sspks / tbc0309/sspks-imnks）
- 更新方式：从 sspks-cf 重新复制 `tools/index.py` 与 `assets/default_icon_*.png`
- 不需要 `data/synology_models.yaml`（那是 Worker 过滤机型用的，索引器不读）

CI 用法见 `.github/workflows/publish-package-source.yml`。
