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
      - /volume1/Cloudreve/OD/20/config:/app/.config/bili-sync
      - /volume1/Cloudreve/OD/20:/Downloads #下载目录 在前端直接/Downloads就是下载到/volume1/Cloudreve/OD/20

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

1. 打开 Container Manager (Docker)
2. 搜索 `qq1582185982/bili-sync`
3. 下载并创建容器
4. 设置端口映射和文件夹映射

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