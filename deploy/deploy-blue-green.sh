#!/usr/bin/env bash
# =============================================================================
# yang-system — 蓝绿部署脚本（服务器侧）
# 适配：Ubuntu 22.04 LTS x86_64 + Docker（bash 5.1，curl，gunzip）
#
# 用法（在包含 config.cloud.toml / yang-system-images.tar.gz 的目录执行）：
#   ./deploy-blue-green.sh status                 # 查看容器/镜像/端口状态
#   ./deploy-blue-green.sh infra-up               # 起 mysql + redis（只初始化一次）
#   ./deploy-blue-green.sh smoke [tar_file]       # 载入镜像并起绿容器（不动线上）
#   ./deploy-blue-green.sh cutover                # 切流量：绿 → 线上（冒烟通过后执行）
#   ./deploy-blue-green.sh deploy [tar_file]      # 一条龙：载入 → 冒烟 → 切流量
#   ./deploy-blue-green.sh rollback               # 回退到上一版本（蓝容器）
#   ./deploy-blue-green.sh logs [container]       # 追踪容器实时日志（Ctrl+C 退出）
#
# 可用环境变量覆盖：APP_DIR / NET / CONFIG_FILE / BACKEND_TAR / 各容器名 /
#   LIVE_HOST_PORT / GREEN_HOST_PORT / METRICS_*_PORT / BIND_ADDR
#
# -----------------------------------------------------------------------------
# 拓扑（每个颜色是「一对」容器，前端与后端共享网络命名空间）
#
#   yang-backend[-green]   监听 0.0.0.0:8080（业务）与 0.0.0.0:9090（管理面）
#   yang-frontend[-green]  --network container:<同色后端>；nginx 监听 8081（所有网卡）
#
#   只有 8081（应用边缘）与 9090（管理面）由**后端**容器发布；前端不发布端口
#   （共享 netns 时，端口由 netns 的拥有者发布）。
#
#   发布规则（硬约束，由 frontend/scripts/verify-deployment-contract.mjs 校验）：
#     **必须写 -p 127.0.0.1:<host_port>:8081，绝不写 -p <host_port>:8081**
#   应用边缘从 2026-09-22 起监听所有网卡（见 frontend/deploy/nginx.conf 头部说明），
#   因此「不暴露公网」由这里的 loopback 绑定承担；公网 HTTPS 必须由宿主机上的
#   受信 TLS 边缘终止并覆盖 Forwarded/X-Forwarded-*。
# -----------------------------------------------------------------------------
set -euo pipefail

# ---------- 配置 ----------
APP_DIR="${APP_DIR:-/home/yjj/yang-system}"
NET="${NET:-yang-system-net}"
CONFIG_FILE="${CONFIG_FILE:-config.cloud.toml}"
BACKEND_TAR="${BACKEND_TAR:-yang-system-images.tar.gz}"
INFRA_COMPOSE="${INFRA_COMPOSE:-compose.infra.yaml}"

MYSQL_CONTAINER="${MYSQL_CONTAINER:-yang-mysql}"
REDIS_CONTAINER="${REDIS_CONTAINER:-yang-redis}"

BACKEND_IMAGE="${BACKEND_IMAGE:-yang-system-backend:live}"
FRONTEND_IMAGE="${FRONTEND_IMAGE:-yang-system-frontend:live}"
BACKEND_BACKUP="${BACKEND_BACKUP:-yang-system-backend:previous}"
FRONTEND_BACKUP="${FRONTEND_BACKUP:-yang-system-frontend:previous}"

LIVE_BACKEND="${LIVE_BACKEND:-yang-backend}"
LIVE_FRONTEND="${LIVE_FRONTEND:-yang-frontend}"
GREEN_BACKEND="${GREEN_BACKEND:-yang-backend-green}"
GREEN_FRONTEND="${GREEN_FRONTEND:-yang-frontend-green}"
BLUE_BACKEND="${BLUE_BACKEND:-yang-backend-blue}"
BLUE_FRONTEND="${BLUE_FRONTEND:-yang-frontend-blue}"

# 容器内端口
EDGE_PORT="${EDGE_PORT:-8081}"
METRICS_PORT="${METRICS_PORT:-9090}"

# 宿主端口
# ⚠️ 默认端口是 18654，**不是** 8154。原因：这台服务器的阿里云安全组只放行
#    80/443 与 **18000-19000** 段（2026-09-23 实测：同机 18501/18093/18110/18088
#    从公网可达，8154 超时；本机 ufw 只显式放行 18088，但 docker 发布的端口走
#    FORWARD 链绕开 ufw，所以 ufw 不是成因——别再往那个方向查）。
#    改成 18000-19000 之外的端口会让公网直接不可达，且**浏览器会经由系统代理给出
#    误导性的 503**，排查时务必用 `curl --noproxy '*'` 直连验证。
LIVE_HOST_PORT="${LIVE_HOST_PORT:-18654}"
GREEN_HOST_PORT="${GREEN_HOST_PORT:-8155}"
METRICS_LIVE_PORT="${METRICS_LIVE_PORT:-9154}"
METRICS_GREEN_PORT="${METRICS_GREEN_PORT:-9155}"

