# 安装指南

## Windows

1. 下载最新版 exe 文件
2. 双击运行 `bili-sync-rs.exe`
3. 打开浏览器访问 `http://localhost:12345`

## Docker（推荐）

### 一键部署
```bash
docker run -d \
  --name bili-sync \
  -p 12345:12345 \
  -v /path/to/data:/app/.config/bili-sync \
  -v /path/to/videos:/Downloads \
  qq1582185982/bili-sync
```

> 程序默认下载目录是容器内的 `/Downloads`（添加视频源时「保存路径」默认就是 `/Downloads`），所以视频卷要挂到 `/Downloads`；挂到别的容器内路径等于下载进容器，容器重建后文件会丢。

### docker-compose
```yaml
services:

  bili-sync:
    image: docker.cnb.cool/sviplk.com/docker/bili-sync:latest
    # build:
    #   context: .
    #   dockerfile: Dockerfile
    restart: unless-stopped
    network_mode: bridge
    # 该选项请仅在日志终端支持彩色输出时启用，否则日志中可能会出现乱码
    tty: false
    # 非必需设置项，推荐设置为宿主机用户的 uid 及 gid (`$uid:$gid`)
    # 可以执行 `id ${user}` 获取 `user` 用户的 uid 及 gid
    # 程序下载的所有文件权限将与此处的用户保持一致，不设置默认为 Root
    # user: 1000:1000
    hostname: bili-sync
    container_name: bili-sync
    # 程序默认绑定 0.0.0.0:12345 运行 http 服务
    ports:
      - 12345:12345
    volumes:
      - ./config:/app/.config/bili-sync
      - ./Downloads:/Downloads # 下载目录：程序默认下载到容器内 /Downloads，这里映射到宿主机 ./Downloads

    environment:
      - TZ=Asia/Shanghai
      - RUST_LOG=None,bili_sync=info
      # 可选：设置执行周期，默认为每天凌晨3点执行
      # - BILI_SYNC_SCHEDULE=0 3 * * *
    # 资源限制（可选）
    # deploy:
    #   resources:
    #     limits:
    #       cpus: '2'
    #       memory: 2G
    #     reservations:
    #       cpus: '0.5'
    #       memory: 500M
```

## 群晖 NAS

群晖有两种装法：**套件（推荐）** 和 **Container Manager（Docker）**。

### 方式一：套件源（推荐）

1. DSM → **套件中心** → **设置** → **套件来源** → **新增**
2. 名称随意（如 `dnof`），位置填 `https://ssp.dnof.net/`
3. 保存后回到「套件中心 → **社群**」，找到 **Bili Sync Up** → 安装
4. 也可以到 [Releases](https://github.com/NeeYoonc/bili-sync-up/releases/latest) 下载 `.spk`，在套件中心「手动安装」

安装向导可设置 **Web 访问端口**（默认 12345）、**访问 Token**、是否放行防火墙。装完点套件中心「打开」，或访问 `http://<NAS-IP>:<端口>`（填了 Token 会先要求输入）。

> 套件以专用用户 `bili-sync-up` 运行（DSM 7 套件规范）。下载目录建议用安装时自动创建的共享文件夹 `bili-sync-up`；若要指向其它共享文件夹，需在「控制面板 → 共享文件夹 → 权限」中给 `bili-sync-up` 用户读写权限。

### 方式二：Container Manager（Docker）

1. 打开 Container Manager (Docker)
2. 搜索 `qq1582185982/bili-sync`
3. 下载并创建容器
4. 设置端口映射和文件夹映射（视频目录挂到容器内 `/Downloads`）

## 升级方法

### Windows
下载新版 exe 替换旧文件即可

### Docker
```bash
docker pull qq1582185982/bili-sync
docker restart bili-sync
```

## 注意事项

- 首次运行会自动创建数据库 `data.sqlite` 并进入设置向导（跟着向导配置 B 站 Cookie、下载路径即可）
- 视频默认保存在 `/Downloads`（容器内），Docker 部署时请把宿主机目录挂到 `/Downloads`
- 建议使用 Docker 部署，更新更方便