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
#   ./deploy-blue-green.sh refresh                # 刷新配置 / 重置数据库（见下）
#
# refresh 由两个环境变量选动作，至少要给一个（通常由本机 deploy.ps1 的
# -SyncConfig / -ResetDb 开关驱动，也可以在这里直接调用）：
#   SYNC_CONFIG=1                  把 $APP_DIR/config.cloud.toml.upload（先 scp 上来）
#                                  就地安装为 config.cloud.toml，并重启应用容器
#   RESET_DB=1                     清空并重建 [mysql].url 指向的整个库，并清空 Redis
#   RESET_DB_CONFIRM=yes           RESET_DB=1 时的**必填**确认，缺了就拒绝执行
#   例：SYNC_CONFIG=1 RESET_DB=1 RESET_DB_CONFIRM=yes ./deploy-blue-green.sh refresh
#
# 可用环境变量覆盖：APP_DIR / NET / CONFIG_FILE / BACKEND_TAR / 各容器名 /
#   LIVE_HOST_PORT / GREEN_HOST_PORT / METRICS_*_PORT / BIND_ADDR /
#   SYNC_CONFIG / RESET_DB / RESET_DB_CONFIRM / STAGED_CONFIG
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

# refresh 子命令的动作开关（用法见文件头）。默认两个都不开：什么都不会发生。
SYNC_CONFIG="${SYNC_CONFIG:-0}"
RESET_DB="${RESET_DB:-0}"
# 清库是不可逆的，所以它要求一个**显式**的确认值。deploy.ps1 只在用户交互确认之后才会
# 置这个变量；在这里直接手工调用时，你得自己写出来（缺了就是拒绝执行，不是默认放行）。
RESET_DB_CONFIRM="${RESET_DB_CONFIRM:-}"
# 暂存的新配置文件名（相对 APP_DIR）。deploy.ps1 用 scp 把它传上来，refresh 校验后就地装。
STAGED_CONFIG="${STAGED_CONFIG:-${CONFIG_FILE}.upload}"

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

# 配置校验。$1 可选：要校验的配置文件路径（默认 $APP_DIR/$CONFIG_FILE）。
# refresh 会拿它先校验**暂存**的新配置——顺序很要紧：先把要装的东西验干净，再去动
# 服务器上那份好的；反过来做就等于「覆盖完才发现新配置是半截模板」。
require_config() {
  local config_path="${1:-$APP_DIR/$CONFIG_FILE}"
  [ -f "$config_path" ] || die "找不到配置文件 $config_path。请从部署产物里复制并填写密钥。"
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
  hits=$(grep -nE '=[[:space:]]*"[^"]*(CHANGE_ME|replace-with)' "$config_path" \
         | grep -vE '^[0-9]+:[[:space:]]*#' || true)
  if [ -n "$hits" ]; then
    err "配置文件里仍有未填写的占位值："
    printf '%s\n' "$hits" | sed 's/^/    /' >&2
    die "请先填写这些项再部署：$config_path"
  fi
  # 防「空文件 / 半截模板」：上面的占位检查对空文件零命中，会直接放行。
  grep -qE '^[[:space:]]*\[authorization\]' "$config_path" \
    || die "配置看起来不完整（缺 [authorization] 段）：$config_path"
}

# ---------- 从 config.cloud.toml 取 MySQL 连接要素 ----------
# 目的：**MySQL 凭据只有一个来源**（config.cloud.toml 的 [mysql].url），
# 不需要额外维护 .env 文件。
#
# 刻意手写解析而不引入 TOML 依赖：服务器上不保证有 python/tomlq，
# 而这里只需要「段头 + 单行 key = "value"」这一种形态。
toml_get() {
  # $1=段 $2=键 [$3=配置文件路径，默认 $APP_DIR/$CONFIG_FILE]
  # $3 刻意允许传空串：`${3:-默认}` 对「显式空串」也回落到默认，调用方无需自己判断。
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
  ' "${3:-$APP_DIR/$CONFIG_FILE}"
}

# 解析 mysql://USER:PASSWORD@HOST:PORT/DB 并导出给 compose 用。
# **因此密码只能用字母数字**（含 @ : / 会破坏解析）。生成合规密码：openssl rand -hex 24
load_mysql_env() {
  # $1 可选：从哪个文件解析（默认 $APP_DIR/$CONFIG_FILE）。
  # refresh 会在**安装新配置之前**先用它试解析一遍暂存文件：URL 写坏了要当场中止，
  # 而不是等把服务器上的好配置覆盖掉之后才发现。
  local url userpass hostportdb hostport
  url=$(toml_get mysql url "${1:-}")
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
  # ⚠️ 不能写成 `curl ... || echo 000`：curl 退出码非 0 时**可能已经打印了状态码**
  #    （写目标出错、或 --max-time 在收到状态行之后才超时都会这样——2026-09-24 彩排实测：
  #    Git Bash 把 -o /dev/null 的路径转换坏掉后 curl 打了 "200" 且 rc=23，于是这里返回
  #    "200000"，`[[ == 200 ]]` 永远不成立 → 健康检查假失败 → 误触发配置回滚）。
  #    只取前 3 位，取不到数字才算 000。
  local out
  out=$(curl -s -o /dev/null -w '%{http_code}' --noproxy '*' --max-time 5 "$1" 2>/dev/null) || true
  case "$out" in
    [1-5][0-9][0-9]*) printf '%s' "${out:0:3}" ;;
    *)                 printf '000' ;;
  esac
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
# $3 可选：管理面的绑定地址（默认 $METRICS_BIND_ADDR）。refresh 会传容器的**实际**绑定进来。
wait_ready() {
  local port="$1" tries="${2:-40}" bind="${3:-$METRICS_BIND_ADDR}" code i
  for ((i=1; i<=tries; i++)); do
    code=$(probe_code "http://$(probe_host "$bind"):${port}/health/ready")
    [[ "$code" == "200" ]] && return 0
    sleep 2
  done
  return 1
}

# $4 / $5 可选：应用边缘与管理面的绑定地址（默认取 $BIND_ADDR / $METRICS_BIND_ADDR）。
health_check() {
  local host_port="$1" metrics_port="$2" label="$3"
  local bind="${4:-$BIND_ADDR}" metrics_bind="${5:-$METRICS_BIND_ADDR}"
  if wait_ready "$metrics_port" 30 "$metrics_bind"; then
    ok "$label 后端 readiness 通过（依赖就绪）"
  else
    warn "$label 后端 readiness 未通过（检查 MySQL/Redis 与 config.cloud.toml）"
    return 1
  fi
  if wait_http "http://$(probe_host "$bind"):${host_port}/" 15; then
    ok "$label 应用边缘 HTTP 响应正常"
  else
    warn "$label 应用边缘无响应"
    return 1
  fi
}

# 容器某个端口在宿主机上的**实际**绑定，形如 127.0.0.1:18654；读不到就返回空串。
# 与 cmd_cutover 收尾用的是同一个模板（它读的是同一份事实）。
container_binding() {
  local fmt
  fmt="{{with index .HostConfig.PortBindings \"$2/tcp\"}}{{(index . 0).HostIp}}:{{(index . 0).HostPort}}{{end}}"
  docker inspect -f "$fmt" "$1" 2>/dev/null || true
}