# 应用边缘（宿主端口见上面的 LIVE_HOST_PORT，当前 18654）的发布绑定地址。
# **默认 127.0.0.1**：只对宿主机可见，公网访问交给宿主机上的受信 TLS 边缘。
# 改成 0.0.0.0 会让该端口**明文 http 直接对公网开放**——凭据、会话 Cookie、密码重置
# 令牌都会以明文传输。仅限测试环境，且应尽快换回 TLS 边缘。
# ⚠️ 这个默认值被 frontend/scripts/verify-deployment-contract.mjs **机械校验**：
#    该文件必须原样保留 `BIND_ADDR="${BIND_ADDR:-127.0.0.1}"` 字面量，否则 CI 部署
#    合同门禁会失败（门禁允许运维在调用时用环境变量显式覆盖，但不允许改默认值）。
BIND_ADDR="${BIND_ADDR:-127.0.0.1}"

# 管理面（宿主 9154，容器内 9090）的绑定地址。**默认与 BIND_ADDR 分开**：管理面暴露
# `/metrics` 与带依赖检查的 `/health/ready`，只应给采集端/探针，所以默认不随应用边缘
# 一起对公网开放——把 BIND_ADDR 改成 0.0.0.0 时，9154 仍然留在 loopback 上。
# ⚠️ 但它**是可以被覆盖的**（`METRICS_BIND_ADDR=0.0.0.0` 完全合法）。真那么做等于把
#    /metrics 与 /health/ready 一并暴露出去，先确认那是你要的。
METRICS_BIND_ADDR="${METRICS_BIND_ADDR:-127.0.0.1}"

# ---------- 输出 ----------
C_RED=$'\033[31m'; C_GRN=$'\033[32m'; C_YEL=$'\033[33m'; C_BLU=$'\033[34m'; C_RST=$'\033[0m'
info() { echo -e "${C_BLU}[INFO]${C_RST} $*"; }
ok()   { echo -e "${C_GRN}[ OK ]${C_RST} $*"; }
warn() { echo -e "${C_YEL}[WARN]${C_RST} $*"; }
err()  { echo -e "${C_RED}[ERR ]${C_RST} $*" >&2; }
die()  { err "$*"; exit 1; }

# ---------- 工具 ----------
need()    { command -v "$1" >/dev/null 2>&1 || die "缺少命令 $1，请先安装"; }
# ⚠️ 这两个判据**必须区分「容器真的不在」与「docker 根本没答上来」**。
#    它们的返回值直接驱动「备份线上容器 / 跳过（首次部署？）」这类分支：
#    若把 docker 不可达静默当成「不存在」，cutover 会跳过备份，紧接着 start_pair 的
#    docker rm -f 会把线上那对容器**直接删掉**——回退源被销毁，而输出只说「首次部署」。
#    （2026-09-23 审计发现；同一条规矩此前只修了 ensure_database 里的查询，漏了这里。）
exists() {
  local names
  names=$(docker ps -a --format '{{.Names}}') \
    || die "docker 不可达：docker ps 失败。请确认 Docker 在运行、当前用户有权限（docker 组）。"
  printf '%s\n' "$names" | grep -qxF "$1"
}
running() {
  local names
  names=$(docker ps --format '{{.Names}}') \
    || die "docker 不可达：docker ps 失败。请确认 Docker 在运行、当前用户有权限（docker 组）。"
  printf '%s\n' "$names" | grep -qxF "$1"
}

cd "$APP_DIR" || die "找不到部署目录：$APP_DIR"

# 精确清理：只删「yang-system 开头」且未被任何容器（含已停止的蓝容器）引用的旧镜像，
# 不碰同机其它项目的镜像，也不做宿主机全局 prune。
prune_unused_images() {
  # ⚠️ 这里以前吞掉两条 docker 的失败：`docker images … || true` 在守护进程不可达时
  #    会让循环空转（**什么都没清理，而 cutover 随后照打「部署完成 ✔」**）；
  #    `docker ps -a … 2>/dev/null` 失败则一律走进「未使用」分支，rmi 失败后再编一个
  #    「可能仍被引用」的理由——真实原因是 docker 不可达。现在失败即中止，理由用原始报错。
  local images repo_tag err
  images=$(docker images --format '{{.Repository}}:{{.Tag}}') \
    || die "docker 不可达：docker images 失败，镜像清理未执行。"
  for repo_tag in $(printf '%s\n' "$images" | grep -E '^yang-system-(backend|frontend)' || true); do
    if docker ps -a --filter "ancestor=${repo_tag}" --format '{{.ID}}' | grep -q .; then
      info "使用中，保留：${repo_tag}"
    elif err=$(docker rmi "$repo_tag" 2>&1); then
      ok "已删除未使用镜像：${repo_tag}"
    else
      warn "删除失败：${err:-未知原因}"
    fi
  done
}

ensure_network() {
  docker network inspect "$NET" >/dev/null 2>&1 || {
    info "创建 docker 网络 $NET"
    docker network create "$NET" >/dev/null
  }
}

