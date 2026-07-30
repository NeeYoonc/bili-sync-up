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
  -p 3001:3000 \
  -v /path/to/data:/app/.config/bili-sync \
  -v /path/to/youtube-browser:/config \
  -v /path/to/videos:/app/videos \
  qq1582185982/bili-sync
```

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
    # 内置登录浏览器需要容器 init 以 Root 启动，不要设置 Docker 的 user 字段。
    # 需要指定下载文件所有者时在 environment 中设置 PUID/PGID。
    hostname: bili-sync
    container_name: bili-sync
    # 12345 为主程序；外部 3001 映射到容器内 HTTP 登录页面 3000。
    ports:
      - 12345:12345
      - 3001:3000
    shm_size: "1gb"
    volumes:
      - /volume1/Cloudreve/OD/20/config:/app/.config/bili-sync
      - /volume1/Cloudreve/OD/20/youtube-login-browser:/config
      - /volume1/Cloudreve/OD/20:/Downloads #下载目录 在前端直接/Downloads就是下载到/volume1/Cloudreve/OD/20 

    environment:
      - TZ=Asia/Shanghai
      - RUST_LOG=None,bili_sync=info
      # 可选：同时设置后启用登录桌面 HTTP Basic Auth
      # - CUSTOM_USER=bili-sync
      # - PASSWORD=change-me
      # - PUID=1000
      # - PGID=1000
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

1. 打开 Container Manager (Docker)
2. 搜索 `qq1582185982/bili-sync`
3. 下载并创建容器
4. 设置 `12345`、`3001` 端口和 `/app/.config/bili-sync`、`/config` 文件夹映射
5. 登录页面默认直接打开；需要额外认证时再同时设置 `CUSTOM_USER` 和 `PASSWORD`

## 升级方法

### Windows
下载新版 exe 替换旧文件即可

### Docker
```bash
docker pull qq1582185982/bili-sync
docker restart bili-sync
```

## 注意事项

- 首次运行会自动创建配置文件
- 视频默认保存在 `videos` 目录
- 建议使用 Docker 部署，更新更方便