# ---------- refresh 用的辅助函数 ----------
# 以 root 执行一条**只读**标量查询并打印结果。
# 判据沿用 ensure_database：必须区分「查到空」与「查询失败」，所以**不能**写 `2>/dev/null`。
# ⚠️ 但 stdout 与 stderr 也**绝不能合并**（`2>&1`）：客户端的一行告警（例如
#    "mysql: [Warning] ..."）会让返回值变成「告警文本 + 数字」，于是调用方的
#    `[ "$n" -gt 0 ]` 报 integer expression expected → 判定成「一张表都没有」→
#    一份**完全正确**的新配置被回滚掉还报失败。stderr 只用于报错/转述，不参与返回值。
mysql_scalar() {
  local out errf rc=0
  errf=$(mktemp) || { err "无法创建临时文件（mktemp 失败）"; return 1; }
  out=$(docker exec -i -e MYSQL_PWD="$MYSQL_ROOT_PASSWORD" "$MYSQL_CONTAINER" \
          mysql -uroot -N -B -e "$1" 2>"$errf") || rc=$?
  if [ "$rc" -ne 0 ]; then
    err "MySQL 查询失败（退出码 $rc）：$1"
    sed 's/^/      /' "$errf" >&2
    rm -f "$errf"
    return 1
  fi
  # 成功也可能带 stderr（告警）：转给人看，但不进返回值。
  if [ -s "$errf" ]; then sed 's/^/      [mysql] /' "$errf" >&2; fi
  rm -f "$errf"
  printf '%s' "$out"
}

# 只读查询的整型判据：把「不可解析」与「真为 0」区分开。
# 上面已经隔离了 stderr，但这里再兜一道——代价极低，而误判的代价是回滚一份好配置。
require_integer() {
  local name="$1" value="$2"
  case "$value" in
    ''|*[!0-9]*)
      err "$name 的查询结果不可解析：[$value]"
      return 1 ;;
  esac
  return 0
}

# 执行一条**写入/DDL** 语句。任何失败都必须立刻中止并把 MySQL 的原始报错打出来——
# 这是清库路径，绝不允许出现 `|| true`（那等于把「没清掉」当成「清掉了」）。
mysql_exec() {
  local out rc=0
  out=$(docker exec -i -e MYSQL_PWD="$MYSQL_ROOT_PASSWORD" "$MYSQL_CONTAINER" \
          mysql -uroot -e "$1" 2>&1) || rc=$?
  if [ "$rc" -ne 0 ]; then
    err "MySQL 语句失败（退出码 $rc）：$1"
    printf '%s\n' "$out" | sed 's/^/      /' >&2
    return 1
  fi
}