require_config() {
  [ -f "$APP_DIR/$CONFIG_FILE" ] || die "找不到配置文件 $APP_DIR/$CONFIG_FILE。请从部署产物里复制并填写密钥。"
  # 占位值检查。
  #
  # ⚠️ **不要指望应用自身的启动校验拦住占位密钥**：`is_placeholder_secret`
  # （src/config/mod.rs:1266）只认 `changeme` / `replace-me` / `example-secret`（精确相等）、
  # `replace-with*` / `replace_with*`（前缀）、以及含 `placeholder` 的值。
  # 像 `CHANGE_ME_TOKEN_ACTIVE_SECRET_AT_LEAST_32_BYTES` 这种（47 字节）**会静默通过**。
  # 所以模板统一用 `replace-with-` 前缀让应用自己 fail-closed，这里再兜一道。
  #
  # 必须跳过注释行：配置里有大量含这些词的注释示例，不排除会误报。
  local hits
  hits=$(grep -nE '=[[:space:]]*"[^"]*(CHANGE_ME|replace-with)' "$APP_DIR/$CONFIG_FILE" \
         | grep -vE '^[0-9]+:[[:space:]]*#' || true)
  if [ -n "$hits" ]; then
    err "配置文件里仍有未填写的占位值："
    printf '%s\n' "$hits" | sed 's/^/    /' >&2
    die "请先填写这些项再部署：$APP_DIR/$CONFIG_FILE"
  fi
  # 防「空文件 / 半截模板」：上面的占位检查对空文件零命中，会直接放行。
  grep -qE '^[[:space:]]*\[authorization\]' "$APP_DIR/$CONFIG_FILE" \
    || die "config.cloud.toml 看起来不完整（缺 [authorization] 段）：$APP_DIR/$CONFIG_FILE"
}

