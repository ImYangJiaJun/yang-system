# syntax=docker/dockerfile:1
# yang-system 后端生产镜像。
#
# 构建上下文必须是 lib_yang 仓库根（本文件位于 project/yang-system/docker/app/，
# 应用通过相对路径依赖 ../../crates 下的 yang-base / yang-db / yang-runtime）：
#
#   docker build -f project/yang-system/docker/app/Dockerfile -t yang-system:local .
#
# 工具链固定为 1.97.1，与 lib_yang/rust-toolchain.toml 和 CI 的 RUST_VERSION 一致；
# MSRV 1.80 兼容性由 .cargo/config.toml 的 resolver fallback 与 --locked 共同守护
#（lettre =0.11.19、async-compression 0.4.32 等约束见项目 AGENTS.md）。

FROM rust:1.97.1-slim-bookworm AS build
WORKDIR /ws

# 先只复制依赖清单与占位源码，预编译全部依赖以利用镜像层缓存；
# 业务源码变更不会使依赖层失效。
# 根 Cargo.toml 必须复制：crates/* 通过 `xxx.workspace = true` 继承
# workspace 元数据与共享依赖，找不到 workspace root 会直接解析失败。
COPY Cargo.toml ./
COPY crates/ crates/
COPY project/yang-system/Cargo.toml project/yang-system/Cargo.lock project/yang-system/
COPY project/yang-system/.cargo/ project/yang-system/.cargo/
RUN mkdir -p project/yang-system/src \
    && printf 'fn main() {}\n' > project/yang-system/src/main.rs \
    && printf '' > project/yang-system/src/lib.rs \
    && cargo build --release --locked --manifest-path project/yang-system/Cargo.toml --bin yang-system

# 复制真实源码，仅重编译 yang-system 本 crate。
# 注意：BuildKit 保留上下文文件的旧 mtime，cargo 会误判“未变更”而跳过
# 重编译（把占位空 binary 打进镜像）；必须先 touch 刷新 mtime。
COPY project/yang-system/src/ project/yang-system/src/
RUN find project/yang-system/src -type f -exec touch {} + \
    && cargo build --release --locked --manifest-path project/yang-system/Cargo.toml --bin yang-system

# 运行时：debian-slim + 非 root 用户。安装 curl 仅用于容器 HEALTHCHECK；
# 选择 debian-slim 而非 distroless 是因为二进制没有健康检查子命令，
# 且 distroless 无任何 HTTP 客户端可用。
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 yang \
    && useradd --system --uid 10001 --gid yang --no-create-home --shell /usr/sbin/nologin yang

WORKDIR /app
COPY --from=build /ws/project/yang-system/target/release/yang-system /usr/local/bin/yang-system
# 基线配置：占位密钥会被启动校验拒绝（失败关闭），生产凭据必须经
# YANG_SYSTEM_* 环境变量或 YANG_SYSTEM_SECRET_DIR secret 目录注入，
# 合成优先级见 project/yang-system/docs/CONFIGURATION.md。
COPY project/yang-system/config.example.toml /app/config.toml

# 容器内默认覆盖：生产环境、监听全部网卡（loopback 在容器内无法被编排器探测）。
# 管理面 9090 只应向采集端/探针暴露，不要对公网发布该端口。
ENV YANG_SYSTEM_APP_ENVIRONMENT=production \
    YANG_SYSTEM_HTTP_BIND=0.0.0.0:8080 \
    YANG_SYSTEM_OBSERVABILITY_METRICS_ENABLED=true \
    YANG_SYSTEM_OBSERVABILITY_METRICS_BIND=0.0.0.0:9090

USER yang
EXPOSE 8080 9090

# 容器级健康检查用业务面 /health/live（始终可用）；
# 编排器的 readiness 探针应指向管理面 /health/ready（:9090，带依赖检查与预算）。
HEALTHCHECK --interval=30s --timeout=3s --start-period=20s --retries=3 \
    CMD curl --fail --silent --max-time 2 http://127.0.0.1:8080/health/live || exit 1

# main.rs 固定读取相对路径 config.toml，因此 WORKDIR 必须是 /app。
ENTRYPOINT ["/usr/local/bin/yang-system"]
