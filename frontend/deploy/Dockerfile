# syntax=docker/dockerfile:1
# yang-system 前端（Quasar SPA）生产镜像。
#
# 构建上下文是 frontend/ 目录本身：
#
#   cd project/yang-system/frontend
#   docker build -f deploy/Dockerfile -t yang-system-frontend:local .
#
# pnpm 版本由 package.json 的 packageManager 字段（pnpm@10.33.1）经 corepack 固定；
# Node 大版本与 CI 一致（24）。nginx 镜像版本与摘要和 CI 语法校验步骤完全同源。

FROM node:24-bookworm-slim AS build
WORKDIR /app
RUN corepack enable
# 依赖层缓存：--ignore-scripts 跳过 postinstall（quasar prepare 需要完整工程
# 文件，无法在本层执行）；拷贝全部源码后再显式补跑 quasar prepare。
COPY package.json pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile --ignore-scripts
COPY . .
RUN pnpm exec quasar prepare && pnpm build

# 运行时直接使用 CI 校验 nginx.conf 的同一镜像（固定版本 + 摘要）。
FROM nginx:1.30.4-alpine3.24@sha256:97d490c12ba55b4946b01546d1c3ed324e8d41ab1c9fcb2a616aa470620e5b46 AS runtime
COPY deploy/nginx.conf /etc/nginx/nginx.conf
COPY --from=build /app/dist/spa /usr/share/nginx/html

# 注意：nginx.conf 有意只监听 127.0.0.1:8081，upstream 也指向 127.0.0.1:8080
#（部署契约见 deploy/README.md，CI 对该文件做只读 nginx -t，不要修改）。
# 因此本镜像必须与后端容器共享网络命名空间运行（Kubernetes 同 Pod sidecar、
# docker --network container:<backend> 或 Linux --network host），
# 公网入口由前置的受信任 TLS 边缘终止并覆盖 Forwarded/X-Forwarded-*。
EXPOSE 8081

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget --quiet --tries=1 --spider http://127.0.0.1:8081/ || exit 1