# ---------- 从 config.cloud.toml 取 MySQL 连接要素 ----------
# 目的：**MySQL 凭据只有一个来源**（config.cloud.toml 的 [mysql].url），
# 不需要额外维护 .env 文件。
#
# 刻意手写解析而不引入 TOML 依赖：服务器上不保证有 python/tomlq，
# 而这里只需要「段头 + 单行 key = "value"」这一种形态。
toml_get() {
  awk -v want_section="$1" -v want_key="$2" '
    /^[[:space:]]*\[/ {
      s = $0
      sub(/^[[:space:]]*\[/, "", s); sub(/\].*$/, "", s)
      section = s; next
    }
    section == want_section {
      line = $0
      sub(/[[:space:]]*#.*$/, "", line)
      if (line ~ "^[[:space:]]*" want_key "[[:space:]]*=") {
        sub(/^[^=]*=[[:space:]]*/, "", line)
        gsub(/^"|"[[:space:]]*$/, "", line)
        print line; exit
      }
    }
  ' "$APP_DIR/$CONFIG_FILE"
}

# 解析 mysql://USER:PASSWORD@HOST:PORT/DB 并导出给 compose 用。
# **因此密码只能用字母数字**（含 @ : / 会破坏解析）。生成合规密码：openssl rand -hex 24
load_mysql_env() {
  local url userpass hostportdb hostport
  url=$(toml_get mysql url)
  [ -n "$url" ] || die "config.cloud.toml 里读不到 [mysql].url"

  case "$url" in
    mysql://*) ;;
    *) die "[mysql].url 必须以 mysql:// 开头，实际读到：$url" ;;
  esac

  userpass=${url#mysql://}
  userpass=${userpass%%@*}
  hostportdb=${url#*@}

  MYSQL_USER=${userpass%%:*}
  MYSQL_PASSWORD=${userpass#*:}
  MYSQL_DATABASE=${hostportdb#*/}
  MYSQL_DATABASE=${MYSQL_DATABASE%%\?*}
  hostport=${hostportdb%%/*}
  MYSQL_HOST=${hostport%%:*}
  MYSQL_PORT=${hostport##*:}
  if [ "$MYSQL_PORT" = "$MYSQL_HOST" ]; then MYSQL_PORT=3306; fi

  for name in MYSQL_USER MYSQL_PASSWORD MYSQL_DATABASE; do
    [ -n "${!name}" ] || die "[mysql].url 里解析不出 $name。格式应为 mysql://USER:PASSWORD@HOST:PORT/DB，且密码只用字母数字"
  done
  export MYSQL_USER MYSQL_PASSWORD MYSQL_DATABASE

  if [ "$MYSQL_HOST" != "$MYSQL_CONTAINER" ]; then
    warn "[mysql].url 的主机名是 ${MYSQL_HOST}，而本脚本管理的 MySQL 容器叫 ${MYSQL_CONTAINER}。"
    warn "用宿主机/外部 MySQL 时请忽略；否则应用会连不上库。"
  fi
}

# root 密码**不需要你维护**：首次使用时随机生成并写到 chmod 600 的文件。
# 它只用于「确保库存在」这类运维动作；应用自己用 [mysql].url 里的账号。
load_root_password() {
  ROOT_PW_FILE="$APP_DIR/.mysql-root-password"
  if [ -f "$ROOT_PW_FILE" ]; then
    MYSQL_ROOT_PASSWORD=$(cat "$ROOT_PW_FILE")
  else
    need openssl
    MYSQL_ROOT_PASSWORD=$(openssl rand -hex 24)
    ( umask 077; printf '%s' "$MYSQL_ROOT_PASSWORD" > "$ROOT_PW_FILE" )
    chmod 600 "$ROOT_PW_FILE" 2>/dev/null || true
    info "已生成 MySQL root 密码并保存到 $ROOT_PW_FILE（请纳入备份；不要提交到仓库）"
  fi
  export MYSQL_ROOT_PASSWORD
}

# 确保 [mysql].url 里那个库存在。
# **应用自己不会建库**：全仓没有 CREATE DATABASE，schema_sync 只渲染 CREATE TABLE，
# 库不存在时连接会直接失败，所以这里补上。
ensure_database() {
  if ! running "$MYSQL_CONTAINER"; then
    warn "$MYSQL_CONTAINER 未运行，跳过建库检查（用外部 MySQL 时由 DBA 负责）"
    return 0
  fi
  [ -n "${MYSQL_ROOT_PASSWORD:-}" ] || load_root_password

  # 必须区分「查询成功但结果为空（库确实不存在）」与「查询本身失败（认证/连接问题）」。
  # 这里曾经写成 `... 2>/dev/null || true`，把 Access denied 一并吞掉，于是认证失败
  # 被误报成「数据库不存在，正在创建」——2026-09-23 实际踩过，排查方向被完全带偏。
  local exists rc=0
  exists=$(docker exec -i -e MYSQL_PWD="$MYSQL_ROOT_PASSWORD" "$MYSQL_CONTAINER" \
             mysql -uroot -N -B \
             -e "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME='$MYSQL_DATABASE'" 2>&1) || rc=$?
  if [ "$rc" -ne 0 ]; then
    err "查询数据库列表失败（退出码 $rc），MySQL 的原始输出："
    printf '%s\n' "$exists" | sed 's/^/      /' >&2
    die "无法连上 $MYSQL_CONTAINER，或 root 凭据不对。请确认 $ROOT_PW_FILE 里的密码与容器实际使用的密码一致（容器首次初始化后改这个文件是无效的）；MySQL 是外部实例时请由 DBA 建库。"
  fi
  if [ "$exists" = "$MYSQL_DATABASE" ]; then
    ok "数据库 $MYSQL_DATABASE 已存在"
    return 0
  fi

  info "数据库 $MYSQL_DATABASE 不存在，正在创建…"
  docker exec -i -e MYSQL_PWD="$MYSQL_ROOT_PASSWORD" "$MYSQL_CONTAINER" \
    mysql -uroot -e "CREATE DATABASE IF NOT EXISTS \`$MYSQL_DATABASE\` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci" \
    || die "建库失败。请确认 $ROOT_PW_FILE 里的 root 密码正确，或 MySQL 是外部实例（那就由 DBA 建库）"
  # 顺带授权给应用账号：用外部 MySQL 时这个账号可能还没有该库的权限。
  docker exec -i -e MYSQL_PWD="$MYSQL_ROOT_PASSWORD" "$MYSQL_CONTAINER" \
    mysql -uroot -e "GRANT ALL PRIVILEGES ON \`$MYSQL_DATABASE\`.* TO '$MYSQL_USER'@'%'" >/dev/null 2>&1 || true
  ok "数据库 $MYSQL_DATABASE 已创建"
}

# ---------- 容器起停 ----------
# 起一对容器：后端是 netns 拥有者（发布端口），前端加入它的命名空间。
# 顺序不能反：--network container:<backend> 要求 backend 先存在。
start_pair() {
  local backend="$1" frontend="$2" host_port="$3" metrics_port="$4"
  docker rm -f "$frontend" "$backend" >/dev/null 2>&1 || true

  docker run -d --name "$backend" \
    --network "$NET" \
    -p "${BIND_ADDR}:${host_port}:8081" \
    -p "${METRICS_BIND_ADDR}:${metrics_port}:9090" \
    --add-host host.docker.internal:host-gateway \
    -v "$APP_DIR/$CONFIG_FILE:/app/config.toml:ro" \
    --restart unless-stopped \
    "$BACKEND_IMAGE" >/dev/null
  ok "后端已启动：$backend（边缘 ${BIND_ADDR}:${host_port} → 8081，管理面 ${METRICS_BIND_ADDR}:${metrics_port} → 9090）"

  docker run -d --name "$frontend" \
    --network "container:${backend}" \
    --restart unless-stopped \
    "$FRONTEND_IMAGE" >/dev/null
  ok "前端已启动：$frontend（共享 ${backend} 的网络命名空间，不单独发布端口）"
}

stop_pair() { docker stop "$2" "$1" >/dev/null 2>&1 || true; }
remove_pair() { docker rm -f "$2" "$1" >/dev/null 2>&1 || true; }

# 健康检查：应用边缘（nginx 首页）+ 后端管理面 readiness。
# 首页在未登录时会 307 跳登录，属正常，不能只认 200。
# ⚠️ 探针 curl 的两条硬规矩：
#  (1) **必须 --noproxy '*'**。本文件自己就写过「代理会让 curl 给出误导结果」，而 curl 对
#      **127.0.0.1 同样会走 http_proxy**（实测 curl 8.21）。部署 shell 里只要 export 了
#      http_proxy，所有健康检查都会失败——服务完全正常，脚本却报「健康检查未通过」，
#      并把理由指向 MySQL/Redis 或端口。
#  (2) **必须 --max-time**。否则一次挂起的连接就吃掉整个预算，打印的「最多 60 秒」并不成立。
probe_code() {
  curl -s -o /dev/null -w '%{http_code}' --noproxy '*' --max-time 5 "$1" 2>/dev/null || echo 000
}

# 探针该打哪个主机：绑 0.0.0.0（或空）时用 127.0.0.1（一定可达）；绑具体地址时就用那个地址。
# 写死 127.0.0.1 会在 `-EdgeBindAddr <宿主IP>` 时**假报「边缘无响应」**——容器其实好好的。
probe_host() {
  case "${1:-}" in
    0.0.0.0|"") echo 127.0.0.1 ;;
    *)          echo "$1" ;;
  esac
}

wait_http() {
  local url="$1" tries="${2:-40}" code i
  for ((i=1; i<=tries; i++)); do
    code=$(probe_code "$url")
    [[ "$code" =~ ^(2|3)[0-9][0-9]$ ]] && return 0
    sleep 2
  done
  return 1
}

# 管理面 /health/ready 带依赖检查（MySQL/Redis）。就绪返回 200；未就绪返回非 2xx。
wait_ready() {
  local port="$1" tries="${2:-40}" code i
  for ((i=1; i<=tries; i++)); do
    code=$(probe_code "http://$(probe_host "$METRICS_BIND_ADDR"):${port}/health/ready")
    [[ "$code" == "200" ]] && return 0
    sleep 2
  done
  return 1
}

health_check() {
  local host_port="$1" metrics_port="$2" label="$3"
  if wait_ready "$metrics_port" 30; then
    ok "$label 后端 readiness 通过（依赖就绪）"
  else
    warn "$label 后端 readiness 未通过（检查 MySQL/Redis 与 config.cloud.toml）"
    return 1
  fi
  if wait_http "http://$(probe_host "$BIND_ADDR"):${host_port}/" 15; then
    ok "$label 应用边缘 HTTP 响应正常"
  else
    warn "$label 应用边缘无响应"
    return 1
  fi
}

# ---------- 子命令 ----------
cmd_status() {
  need docker
  echo "容器："
  # ⚠️ 这里以前把三条 docker 的失败全部吞掉（`2>/dev/null || true` / `|| echo 不存在`）。
  #    docker 守护进程不可达时，整份报告会显示成「容器为空、镜像为空、网络不存在」——
  #    一个完全错误的方向，而真实原因是 docker 根本连不上（2026-09-23 审计发现）。
  #    故障时 status 往往是最先跑的命令，所以它最不该骗人。
  docker ps -a --filter "name=yang-" --format '  {{.Names}}\t{{.Image}}\t{{.Status}}\t{{.Ports}}'
  echo "镜像："
  docker images --format '  {{.Repository}}:{{.Tag}}\t{{.ID}}\t{{.Size}}' | grep -E 'yang-system-(backend|frontend)' || echo "  (无 yang-system-* 镜像)"
  echo "网络："
  if docker network inspect "$NET" >/dev/null 2>&1; then
    docker network inspect "$NET" -f '  {{.Name}} ({{len .Containers}} 个容器)'
  else
    echo "  $NET 不存在"
  fi
}

cmd_logs() {
  need docker
  local container="${1:-$LIVE_BACKEND}"
  exists "$container" || die "容器不存在：$container（可用：$LIVE_BACKEND / $LIVE_FRONTEND / $GREEN_BACKEND / $GREEN_FRONTEND / yang-mysql / yang-redis）"
  info "跟踪容器日志（Ctrl+C 退出）：$container"
  docker logs -f --tail 200 "$container" || true
}

cmd_infra_up() {
  need docker
  [ -f "$INFRA_COMPOSE" ] || die "找不到 $INFRA_COMPOSE"
  require_config
  load_mysql_env       # 凭据从 config.cloud.toml 来，不需要 .env
  load_root_password
  # compose 里 yang-system-net 声明为 external，所以必须先建网络。
  ensure_network

  info "启动基础设施（MySQL + Redis）"
  # 不传 --env-file：MYSQL_* 已由上面的解析导出到当前 shell，compose 直接继承。
  docker compose -f "$INFRA_COMPOSE" up -d
  ok "基础设施已启动"
  docker compose -f "$INFRA_COMPOSE" ps
  echo "   提示：MySQL 首次初始化需要几十秒；等 healthcheck 变 healthy 再继续。"

  # ⚠️ 判据必须是**能认证成功的真实查询**，不能用 `mysqladmin ping`：
  #    按 MySQL 文档，ping 在 Access denied 时**依然返回 0**（"服务器活着但拒绝连接"
  #    也算活着）；而首次初始化时它打到的还是 entrypoint 的**临时服务器**
  #    （--skip-networking，仅 socket 可达）。两者叠加会让循环在初始化完成前就 break，
  #    紧接着 ensure_database 撞上「Access denied for user 'root'@'localhost'」
  #    ——2026-09-23 实际踩过。用 SELECT 1 则同时验证「服务已起」与「密码已生效」。
  info "等待 MySQL 就绪（以认证查询为准，最多 90s）…"
  local i ready=0
  for ((i=1; i<=30; i++)); do
    if docker exec -i -e MYSQL_PWD="$MYSQL_ROOT_PASSWORD" "$MYSQL_CONTAINER" \
         mysql -uroot -N -B -e "SELECT 1" >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 3
  done
  if [ "$ready" -ne 1 ]; then
    echo "  当前容器状态：" >&2
    docker compose -f "$INFRA_COMPOSE" ps >&2 || true
    die "MySQL 在 90s 内仍未就绪。请查看：docker logs $MYSQL_CONTAINER --tail 50"
  fi
  ensure_database
}

prepare() {
  need docker; need curl; need gunzip
  ensure_network
  require_config
  load_mysql_env
  # 部署前先确保库存在——应用只建表不建库，库缺失时容器会起来就退出。
  ensure_database
  # 优先用镜像包；包不在时退回复用本机已有镜像。
  # **上传成功后两侧都会删包**（deploy.ps1 删本机的，cmd_cutover 删服务器上的），
  # 所以「镜像没变、只想改运行时参数（宿主端口 / BIND_ADDR / 环境变量）」的重新部署
  # 不该被一个已被删掉的文件挡住——那会逼着回本机重新构建 9 分钟（2026-09-23 实际踩过）。
  if [ -f "$BACKEND_TAR" ]; then
    info "载入镜像：$BACKEND_TAR"
    case "$BACKEND_TAR" in
      *.gz|*.tgz) gunzip -c "$BACKEND_TAR" | docker load ;;
      *)          docker load < "$BACKEND_TAR" ;;
    esac
  elif docker image inspect "$BACKEND_IMAGE" >/dev/null 2>&1 \
    && docker image inspect "$FRONTEND_IMAGE" >/dev/null 2>&1; then
    warn "找不到镜像包 $BACKEND_TAR，改用本机已有的 $BACKEND_IMAGE / $FRONTEND_IMAGE（不重新载入）"
    warn "  它们就是上一次部署留下的那一份；若源码已变，请回本机跑 deploy.ps1 重新构建并上传。"
  else
    die "既找不到镜像包 $BACKEND_TAR，本机也没有可用的 $BACKEND_IMAGE / $FRONTEND_IMAGE。请先在本机跑 deploy.ps1 上传。"
  fi

  # 备份当前线上镜像，供 rollback 用。
  local old_backend old_frontend
  old_backend=$(docker inspect -f '{{.Image}}' "$LIVE_BACKEND" 2>/dev/null || true)
  old_frontend=$(docker inspect -f '{{.Image}}' "$LIVE_FRONTEND" 2>/dev/null || true)
  # ⚠️ `docker tag … || true` 会吞掉失败却仍宣布「已备份」——备份不成立时回退会**悄悄
  #    落到上上版**，而任何一条输出都不会揭示差异（2026-09-23 审计发现）。
  if [ -n "$old_backend" ]; then
    if docker tag "$old_backend" "$BACKEND_BACKUP" >/dev/null 2>&1; then
      ok "旧后端镜像已备份为 $BACKEND_BACKUP（$old_backend）"
    else
      warn "备份旧后端镜像**失败**（docker tag $old_backend → $BACKEND_BACKUP）——回退可能落到更早的版本！"
    fi
  else
    warn "未找到线上后端容器 $LIVE_BACKEND 的镜像，跳过镜像备份（首次部署？）"
  fi
  if [ -n "$old_frontend" ]; then
    if docker tag "$old_frontend" "$FRONTEND_BACKUP" >/dev/null 2>&1; then
      ok "旧前端镜像已备份为 $FRONTEND_BACKUP（$old_frontend）"
    else
      warn "备份旧前端镜像**失败**（docker tag $old_frontend → $FRONTEND_BACKUP）——回退可能落到更早的版本！"
    fi
  fi
}

cmd_smoke() {
  prepare
  # 首次部署时线上还不存在——这不是错误，直接起绿容器即可。
  if running "$LIVE_BACKEND"; then
    info "线上容器在运行，起绿容器做冒烟（线上不受影响）"
  else
    warn "线上容器未运行（首次部署？）——绿容器起来后将直接用于 cutover"
  fi
  start_pair "$GREEN_BACKEND" "$GREEN_FRONTEND" "$GREEN_HOST_PORT" "$METRICS_GREEN_PORT"
  info "等待绿容器就绪（最多 60 秒）…"
  if health_check "$GREEN_HOST_PORT" "$METRICS_GREEN_PORT" "绿容器"; then
    echo "   查看日志：docker logs -f $GREEN_BACKEND"
    echo "   外部验证（在宿主机上）：curl -I --noproxy '*' http://$(probe_host "$BIND_ADDR"):${GREEN_HOST_PORT}/"
    echo "   确认无误后切流量：$0 cutover"
  else
    warn "绿容器健康检查未通过！线上容器未受影响。"
    echo "   排查：docker logs $GREEN_BACKEND"
    echo "         docker logs $GREEN_FRONTEND"
    echo "   清理：docker rm -f $GREEN_FRONTEND $GREEN_BACKEND"
    exit 1
  fi
}

cmd_cutover() {
  need docker
  running "$GREEN_BACKEND" || die "绿容器 $GREEN_BACKEND 未运行（先执行 $0 smoke）"
  info "切流量前再次健康检查…"
  wait_ready "$METRICS_GREEN_PORT" 10 || die "绿容器 readiness 未通过，中止切流量（线上不受影响）"
  wait_http "http://$(probe_host "$BIND_ADDR"):${GREEN_HOST_PORT}/" 10 || die "绿容器边缘未响应，中止切流量（线上不受影响）"

  info "备份线上容器为蓝（停止但不删除，供回退）…"
  remove_pair "$BLUE_BACKEND" "$BLUE_FRONTEND"
  if exists "$LIVE_BACKEND"; then
    stop_pair "$LIVE_BACKEND" "$LIVE_FRONTEND"
    docker rename "$LIVE_FRONTEND" "$BLUE_FRONTEND"
    docker rename "$LIVE_BACKEND"  "$BLUE_BACKEND"
    ok "旧容器已备份为 $BLUE_BACKEND + $BLUE_FRONTEND（已停止）"
  else
    warn "没有可备份的线上容器（首次部署）"
  fi

  info "在线上端口 ${LIVE_HOST_PORT} 启动新版本…"
  start_pair "$LIVE_BACKEND" "$LIVE_FRONTEND" "$LIVE_HOST_PORT" "$METRICS_LIVE_PORT"

  info "清理绿容器…"
  remove_pair "$GREEN_BACKEND" "$GREEN_FRONTEND"
  # ⚠️ `docker rm -f | true` 后无条件说「已清理」是假的：rm 被拒（daemon 异常、容器卡在
  #    removing）时绿容器其实还在占着 ${GREEN_HOST_PORT}/${METRICS_GREEN_PORT}。
  if exists "$GREEN_BACKEND" || exists "$GREEN_FRONTEND"; then
    warn "绿容器未完全清理（可能仍占着 ${GREEN_HOST_PORT}/${METRICS_GREEN_PORT}）：docker ps -a --filter name=yang-"
  else
    ok "已清理临时绿容器"
  fi

  # ⚠️ 只在文件确实存在时才宣称「已清理」。`rm -f` 对不存在的文件也返回 0，而
  #    `-Mode cutover` 这条路径根本不经过 prepare（服务器上的包通常已被上一次删掉），
  #    所以旧版那句「已清理镜像包 xxx」在**合法路径上必然为假**（2026-09-23 审计发现）。
  if [ -e "$BACKEND_TAR" ]; then
    rm -f "$BACKEND_TAR"
    ok "已清理镜像包 $BACKEND_TAR"
  else
    info "镜像包 $BACKEND_TAR 本就不存在，无需清理"
  fi

  prune_unused_images

  if health_check "$LIVE_HOST_PORT" "$METRICS_LIVE_PORT" "线上"; then
    ok "部署完成 ✔"
    # ⚠️ 这里以前**写死** `127.0.0.1`，与容器实际绑定无关：改绑 0.0.0.0 后上一行说
    #    「边缘 0.0.0.0:18654」而这里说「对外入口 http://127.0.0.1:18654/」，自相矛盾，
    #    人只会记住最后这句（2026-09-23 因此误判过一次）。
    #    现在**只陈述本脚本能观测到的事实**，不替安全组下结论：
    #      · 实际绑定       —— docker inspect，可观测
    #      · loopback ⇒ 公网不可达 —— 由绑定可证
    #      · 0.0.0.0  ⇒ **能否真从公网访问取决于云安全组与宿主防火墙，脚本探测不到**
    #        （本文件的默认端口注释就记着反例：绑了所有网卡但安全组不放行的端口照样不可达）
    local edge_bind edge_host
    if ! edge_bind=$(docker inspect -f \
        '{{with index .HostConfig.PortBindings "8081/tcp"}}{{(index . 0).HostIp}}:{{(index . 0).HostPort}}{{end}}' \
        "$LIVE_BACKEND" 2>/dev/null) || [ -z "$edge_bind" ]; then
      warn "读不到 $LIVE_BACKEND 的端口绑定（docker inspect 失败，或该端口未发布）——"
      warn "  下面的暴露面结论不可信，请手工核对：docker ps --filter name=$LIVE_BACKEND"
      edge_bind="（未知）"
    fi
    edge_host="${edge_bind%%:*}"
    echo "   应用边缘绑定：${edge_bind}（容器内 8081）"
    echo "   宿主机自测：curl --noproxy '*' -I http://127.0.0.1:${LIVE_HOST_PORT}/"
    case "$edge_host" in
      127.0.0.1)
        warn "公网不可达：应用边缘只绑在 loopback（BIND_ADDR 的安全默认值）。"
        warn "  若本意是公网直接访问，请重新发布：BIND_ADDR=0.0.0.0 LIVE_HOST_PORT=$LIVE_HOST_PORT $0 deploy"
        warn "  （deploy.ps1 用 -EdgeBindAddr 0.0.0.0）" ;;
      0.0.0.0)
        info "应用边缘已绑到所有网卡。**能否从公网访问取决于云安全组与宿主防火墙，本脚本探测不到**"
        info "  ——请从外网直连验证：curl.exe --noproxy \"*\" -I http://<公网IP>:${LIVE_HOST_PORT}/"
        info "  且这是明文 http：凭据 / 会话 Cookie / 密码重置令牌都不加密；生产请改经受信 TLS 边缘。" ;;
      *)
        info "应用边缘绑在 ${edge_host}（既非 loopback 也非全网卡）。是否公网可达脚本探测不到，请从外网验证。" ;;
    esac
    echo "   观察：docker logs -f $LIVE_BACKEND"
    echo "   回退：$0 rollback"
  else
    warn "部署后健康检查未通过，建议立即回退：$0 rollback"
    exit 1
  fi
}

cmd_rollback() {
  need docker
  exists "$BLUE_BACKEND" || die "没有备份容器 $BLUE_BACKEND，无法回退"
  info "回退到上一版本…"
  remove_pair "$LIVE_BACKEND" "$LIVE_FRONTEND"

  # 先删掉 :live 标签，再把备份镜像重新打回 :live —— 让回退后的线上镜像与标签一致。
  if docker image inspect "$BACKEND_BACKUP" >/dev/null 2>&1; then
    docker tag "$BACKEND_BACKUP" "$BACKEND_IMAGE" >/dev/null 2>&1 || true
  fi
  if docker image inspect "$FRONTEND_BACKUP" >/dev/null 2>&1; then
    docker tag "$FRONTEND_BACKUP" "$FRONTEND_IMAGE" >/dev/null 2>&1 || true
  fi

  docker rename "$BLUE_FRONTEND" "$LIVE_FRONTEND"
  docker rename "$BLUE_BACKEND"  "$LIVE_BACKEND"
  # 顺序要紧：后端要先起（它是 netns 拥有者），前端才能加入它的命名空间。
  docker start "$LIVE_BACKEND" >/dev/null
  docker start "$LIVE_FRONTEND" >/dev/null
  ok "已回退并启动 $LIVE_BACKEND + $LIVE_FRONTEND"

  # ⚠️ 回退复用的是**旧容器**（docker rename + start），而**端口绑定在容器创建时就固定了**，
  #    `docker start` 不会改它。所以若该容器当初是用不同的 BIND_ADDR / LIVE_HOST_PORT
  #    创建的，回退会让**公网暴露面静默变化**——典型场景：从 0.0.0.0:18654 退回
  #    127.0.0.1:8154，外网直接打不开，而脚本不会有任何提示。2026-09-23 已知该风险。
  local expected="${BIND_ADDR}:${LIVE_HOST_PORT}"
  local actual
  actual=$(docker inspect -f \
    '{{with index .HostConfig.PortBindings "8081/tcp"}}{{(index . 0).HostIp}}:{{(index . 0).HostPort}}{{end}}' \
    "$LIVE_BACKEND" 2>/dev/null || true)
  if [ "$actual" != "$expected" ]; then
    warn "回退后的端口绑定是「${actual:-未知}」，与当前期望的「$expected」不一致！"
    warn "  端口绑定在容器创建时就固定，docker start 不会改。外网可达性可能与预期不同。"
    warn "  要让绑定生效必须重新发布：BIND_ADDR=$BIND_ADDR LIVE_HOST_PORT=$LIVE_HOST_PORT $0 deploy"
  else
    ok "端口绑定与期望一致（$actual）"
  fi

  info "等待就绪…"
  if health_check "$LIVE_HOST_PORT" "$METRICS_LIVE_PORT" "回退后"; then
    ok "回退成功"
  else
    warn "回退后健康检查未通过，查看：docker logs $LIVE_BACKEND"
    # ⚠️ 必须非 0 退出：以前只 warn，脚本以 0 结束，`deploy.ps1 -Mode rollback` 会因为
    #    ssh 退出码为 0 而报告「完成」——**回退失败被当成回退成功**（2026-09-23 审计发现）。
    exit 1
  fi
}

cmd_deploy() {
  # ⚠️ 不要写成 `cmd_smoke || exit 1`。把函数放进 `||` / `if` 这类**条件上下文**里，
  #    bash 会让 `set -e` 在**该函数体内整体失效**——于是 cmd_smoke 及其整条调用链
  #    （prepare / load 镜像 / start_pair …）里任何命令失败都被静默忽略，只剩末尾的
  #    health_check 兜底。2026-09-23 实际踩过：前端容器因
  #    "cannot join network namespace of container: ... is restarting" 起失败，
  #    脚本却照常打印 `[ OK ] 前端已启动`。
  #    裸调用即可——cmd_smoke 的 health_check 失败时会自己 `exit 1`。
  cmd_smoke
  cmd_cutover
}

# ---------- 入口 ----------
main() {
  [ -n "${2:-}" ] && BACKEND_TAR="$2"
  case "${1:-}" in
    status)   cmd_status ;;
    logs)     cmd_logs "${2:-}" ;;
    infra-up) cmd_infra_up ;;
    smoke)    cmd_smoke ;;
    cutover)  cmd_cutover ;;
    deploy)   cmd_deploy ;;
    rollback) cmd_rollback ;;
    *) cat <<USAGE
用法：$0 {status|logs|infra-up|smoke|cutover|deploy|rollback} [参数]

  status                 查看容器 / 镜像 / 网络状态
  logs   [container]     追踪容器实时日志（Ctrl+C 退出，默认线上后端）
  infra-up               启动 MySQL + Redis（只初始化一次）
  smoke  [tar_file]      载入镜像并起绿容器冒烟（不动线上）
  cutover                冒烟通过后切流量（绿 → 线上）
  deploy [tar_file]      一条龙：载入 → 冒烟 → 切流量
  rollback               回退到上一版本（蓝容器）

环境变量：
  APP_DIR         部署目录，默认 /home/yjj/yang-system
  BIND_ADDR       应用边缘的发布绑定地址，默认 127.0.0.1（只对宿主机可见，公网交给受信
                  TLS 边缘）。设为 0.0.0.0 即**明文 http 直连公网**——仅限测试环境。
                  管理面 9154 由 METRICS_BIND_ADDR 单独控制，**不随边缘一起暴露**。
                  ⚠️ 这个默认值被 frontend/scripts/verify-deployment-contract.mjs 锁定，
                  不要改它；要覆盖请在调用时传（deploy.ps1 用 -EdgeBindAddr）。
  LIVE_HOST_PORT  线上宿主端口，默认 18654。
                  ⚠️ 必须是阿里云安全组放行的端口（当前只放行 80/443 与 18000-19000 段），
                  否则公网不可达——且浏览器会经系统代理给出误导性的 503。
                  排查时务必 curl --noproxy '*' 直连验证。
USAGE
       exit 1 ;;
  esac
}

main "$@"