# 清库目标的护栏。与 ensure_database 的宽松（外部 MySQL 只 warn）刻意不同：
# DROP DATABASE 打错地方是不可逆的，所以这里**一律拒绝**非本容器的目标，没有逃生门。
#
# 这里以前有个 RESET_DB_ALLOW_NONLOCAL=yes 的开关，已删除：本脚本的 mysql_scalar/mysql_exec
# 一律走 `docker exec $MYSQL_CONTAINER`，**URL 里的 host/port 根本不参与连接**。所以那个开关
# 既清不到真正的远端库，又会把**本机容器里同名的库**DROP 掉，最后照样打印「已删除库 …」
# 与「refresh 完成 ✔」（2026-09-24 审查发现：这是「作用在错误的路径上」＋「假报成功」叠加）。
# 与其给一个打错目标的逃生门，不如直接把话说清楚：本脚本只能重置自己管的那个容器。
assert_reset_target() {
  if [ "$MYSQL_HOST" != "$MYSQL_CONTAINER" ]; then
    die "[mysql].url 的主机名是 ${MYSQL_HOST}，不是本脚本管理的容器 ${MYSQL_CONTAINER}。
      本脚本只能重置**自己那个 MySQL 容器**里的库（mysql_scalar/mysql_exec 都走
      \`docker exec $MYSQL_CONTAINER\`，不按 URL 里的 host 建连接）——对外部/共享 MySQL
      执行这里等于打错目标，所以直接拒绝。
      若这确实是本机的容器：请把 [mysql].url 的主机名改成 ${MYSQL_CONTAINER} 再重跑。
      若它真是外部实例：清库请由 DBA 手工处理。"
  fi

  [ -n "$MYSQL_DATABASE" ] || die "[mysql].url 里解析不出库名，拒绝执行清库。"
  case "$MYSQL_DATABASE" in
    *[!A-Za-z0-9_]*)
      die "库名 [$MYSQL_DATABASE] 含非字母数字下划线字符，拒绝把它当作 DROP DATABASE 的目标。" ;;
    mysql|information_schema|performance_schema|sys)
      die "库名 [$MYSQL_DATABASE] 是 MySQL 系统库，拒绝清空。" ;;
  esac
  # 下面的 CREATE USER / ALTER USER 会把**用户名与密码**直接拼进 SQL，所以两者都要求只含
  # 字母数字（密码还额外允许下划线；用户名允许下划线是因为 MySQL 用户名常见这种写法）。
  # load_mysql_env 的 URL 解析本来就只支持字母数字密码——这里把口径写死，不让引号/反引号
  # 有机会破坏 SQL 拼接。
  case "$MYSQL_PASSWORD" in
    *[!A-Za-z0-9_]*) die "密码含非字母数字下划线字符，拒绝把它拼进 CREATE/ALTER USER 语句。" ;;
  esac
  case "$MYSQL_USER" in
    ''|*[!A-Za-z0-9_]*) die "用户名 [$MYSQL_USER] 含非字母数字下划线字符，拒绝把它拼进 SQL 语句。" ;;
  esac
}

# 停下来、稍后**照原样**起回来的容器对：线上那对，外加「停下来之前正在跑的」绿对。
# 绿对只在冒烟中途调用 refresh 时才在跑；已经停着的绿容器（上次冒烟的失败残留）不去动它，
# 也不在收尾时把它拉起来——那是冒烟用的临时容器，起回来只会占住 8155/9155。
GREEN_WAS_RUNNING=0
# 容器「停下来了还没起回去」的标志。它是 EXIT trap（refresh_on_exit）唯一依据：
# 只要它为 1，脚本无论以什么路径退出（包括 reset_database 里的 die），都会尝试把容器起回去。
# 2026-09-24 彩排实测过没它会发生什么：配置改坏时 `docker start frontend` 撞上 restarting 的
# backend，`set -e` 直接把脚本掀掉 —— 回滚逻辑一次都没跑到，站点留在「坏配置 + 应用全停」。
PAIRS_STOPPED=0
# 「停之前线上前端在跑吗」。只用于一个窄场景：后端不存在/起不来时，别把前端停在那儿不管——
# 容器名写错时就会出现「停掉了真实的 $LIVE_FRONTEND，而收尾因后端不存在直接返回」，边缘被留在
# down 状态（2026-09-24 彩排实测）。
LIVE_FRONTEND_WAS_RUNNING=0
stop_all_pairs() {
  # 按**实际结果**报告：以前是无条件打「已停止 A + B」，哪怕 docker stop 被 || true 吞掉了失败、
  # 容器根本没停（同族的「假报成功」，2026-09-24 审查发现）。
  local b_state f_state
  if running "$LIVE_FRONTEND"; then LIVE_FRONTEND_WAS_RUNNING=1; fi
  stop_pair "$LIVE_BACKEND" "$LIVE_FRONTEND"
  PAIRS_STOPPED=1
  b_state=$(container_state "$LIVE_BACKEND")
  f_state=$(container_state "$LIVE_FRONTEND")
  info "已请求停止 $LIVE_BACKEND（现在 $b_state）+ $LIVE_FRONTEND（现在 $f_state）"
  if running "$LIVE_BACKEND" || running "$LIVE_FRONTEND"; then
    warn "有容器没停下来——清库期间它们仍可能持有连接（DROP 会成功，但它们会持续报错）。"
  fi
  # 只把「绿**后端**曾在跑」记为需要恢复：它是 netns 拥有者。
  # 用 `running 后端 || running 前端` 当判据会把一个**本来停着**的绿后端也在收尾时拉起来，
  # 与本文件「已经停着的绿容器不去动它」的约定相反（2026-09-24 复审发现）。
  if running "$GREEN_BACKEND"; then
    GREEN_WAS_RUNNING=1
    stop_pair "$GREEN_BACKEND" "$GREEN_FRONTEND"
    info "已请求停止 $GREEN_BACKEND（现在 $(container_state "$GREEN_BACKEND")）+ $GREEN_FRONTEND（现在 $(container_state "$GREEN_FRONTEND")）"
  elif running "$GREEN_FRONTEND"; then
    # 前端单独在跑（后端已经不在）本来就是个坏状态：停掉，但不把它当成「需要恢复」。
    info "绿前端 $GREEN_FRONTEND 单独在跑（绿后端不在），一并停掉且不恢复。"
    stop_pair "$GREEN_BACKEND" "$GREEN_FRONTEND"
  fi
}

# 容器的当前状态字符串（running / exited / restarting …）；读不到就打 unknown。
# ⚠️ 容器不存在时 docker inspect 会往 stdout 打**一个空行**并返回 1，命令替换只去掉尾部换行，
#    于是 `|| echo unknown` 会拼出 "\nunknown"，把一行日志折成两行（2026-09-24 复审发现）。
#    所以先取值、判空、再归一化，而不是直接 `||`。
container_state() {
  local s=""
  s=$(docker inspect -f '{{.State.Status}}' "$1" 2>/dev/null) || s=""
  case "$s" in
    ''|*[!a-z]*) printf 'unknown' ;;
    *)           printf '%s' "$s" ;;
  esac
}

# 等后端**真的进入 running**。配置有问题时它会 crash-loop（Dockerfile 没有健康门禁，
# 进程一退出容器就重启），此时起前端必然失败（── 见下面的注释），所以要等。
# 返回 0=已 running，1=超时仍没起来。**调用方不许因此 die**：起不来正是健康检查要报告的事，
# 而 refresh 必须活到「回滚配置」那一步。
wait_backend_running() {
  local c="$1" tries="${2:-20}" i
  for ((i=1; i<=tries; i++)); do
    [ "$(container_state "$c")" = "running" ] && return 0
    sleep 1
  done
  return 1
}

# ⚠️ 启动顺序是被实测逼出来的，不是风格问题：frontend 以
#    `--network container:<同色 backend>` 加入**后端**的网络命名空间。实测（Docker 29.7.2）：
#    一旦 backend 重启，它拿到的是一个全新的 netns，而 frontend 会滞留在那个已经消失的旧
#    netns 里（容器内 /proc/net/dev 只剩 lo，连不上 backend 的 127.0.0.1:8081）——必须把
#    frontend 也重启，它才会重新加入当前的 netns。
#    所以：**后端先起，前端后起**；只重启后端会让前端变成一具「网都没了」的僵尸。
#    另：这里用 docker start 而**不是** start_pair —— 端口绑定在容器创建时就固定了，
#    start 不会改它；重建则会按当前 BIND_ADDR/LIVE_HOST_PORT 重新发布，等于悄悄改变暴露面。
#
# ⚠️ 这里**绝不 die**（所以每条失败都自己处理并 warn）：起不来正是随后 health_check 要报告的
#    事，而调用方 refresh 必须活到「回滚配置」那一步。2026-09-24 彩排实测：配置改坏时后端
#    crash-loop，`docker start frontend` 报 "cannot join network namespace ... is restarting"，
#    在没做这段处理之前 `set -e` 直接把整个脚本掀掉——回滚逻辑一次都没跑到。
# 起**一对**容器。线上对与绿对共用这一份实现——绿对原先各写一份，于是线上侧修好的规则
# （后端被重启过 → 前端必须 restart 才能重挂 netns；起前端前要等后端真的 running）没有搬过去
# （2026-09-24 复审发现）。成功与否一律 warn，不 die（理由见 start_all_pairs 的注释）。
start_pair_tracked() {
  # $4 可选：前端在**停下来之前**是否在跑（默认 1）。只在后端缺位时用得上。
  local backend="$1" frontend="$2" label="$3" frontend_was_running="${4:-1}"
  if ! exists "$backend"; then
    warn "容器 $backend 不存在——$label 这一对的后端不在。"
    if [ "$frontend_was_running" = 1 ] && exists "$frontend"; then
      # 它原本在跑，是我们刚才停掉的：起回去，别把系统留在「被我们改坏」的状态。
      # （没有后端时边缘会 502，但那是它进来时就有的状态——我们只负责不留新伤。）
      warn "  但它进 refresh 时原本在跑，正在把它起回去（没有后端，边缘会 502）。"
      docker start "$frontend" >/dev/null 2>&1 || warn "  启动 $frontend 失败。"
    fi
    return 0
  fi
  local berr backend_restarted=0
  if running "$backend"; then
    :
  elif berr=$(docker start "$backend" 2>&1); then
    backend_restarted=1   # 本次真的把它起起来了 —— 说明它换了 netns（见上面的注释）
  else
    warn "启动 $backend 失败：${berr}"
  fi
  # 后端是 netns 拥有者：它没进入 running 就起前端，只会得到 "cannot join network namespace"。
  if ! wait_backend_running "$backend" 20; then
    warn "$backend 在 20s 内没有进入 running（配置有问题时它会 crash-loop）——先不起 $frontend。"
    warn "  这是健康检查随后要判定的事；若用了 SYNC_CONFIG，脚本会接着回滚配置。"
    return 0
  fi
  if ! exists "$frontend"; then
    warn "容器 $frontend 不存在——$label 的应用边缘（nginx）没有起来。"
    return 0
  fi
  local ferr
  if [ "$backend_restarted" = 1 ]; then
    # ⚠️ 这里**不能用「前端还在 running 就不动它」来判断**：后端换了 netns 之后，前端可能仍然
    #    running、却滞留在那个已经消失的旧 netns 里（连不上后端 127.0.0.1:8081）——`docker start`
    #    对已 running 的容器是空操作，救不了它，必须 restart 才会重新加入当前的 netns。
    if ferr=$(docker restart "$frontend" 2>&1); then
      ok "已启动 $backend，并重启 $frontend（重新加入后端 netns，端口绑定保持不变）"
    else
      warn "重启 $frontend 失败：${ferr}"
      warn "  （前端共享后端 netns；后端刚起来时的这个窗口最容易失败，随后的健康检查会判定）"
    fi
  elif running "$frontend"; then
    ok "$backend 与 $frontend 都在运行（后端本次未被重启，前端 netns 无需重挂）"
  elif ferr=$(docker start "$frontend" 2>&1); then
    ok "已启动 $backend + $frontend（端口绑定保持不变）"
  else
    warn "启动 $frontend 失败：${ferr}"
  fi
}

start_all_pairs() {
  start_pair_tracked "$LIVE_BACKEND" "$LIVE_FRONTEND" "线上" "$LIVE_FRONTEND_WAS_RUNNING"
  if [ "$GREEN_WAS_RUNNING" = 1 ]; then
    start_pair_tracked "$GREEN_BACKEND" "$GREEN_FRONTEND" "绿（冒烟用的临时容器，线上不受影响）" 0
  fi
  # 到这一步容器都尝试起过了；把标志清掉，避免 EXIT trap 再起一遍。
  PAIRS_STOPPED=0
}

# 与 assert_reset_target 同一条规矩：清空目标必须是**本脚本管理的那个 Redis 容器**。
# flush_redis 只认 $REDIS_CONTAINER，从不按 [redis].url 的 host 建连接——URL 指向外部实例时
# FLUSHALL 会打在错的实例上（应用真正的 Redis 里会话/授权版本缓存照旧留着），却报「已清空」。
# 这是 MySQL 侧 F2 的同一类缺陷，2026-09-24 复审发现只修了一侧。
# 成功时不打印（cmd_refresh 的预校验会先调它，避免输出重复）。
assert_redis_target() {
  # $1 可选：要校验的配置文件路径（默认 $APP_DIR/$CONFIG_FILE）。
  # ⚠️ 必须能传路径：预校验跑在 install_staged_config **之前**，只看默认路径的话，SYNC_CONFIG=1
  #    推送的新配置改了 [redis].url 主机时就核不到——`-SyncConfig -ResetDb` 会在 DROP DATABASE
  #    **之后**的 flush_redis 才拒绝，留下「库已清空、Redis 没清、配置被回滚」的半截状态
  #    （2026-09-24 彩排实测 + 复审发现。MySQL 侧因为 load_mysql_env 显式传了 staged 路径而没有这个问题）。
  local cfg="${1:-$APP_DIR/$CONFIG_FILE}" url hostport host
  # 先确认文件在：否则 awk 会自己报 fatal 并以 2 退出，在 `url=$(...)` 处被 set -e 掀掉，
  # 连下面那句 die 都跑不到（首次部署时服务器上还没有配置就是这条路径）。
  [ -f "$cfg" ] || die "找不到要校验的配置文件 $cfg，拒绝清空 Redis。"
  url=$(toml_get redis url "$cfg")
  [ -n "$url" ] || die "配置里读不到 [redis].url，拒绝清空 Redis。"
  case "$url" in
    redis://*) ;;
    *) die "[redis].url 必须以 redis:// 开头，实际读到：$url" ;;
  esac
  hostport=${url#redis://}
  hostport=${hostport%%/*}      # 去掉 /db
  hostport=${hostport##*@}      # 去掉可选的 user:pass@
  host=${hostport%%:*}          # 去掉 :port
  [ "$host" = "$REDIS_CONTAINER" ] || die "[redis].url 的主机名是 ${host}，不是本脚本管理的容器 ${REDIS_CONTAINER}。
      本脚本只对 $REDIS_CONTAINER 执行 FLUSHALL（不按 URL 里的 host 建连接）——对外部 Redis
      执行这里等于打错目标，所以直接拒绝。若这确实是本机容器：请把 [redis].url 的主机名改成
      ${REDIS_CONTAINER} 再重跑。真是外部实例：清空请由 DBA 手工处理。"
}

# 清空 Redis。与本系统同源的会话/授权版本缓存/验证码/step-up 令牌都在这里，
# 清了只需重新登录；而**只清库不清 Redis** 会留下指向已消失用户的缓存与已签发令牌，
# 让「重置」变成一半的状态。
flush_redis() {
  # 再核一遍目标（cmd_refresh 的预校验已经核过，这里是防「将来被别处调用」的兜底）。
  assert_redis_target
  running "$REDIS_CONTAINER" || die "$REDIS_CONTAINER 未运行，无法清空 Redis。先跑 infra-up。"
  # stderr 必须与 stdout 分开（同 mysql_scalar 的判据）：下面那句 `= "OK"` 是**严格相等**，
  # 一旦 stderr 上有一行告警被合并进来，成功也会被读成失败。
  local out errf rc=0
  errf=$(mktemp) || { err "无法创建临时文件（mktemp 失败）"; return 1; }
  out=$(docker exec -i "$REDIS_CONTAINER" redis-cli FLUSHALL 2>"$errf") || rc=$?
  # redis-cli 连不上时也可能返回 0 并把错误打在 stdout，所以两个判据都要看。
  if [ "$rc" -ne 0 ] || [ "$out" != "OK" ]; then
    err "清空 Redis 失败（退出码 $rc，redis-cli 输出：${out:-<空>}）"
    sed 's/^/      /' "$errf" >&2
    rm -f "$errf"
    return 1
  fi
  if [ -s "$errf" ]; then sed 's/^/      [redis-cli] /' "$errf" >&2; fi
  rm -f "$errf"
  ok "已清空 Redis（会话 / 授权版本缓存 / 验证码 / step-up 令牌；Redis 本就不备份）"
}

# 清库之后**必须验证表真的被建出来了**。
# 为什么不能只靠 readiness 探针：它只执行 `SELECT 1`（crates/yang-db/src/mysql/database.rs），
# 空库照样返回 200 —— 探针通过不等于 schema 已重建。而 schema 同步只在进程启动时跑一次
# （src/bootstrap.rs），所以「应用起来了」与「表建好了」是两件事，必须分别证明。
verify_schema_rebuilt() {
  local n
  n=$(mysql_scalar "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='$MYSQL_DATABASE'") || return 1
  # 先确认拿到的是个整数再比大小：把「不可解析」当成「0」会把一份**正确的配置**判成失败并回滚。
  require_integer "表数查询结果" "$n" || return 1
  if [ "$n" -gt 0 ]; then
    ok "schema 已重建：库 $MYSQL_DATABASE 现有 $n 张表"
    return 0
  fi
  err "库 $MYSQL_DATABASE 里一张表都没有——应用启动时的 schema 同步没有生效。"
  err "  排查：docker logs --tail 100 $LIVE_BACKEND"
  return 1
}

# 把暂存的新配置**就地**装成正式配置。
# ⚠️ 绝不能用 mv / rename / sed -i / busybox 的 cp —— 配置是以**单文件 bind mount**
#    注入容器的（start_pair 的 `-v .../config.toml:ro`），容器认的是**创建时那个 inode**。
#    实测（Docker 29.7.2）：换 inode 的写法会让运行中的容器一直读到旧内容，`docker restart`
#    也救不回来（挂载源在创建时就绑死了）。shell 重定向 `cat src > dst` 是**原地截断**，
#    inode 与权限位都不变，容器重启后读到新值。
install_staged_config() {
  local staged="$APP_DIR/$STAGED_CONFIG" dst="$APP_DIR/$CONFIG_FILE"
  [ -f "$staged" ] || die "找不到暂存的新配置 $staged（deploy.ps1 -SyncConfig 会先 scp 上来）。"
  # 暂存路径与正式路径相同的话，下面那句 `cat "$staged" > "$dst"` 会**先把自己截断成 0 字节**
  # 再拷回去 —— 生产配置当场变空文件（备份也不会被自动还原）。STAGED_CONFIG 在文件头被列为
  # 可覆盖变量，所以补一道一行的护栏。
  [ "$staged" != "$dst" ] || die "STAGED_CONFIG 不能与 CONFIG_FILE 相同（会自我截断）：$staged"
  # 它含明文凭据，先收紧权限（cmd_refresh 也收紧一次，这里再兜一道）。
  chmod 600 "$staged" 2>/dev/null || true

  if [ -f "$dst" ]; then
    CONFIG_BACKUP="$dst.bak.$(date +%Y%m%d-%H%M%S)"
    # **不用 cp -p**：保留源文件时间戳会让备份的 mtime 等于「config 上次被写入的时间」，而
    # prune_config_backups 是按 mtime 排序的 —— 那样可能把刚生成的这份（本次的安全网）当成
    # 最旧的删掉（2026-09-24 复审发现）。mtime=现在才是备份真正的生成时间。
    cp "$dst" "$CONFIG_BACKUP" || die "备份现有配置失败：$CONFIG_BACKUP"
    # 备份里是同一份明文凭据：权限收到 600，并只保留最近几份（否则每次 -SyncConfig 都会在
    # 服务器上多加一份可读的生产凭据，永不清理）。
    chmod 600 "$CONFIG_BACKUP" 2>/dev/null || true
    prune_config_backups "$dst"
    ok "现有配置已备份为 $CONFIG_BACKUP（chmod 600，只保留最近 5 份）"
  else
    CONFIG_BACKUP=""
    warn "服务器上原本没有 $dst，将新建一份（权限按当前 umask）。"
  fi

  # 写之前先核对「容器正在读的就是这个文件」，见该函数的注释（防静默不生效）。
  assert_config_mount_is_live || return 1

  if ! cat "$staged" > "$dst"; then
    # ⚠️ 重定向在 cat 读取**之前**就把目标截断了，所以失败时 $dst 是「已截断 + 可能写了一半」，
    #    绝不是「保持原样」（2026-09-24 复审发现旧文案与事实不符）。既然刚备份过，就直接还原。
    err "写入配置失败：$dst（重定向可能已经把它截断/写残）"
    if restore_config_backup; then
      warn "已把 $dst 就地还原为改动前的内容。"
    else
      err "就地还原失败——请手工把 $CONFIG_BACKUP 复制回 $dst 再继续。"
    fi
    return 1
  fi
  rm -f "$staged"
  CONFIG_APPLIED=1
  # 措辞按**实际**核对结果：没核对就不能说「已核对」（彩排实测过这条不实文案）。
  if [ "$MOUNT_PROBE_RESULT" = "passed" ]; then
    ok "配置已就地更新：$dst（原地截断写，inode 未变，且已核对容器读得到新内容）"
  else
    ok "配置已就地更新：$dst（原地截断写，inode 未变）"
    warn "  挂载核对未执行（没有可用的线上容器/容器内读不到该文件）：本次写入是否对它生效**未经验证**。"
  fi
}

# 只保留最近 5 份配置备份。
prune_config_backups() {
  local dst="$1" keep=5 files f i=0
  files=$(ls -1t "$dst".bak.* 2>/dev/null || true)
  for f in $files; do
    i=$((i + 1))
    if [ "$i" -gt "$keep" ]; then
      rm -f "$f" && info "已清理更早的配置备份：$f"
    fi
  done
}

# 核对「容器正在读的配置」就是宿主这个路径当前指向的那个 inode。
#
# 为什么必须查：配置是单文件 bind mount，容器绑的是**创建那一刻的 inode**。如果该文件在容器
# 创建之后被换过 inode（`sed -i`、vim 的 backupcopy、`mv`、busybox 的 `cp` 都会），那么下面的
# 「就地写入」写的是当前路径上的新 inode，而容器读的仍是旧的——换配置**静默不生效**，且因为
# 应用读旧配置也一切正常，健康检查照样全绿、照样打印 ✔（2026-09-24 审查发现，正是本仓库
# 最忌讳的那类事故）。
#
# 判据用**功能核对**而不是比对 inode 数字：把探测标记追加到宿主文件，再看容器里能不能读到。
# inode 编号在 bind mount 两侧不一定相同（Docker Desktop 的 9p/virtiofs 就是如此），而
# 「容器能不能看到我刚写进去的东西」才是真正要证明的事。
# 结果写进 $MOUNT_PROBE_RESULT（passed / skipped），供 install_staged_config 如实措辞——
# 「已核对」这三个字不能在想跳过核对时也照样打印（2026-09-24 彩排实测：容器不存在时它仍打了
# 「且已核对容器读得到新内容」，等于替一个没发生的检查背书）。
MOUNT_PROBE_RESULT=""
assert_config_mount_is_live() {
  local dst="$APP_DIR/$CONFIG_FILE" marker probe_out started_for_probe=0
  MOUNT_PROBE_RESULT="skipped"
  if ! exists "$LIVE_BACKEND"; then
    # 首次部署：还没有容器，谈不上「挂载源是哪个 inode」——随后的 start_pair（smoke/cutover）
    # 会在创建容器时按当时的路径挂载，没问题。
    info "$LIVE_BACKEND 不存在（首次部署？），跳过挂载核对。"
    return 0
  fi
  if ! running "$LIVE_BACKEND"; then
    # 容器存在但停着：**不能放行**——挂载源在创建时就绑死了，配置若在容器停着时被换过 inode，
    # 起来后读的仍是旧的那份，而健康检查只看服务能不能起（用旧配置往往一切正常），于是
    # 「换了配置但没生效」会一路绿灯（2026-09-24 复审指出的残留缺口）。
    # 核对需要容器在跑，所以先把它起一下——反正随后 stop_all_pairs/start_all_pairs 还会再来一轮。
    info "$LIVE_BACKEND 当前是停止态，先起一下才能核对挂载（随后流程还会再停/起一次）。"
    started_for_probe=1
    if ! docker start "$LIVE_BACKEND" >/dev/null 2>&1 || ! wait_backend_running "$LIVE_BACKEND" 20; then
      warn "$LIVE_BACKEND 起不来（配置有问题时它会 crash-loop），无法核对挂载。"
      warn "  若它当初挂的是别的 inode，本次写入对它不生效；随后的健康检查会把起不来的问题暴露出来。"
      return 0
    fi
  fi
  marker="# mount-probe-$$"
  printf '%s\n' "$marker" >> "$dst" || { err "无法把探测标记写进 $dst"; return 1; }
  # ⚠️ 判据的三个坑，都是踩过的：
  #  1) 命令要包进容器侧 sh -c，让 /app/config.toml 留在引号里：Git Bash/MSYS 会把**裸的**绝对
  #     路径参数改写成 C:/Program Files/Git/app/config.toml，于是「好配置 + 挂载正常」也被判成
  #     读不到（2026-09-24 彩排实测的假失败）。
  #  2) **不能拿退出码当判据**：`grep -c` 在没有匹配行时打印 0 且**退出码为 1**，docker exec 原样
  #     透传——于是「容器读不到刚写进去的内容」（这个函数唯一要拦的情况）会被误判成「docker exec
  #     失败」而放行（2026-09-24 复审发现：报错分支成了死代码）。所以让容器侧自己把结果**归一化成
  #     记号**打印，退出码只用来判断「有没有跑到容器里」。
  #  3) 不合并 stderr：docker 的报错文本不该混进记号。
  local prc=0
  probe_out=$(docker exec "$LIVE_BACKEND" sh -c \
      "if test -r /app/config.toml; then grep -q -- '$marker' /app/config.toml && echo MATCH || echo NOMATCH; else echo UNREADABLE; fi" \
      2>/dev/null) || prc=$?
  if [ "$prc" -ne 0 ]; then
    warn "无法在容器内执行挂载核对（docker exec 退出码 $prc，容器可能在 restarting）——跳过核对。"
    warn "  这不等于 inode 错配；$dst 末尾留了一行 '$marker' 注释（无害，可手工删掉）。"
    return 0
  fi
  case "$probe_out" in
    MATCH)
      MOUNT_PROBE_RESULT="passed"
      ok "挂载核对通过：容器读得到刚写进 $dst 的内容（挂的就是这个 inode）"
      return 0 ;;
    UNREADABLE)
      warn "容器读不到 $dst 这个挂载点（容器内 /app/config.toml 不可读）——跳过核对。"
      warn "  应用启动时也会因为读不到配置而失败，随后的健康检查会暴露它。"
      return 0 ;;
    NOMATCH) : ;;   # 落到下面按「换了 inode」处理
    *)
      warn "挂载核对的返回值无法识别：[$probe_out] ——跳过核对。"
      return 0 ;;
  esac
  # 拒绝路径必须把刚追加的那行**删掉**：一次「什么都没装、被拒绝」的操作不该改动线上配置
  # （否则外部的配置审计/gitops 漂移检测会把它当成配置变更——2026-09-24 彩排实测）。
  err "容器读不到刚写进 $dst 的内容——它挂的是**另一个 inode**（该文件在容器创建后被换过，"
  err "  或者挂载点被建成了目录）。继续写下去不会对它生效，docker restart 也救不回来。"
  err "  请先重建容器再重试："
  err "    $0 deploy        # 或 $0 cutover（两者都会重新创建容器）"
  if strip_probe_marker "$dst" "$marker"; then
    err "  （刚追加的探测注释已删除，$dst 内容回到原样。）"
  else
    err "  （探测注释未能自动删除——见上面的告警，$dst 末尾那行可手工删掉。）"
  fi
  if [ "$started_for_probe" = 1 ]; then
    # 「失败不改动系统状态」：它原本是停止态，是我们为了核对才起起来的，那就停回去。
    docker stop "$LIVE_BACKEND" >/dev/null 2>&1 || true
    warn "  它原本是停止态，本次为核对起过它，现已停回原状（$LIVE_FRONTEND 全程未动）。"
  fi
  return 1
}

# 就地删掉尾部那行探测标记（保持 inode：读出内容再 cat 回去）。
# 两种形态都要处理，因为 `printf '%s\n' >>` 是**追加**：
#   A) 文件本来以换行结尾 → 标记自成一行，删掉整行；
#   B) 文件末行**没有**结尾换行 → 标记粘在末行尾部（`url = "x"# mount-probe-123`）——
#      只看「末行 == 标记」会判不成立、静默空转，却仍对调用者报「已删除」
#      （2026-09-24 终版复审发现：这既是假报成功，也确实改动了线上配置）。
# 形态都不匹配时不硬删，如实返回失败（调用方据此措辞）。
strip_probe_marker() {
  local dst="$1" marker="$2" tmp last
  tmp=$(mktemp) || { warn "无法创建临时文件，探测标记留在 $dst 末尾（无害）。"; return 1; }
  last=$(tail -n 1 "$dst" 2>/dev/null)
  if [ "$last" = "$marker" ]; then
    head -n -1 "$dst" > "$tmp"
  elif [ "$last" != "${last%"$marker"}" ]; then
    # 形态 B：把标记从末行尾部削掉，并且**不加回换行**（还原文件原本的结尾形态）。
    head -n -1 "$dst" > "$tmp"
    printf '%s' "${last%"$marker"}" >> "$tmp"
  else
    rm -f "$tmp"
    warn "在 $dst 末尾找不到探测标记（形态与预期不符），未做改动——请手工核对这一行：$marker"
    return 1
  fi
  if cat "$tmp" > "$dst"; then
    rm -f "$tmp"
    info "已删除 $dst 末尾的探测标记（内容回到追加前）"
    return 0
  fi
  rm -f "$tmp"
  warn "删除探测标记失败，$dst 末尾多了一行 '$marker'（无害，可手工删掉）。"
  return 1
}

# refresh 的退出兜底。两件事必须在**任何**退出路径上做：
#   1) 容器停过就必须尝试起回去——2026-09-24 审查发现：reset_database 里任何 die 都会直接
#      exit 1，而「起回来」只写在健康检查失败分支里，于是「清库失败」会把站点留在**停着的**
#      状态（比失败本身更糟）。
#   2) 暂存配置是含明文凭据的文件，不该留在磁盘上（chmod 600 之后也一样）。
refresh_on_exit() {
  local rc=$?
  # 顺序是有意的（2026-09-24 复审）：**清理暂存文件排在最前**。它是含明文凭据的文件，而下面的
  # start_all_pairs 会走 exists/running —— 那两个函数在 docker 不可达时是 `die`（直接 exit），
  # 排在它后面的清理就永远跑不到（实测过：退出码被改写、暂存文件留在盘上）。
  if [ "$SYNC_CONFIG" = 1 ] && [ -f "$APP_DIR/$STAGED_CONFIG" ]; then
    rm -f "$APP_DIR/$STAGED_CONFIG" || true
    info "已清理暂存配置 $APP_DIR/$STAGED_CONFIG（未安装成功的那份不留在磁盘上）"
  fi
  # 配置装上了、但还没通过健康检查就中止：先把它退回改动前那份，再起容器。否则一份**从未被
  # 验证过**的配置就成了线上的运行配置（最现实的触发点是 ensure_database 失败：root 凭据不对、
  # MySQL 没起——那时容器已经停着，一条 die 就走到这里）。
  if [ "${CONFIG_APPLIED:-0}" = 1 ] && [ -n "${CONFIG_BACKUP:-}" ]; then
    warn "配置已就地安装但尚未通过健康检查就中止了——先回滚配置，再起容器。"
    if restore_config_backup; then CONFIG_APPLIED=0; else
      warn "自动回滚配置失败，请手工把 $CONFIG_BACKUP 复制回 $APP_DIR/$CONFIG_FILE。"
    fi
  fi
  if [ "$PAIRS_STOPPED" = 1 ]; then
    warn "脚本以退出码 $rc 中止，而容器之前被停过（清库/换配置中途）——正在尝试把容器起回去…"
    # 放进子 shell：start_all_pairs 内部的 die（exists/running 在 docker 不可达时会 die）只结束
    # 子 shell，既不改写本脚本的退出码，也不会打断上面两段清理。
    ( start_all_pairs ) || true
    warn "  若上面是清库失败，库可能停在半途（已 DROP 但没重建完）。修好原因后重跑一次 refresh"
    warn "  （SYNC_CONFIG/RESET_DB 与这次相同），它会把库与账号重新对齐。"
  fi
}

# 就地还原备份。同样必须用 `cat > dst`：换 inode 的还原是假还原（容器仍读新配置）。
restore_config_backup() {
  [ -n "${CONFIG_BACKUP:-}" ] && [ -f "$CONFIG_BACKUP" ] || return 1
  cat "$CONFIG_BACKUP" > "$APP_DIR/$CONFIG_FILE" || return 1
  ok "配置已回滚为 $CONFIG_BACKUP"
}

# 清空并重建 [mysql].url 指向的库。调用方必须**先**停下来应用容器：
# 应用带着活动连接时会持续报错，而它一旦被 `--restart unless-stopped` 重启，就会用**旧**
# 代码把旧 schema 同步进刚清空的库——那正好毁掉「用清库来应用破坏性 schema 变更」的意图。
reset_database() {
  local tables size
  tables=$(mysql_scalar "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='$MYSQL_DATABASE'") \
    || die "读不到 $MYSQL_DATABASE 的表数量，拒绝继续（无法确认目标库的身份）。"
  size=$(mysql_scalar "SELECT IFNULL(ROUND(SUM(data_length+index_length)/1024/1024,1),0) FROM information_schema.tables WHERE table_schema='$MYSQL_DATABASE'") \
    || die "读不到 $MYSQL_DATABASE 的占用大小，拒绝继续。"
  warn "即将清空：库 $MYSQL_DATABASE（当前 ${tables:-0} 张表 / 约 ${size:-0} MB），容器 $MYSQL_CONTAINER"

  mysql_exec "DROP DATABASE IF EXISTS \`$MYSQL_DATABASE\`" \
    || die "DROP DATABASE 失败，已中止（库可能仍在，请手工核对）。"
  ok "已删除库 $MYSQL_DATABASE 及其全部数据"
  # 字符集/排序规则与 schema 同步渲染的建表语句 (render.rs) 及 ensure_database 保持一致，
  # 否则会出现库与表两套 collation 的漂移。
  mysql_exec "CREATE DATABASE \`$MYSQL_DATABASE\` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci" \
    || die "重建库失败，请手工建库后再重跑。"

  # 账号也要按配置对齐。两个理由：
  #   (1) MySQL 8.0 的 GRANT **不再隐式建用户**（旧脚本那句 GRANT 因此对不存在的账号是无效的）；
  #   (2) 账号与密码**只在数据卷为空时**由 compose 初始化——改了 [mysql].url 的账号/密码却
  #       不重建卷的话，应用只会以 Access denied 起不来（README「Access denied」那条症状）。
  #   清库这个动作本来就意味着「配置是唯一事实源」，所以在这里把账号一并同步过去。
  mysql_exec "CREATE USER IF NOT EXISTS '$MYSQL_USER'@'%' IDENTIFIED BY '$MYSQL_PASSWORD'" \
    || die "创建应用账号 $MYSQL_USER 失败。"
  mysql_exec "ALTER USER '$MYSQL_USER'@'%' IDENTIFIED BY '$MYSQL_PASSWORD'" \
    || die "同步应用账号密码失败。"
  mysql_exec "GRANT ALL PRIVILEGES ON \`$MYSQL_DATABASE\`.* TO '$MYSQL_USER'@'%'" \
    || die "授权失败。"
  ok "空库已就绪，应用账号 $MYSQL_USER@'%' 已按 [mysql].url 对齐（密码已同步、权限已授予）"

  # Redis 与库是同源状态，必须一起清（调用方已把它作为重置的一部分）。
  flush_redis || die "Redis 未清空：库已清空而 Redis 没清，状态是半截的。请手工执行：
      docker exec $REDIS_CONTAINER redis-cli FLUSHALL"
  ok "数据库与 Redis 均已清空"
}

# 清库的确认闸门。缺了 RESET_DB_CONFIRM=yes 就**拒绝执行**（不是默认放行）。
require_reset_confirmation() {
  case "$RESET_DB_CONFIRM" in
    yes) ok "已收到 RESET_DB_CONFIRM=yes（调用方已完成交互确认）" ;;
    *) die "清空数据库不可逆，必须显式确认后再执行。请加：
      RESET_DB_CONFIRM=yes $0 refresh
    （用 deploy.ps1 时，它会先做交互确认，通过后才置这个变量；\`-Yes\` 可跳过交互。）" ;;
  esac
  case "$MYSQL_USER" in
    root|mysql.sys|mysql.session)
      die "拒绝以 [$MYSQL_USER] 作为应用账号去 ALTER USER——那可能改掉 MySQL 自己的账号。" ;;
  esac
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

# 刷新配置 / 重置数据库。
#
# 为什么这两件事合成一条子命令（而不是 sync-config + reset-db 两条）：
#   两者都要求**重启应用容器**（配置是启动时读一次的文件；schema 同步也只在启动时跑一次）。
#   拆成两条会白重启一轮，还会出现「配置已换成新库名、库却还没重建」的中间态——那时应用
#   连不上库，健康检查必然失败，看起来像「新配置是坏的」。合成一条还能让失败回滚同时知道
#   「配置改没改、库清没清」。
cmd_refresh() {
  need docker; need curl
  if [ "$SYNC_CONFIG" != 1 ] && [ "$RESET_DB" != 1 ]; then
    die "refresh 至少要给一个动作：SYNC_CONFIG=1（安装新配置）和/或 RESET_DB=1（清空数据库）。见 $0 的 usage。"
  fi
  # 退出兜底：容器停过就起回去、暂存配置不留在盘上（理由见 refresh_on_exit 的注释）。
  trap refresh_on_exit EXIT

  # 「换库名」时旧库会被留成孤儿：我们只清 [mysql].url 现在指向的那个库。
  local old_database=""
  if [ -f "$APP_DIR/$CONFIG_FILE" ]; then
    old_database=$(toml_get mysql url | sed -E 's#^.*/##; s#\?.*$##')
  fi

  # ---- 1) 只读准备：任何一项不合格都在**动任何东西之前**中止 ----
  if [ "$SYNC_CONFIG" = 1 ]; then
    [ -f "$APP_DIR/$STAGED_CONFIG" ] \
      || die "找不到暂存的新配置 $APP_DIR/$STAGED_CONFIG（deploy.ps1 -SyncConfig 会先 scp 上来）。"
    # 暂存文件含明文凭据：在**任何进一步动作之前**先收紧权限。以前 chmod 只在安装函数里做，
    # 于是「预校验就失败」的路径会让它一直以 644 躺在服务器上（2026-09-24 审查发现）。
    # 退出时 refresh_on_exit 还会把它删掉。
    chmod 600 "$APP_DIR/$STAGED_CONFIG" 2>/dev/null || true
    # 先验暂存文件，再动服务器上那份好的：顺序反过来就等于「覆盖完才发现新配置不合格」。
    require_config "$APP_DIR/$STAGED_CONFIG"
    # 顺带把 [mysql].url 试解析一遍（写坏了当场中止），并把 MYSQL_* 设成**新配置**的值。
    load_mysql_env "$APP_DIR/$STAGED_CONFIG"
  else
    require_config
    load_mysql_env
  fi
  if [ "$RESET_DB" = 1 ]; then
    # 顺序有意为之：**先做只读的拒绝判断，再碰任何会写盘的东西**。
    # load_root_password 在 `.mysql-root-password` 不存在时会**生成并落盘**一个随机 root 密码，
    # 排在拒绝判断之前的话，「本该被拒绝」的请求也会留下一个与容器实际 root 密码不符的文件，
    # 之后 ensure_database 会拿它去认证并 die，把后续所有清库/建库都卡死（2026-09-24 复审发现）。
    assert_reset_target
    # Redis 也要核目标：清错实例同样会造成「应用真正的会话/授权缓存没清，却报已清空」。
    # 有 SYNC_CONFIG 时核**即将生效的那份**（staged），否则核现装的那份——与 MySQL 侧对称。
    if [ "$SYNC_CONFIG" = 1 ]; then
      assert_redis_target "$APP_DIR/$STAGED_CONFIG"
    else
      assert_redis_target
    fi
    require_reset_confirmation
    load_root_password
    if [ -n "$old_database" ] && [ "$old_database" != "$MYSQL_DATABASE" ]; then
      warn "库名从 $old_database 变成了 $MYSQL_DATABASE：本次只清后者，**旧库会原样留着**。"
      warn "  需要的话手工删：docker exec -it $MYSQL_CONTAINER mysql -uroot -p -e 'DROP DATABASE \`$old_database\`'"
    fi
  fi

  # ---- 2) 安装新配置（就地写，见 install_staged_config 的注释） ----
  if [ "$SYNC_CONFIG" = 1 ]; then
    install_staged_config
    require_config   # 装完再验一遍**落盘的那份**，防写入被截断
  fi

  # ---- 3) 停下来（清库与换配置都要重启应用，见本函数头部说明） ----
  stop_all_pairs

  # ---- 4) 清库 + 清 Redis ----
  if [ "$RESET_DB" = 1 ]; then
    reset_database
  fi

  # ---- 5) 起回来 ----
  # ensure_database 兜住「新配置指向了一个还不存在的库」：应用只建表不建库，缺库时连接会
  # 直接失败。它在这里是幂等的（库已存在就只报一句「已存在」）。
  ensure_database
  start_all_pairs

  # ---- 6) 验证 ----
  # 探针目标取**容器实际的绑定**，而不是脚本环境里的期望值：这对容器可能是更早用别的
  # LIVE_HOST_PORT / BIND_ADDR 创建出来的（端口绑定在创建时就固定），拿期望值去探会得到
  # 「假失败」——而假失败会让下面误触发配置回滚，看起来像「新配置是坏的」。
  local edge_port="$LIVE_HOST_PORT" metrics_port="$METRICS_LIVE_PORT"
  local edge_bind="$BIND_ADDR" metrics_bind="$METRICS_BIND_ADDR"
  if exists "$LIVE_BACKEND"; then
    local b_e b_m
    b_e=$(container_binding "$LIVE_BACKEND" 8081)
    b_m=$(container_binding "$LIVE_BACKEND" 9090)
    if [ -n "$b_e" ] && [ -n "$b_m" ]; then
      edge_bind=${b_e%%:*};    edge_port=${b_e##*:}
      metrics_bind=${b_m%%:*}; metrics_port=${b_m##*:}
      if [ "$edge_port" != "$LIVE_HOST_PORT" ] || [ "$edge_bind" != "$BIND_ADDR" ]; then
        info "按容器实际绑定探测：边缘 ${b_e}、管理面 ${b_m}（环境里给的是 ${BIND_ADDR}:${LIVE_HOST_PORT} / ${METRICS_BIND_ADDR}:${METRICS_LIVE_PORT}）"
      fi
    else
      warn "读不到 $LIVE_BACKEND 的端口绑定，按环境变量探测：${BIND_ADDR}:${LIVE_HOST_PORT} / ${METRICS_BIND_ADDR}:${METRICS_LIVE_PORT}"
    fi
  fi

  local verified=0 app_checked=0
  if exists "$LIVE_BACKEND"; then
    app_checked=1
    if health_check "$edge_port" "$metrics_port" "线上" "$edge_bind" "$metrics_bind"; then
      verified=1
      # 这条配置已经被健康检查证明可用：取消失败兜底里的「回滚未验证配置」。
      CONFIG_APPLIED=0
    elif wait_ready "$metrics_port" 5 "$metrics_bind"; then
      # 一次针对性的自愈：readiness 通、边缘不通。这时最常见的原因**不是配置**，而是后端在
      # 崩溃/重启期间换了 netns，而 frontend 不会自己跟过去（Docker 的 `--network container:`
      # 就是这么工作的；彩排实测：MySQL 停机时后端被 --restart 反复拉起，前端的 /sys/class/net
      # 只剩 lo，环境恢复后 edge 一直 000，必须手工重启前端）。前端无状态，重启一次代价极小；
      # 若真是配置坏了，重启前端也救不回来，下面照旧回滚配置。
      warn "后端 readiness 已通过但应用边缘不通——疑似前端滞留在后端旧 netns，重启一次前端再试。"
      if docker restart "$LIVE_FRONTEND" >/dev/null 2>&1; then
        sleep 2
        if health_check "$edge_port" "$metrics_port" "线上（重启前端后）" "$edge_bind" "$metrics_bind"; then
          verified=1
          CONFIG_APPLIED=0
          warn "  边缘已恢复——原因就是前端 netns 滞留（后端重启换代，前端不会自己跟过去）。"
        fi
      else
        warn "重启 $LIVE_FRONTEND 失败。"
      fi
    fi
    # 清库之后还要单独证明「表真的被建出来了」：readiness 只做 SELECT 1，空库也返回 200。
    if [ "$RESET_DB" = 1 ] && [ "$verified" = 1 ]; then
      verify_schema_rebuilt || verified=0
    fi
  else
    warn "线上容器 $LIVE_BACKEND 不存在，本次没有可验证的应用。"
    warn "  schema 尚未创建——它由应用启动时的同步建出来，等下次 deploy 之后才会有。"
  fi

  # ---- 7) 失败处理：先把配置回滚，再如实报告 ----
  if [ "$app_checked" = 1 ] && [ "$verified" != 1 ]; then
    err "refresh 之后的健康检查未通过。"
    if restore_config_backup; then
      CONFIG_APPLIED=0
      stop_all_pairs
      start_all_pairs
      if exists "$LIVE_BACKEND" && health_check "$edge_port" "$metrics_port" "回滚后" "$edge_bind" "$metrics_bind"; then
        ok "配置已回滚，线上已恢复。**本次的新配置没有生效**，请先在本机修好它再重试。"
      else
        err "配置已回滚，但健康检查仍未通过——问题不只是配置。"
        err "  排查：docker logs --tail 100 $LIVE_BACKEND"
      fi
    else
      warn "本次没有可回滚的配置（未用 SYNC_CONFIG，或服务器上原本没有配置）。"
    fi
    if [ "$RESET_DB" = 1 ]; then
      err "注意：数据库与 Redis 的清空**无法撤销**（MySQL 无备份机制）。"
    fi
    exit 1
  fi

  if [ "$app_checked" != 1 ]; then
    # 没有线上容器 = 什么都没被验证过（schema 也不会被重建）。这时**不能**打 ✔：调用方
    # （人，或 deploy.ps1 收尾那句「完成」）会把它读成「重置成功」（2026-09-24 审查发现）。
    # 同时必须把 CONFIG_APPLIED 清掉：没有任何容器会读到这份配置，它就该留在盘上；不清的话
    # EXIT trap 会把用户刚推上去的配置**回滚成旧的那份**（首次部署时旧的那份往往还是带
    # replace-with-* 占位值的模板，下次部署会因为占位值被拒——2026-09-24 复审发现）。
    CONFIG_APPLIED=0
    warn "refresh 已执行，但**未经验证**：没有线上容器可探测，schema 也还没被创建（等下次 deploy）。"
    if [ "$RESET_DB" = 1 ]; then
      warn "  也就是说：库现在是空的，而且没有任何东西会去重建表。"
      warn "  数据库与 Redis 的清空无法撤销（MySQL 无备份机制）。"
    fi
    exit 0
  fi
  ok "refresh 完成 ✔"
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
  # 只有 smoke / deploy 接受位置参数（镜像包名）。这一行以前对所有子命令生效，于是
  # `logs <容器名>` 会顺手把容器名也塞进 BACKEND_TAR。refresh 不接受位置参数，更不该
  # 让一个手滑多写的词悄悄改变镜像包名——所以这里先把子命令圈定，再取参数。
  # 用 if 而不是 `[ -n ... ] && ...`：后者的退出码会污染 case 分支的状态（set -e 的坑）。
  if [ -n "${2:-}" ]; then
    case "${1:-}" in
      smoke|deploy) BACKEND_TAR="$2" ;;
    esac
  fi
  case "${1:-}" in
    status)   cmd_status ;;
    logs)     cmd_logs "${2:-}" ;;
    infra-up) cmd_infra_up ;;
    smoke)    cmd_smoke ;;
    cutover)  cmd_cutover ;;
    deploy)   cmd_deploy ;;
    rollback) cmd_rollback ;;
    refresh)  cmd_refresh ;;
    *) cat <<USAGE
用法：$0 {status|logs|infra-up|smoke|cutover|deploy|rollback|refresh} [参数]

  status                 查看容器 / 镜像 / 网络状态
  logs   [container]     追踪容器实时日志（Ctrl+C 退出，默认线上后端）
  infra-up               启动 MySQL + Redis（只初始化一次）
  smoke  [tar_file]      载入镜像并起绿容器冒烟（不动线上）
  cutover                冒烟通过后切流量（绿 → 线上）
  deploy [tar_file]      一条龙：载入 → 冒烟 → 切流量
  rollback               回退到上一版本（蓝容器）
  refresh                刷新配置和/或重置数据库（由本机 deploy.ps1 的 -SyncConfig /
                         -ResetDb 驱动，也可在此直接调用）。用环境变量选动作，至少一个：
                           SYNC_CONFIG=1         安装 $STAGED_CONFIG 为 $CONFIG_FILE
                           RESET_DB=1            清空并重建 [mysql].url 指向的库 + 清空 Redis
                           RESET_DB_CONFIRM=yes  RESET_DB=1 时必须同时给，缺了就拒绝执行
                         两者可同时给；动作做完会重启应用容器并验证，配置改坏会自动回滚。
                         例：SYNC_CONFIG=1 RESET_DB=1 RESET_DB_CONFIRM=yes $0 refresh

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
  SYNC_CONFIG / RESET_DB / RESET_DB_CONFIRM / STAGED_CONFIG
                  只被 refresh 读取，含义见上面 refresh 那一段。
                  ⚠️ refresh 只能操作**本脚本管理的那些容器**：MySQL 的 SQL 走
                  `docker exec $MYSQL_CONTAINER`、Redis 的清空走 `docker exec $REDIS_CONTAINER`，
                  都**不按 URL 里的 host 建连接**。所以 [mysql].url / [redis].url 的主机名不是
                  对应容器时，清库/清缓存会被直接拒绝（没有逃生门——给一个会打错目标的开关
                  比拒绝更危险）。
USAGE
       exit 1 ;;
  esac
}

main "$@"
