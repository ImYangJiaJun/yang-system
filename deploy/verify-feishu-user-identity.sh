#!/usr/bin/env bash
# =============================================================================
# 飞书用户身份（OAuth）真机验收 —— 只读探针（在服务器上跑）
#
# 用途：确认「发起人姓名 → open_id」这条路径**真的能走通**，即系统持有的
#       user_access_token 调得动 `POST /open-apis/contact/v3/users/search`。
#
# 为什么需要它：授权回调成功、控制台显示「已授权」只证明**凭证落库了**，
#       不证明这份凭证有通讯录权限。两者的差别正是这个功能唯一的真机风险
#       （应用级缺 scope 报 99991672、用户凭证缺 scope 报 99991679）。
#
# 全程只读：只 SELECT `feishu_user_token`（不改任何数据）；只调飞书的**搜索**
#       接口（不创建审批实例、不写多维表格）；凭证本体不落 stdout。
#
# 用法（在服务器部署目录里）：
#   bash verify-feishu-user-identity.sh [要搜索的姓名]
# 姓名缺省取授权行里的 user_name（即授权人自己，必然存在于通讯录）。
#
# 本地入口见同目录的 verify-feishu-user-identity.ps1（负责推上来再执行）。
# =============================================================================
set -uo pipefail

APP_DIR="${APP_DIR:-/home/yjj/yang-system}"
MYSQL_CONTAINER="${MYSQL_CONTAINER:-yang-mysql}"
BACKEND_CONTAINER="${BACKEND_CONTAINER:-yang-backend}"
DB="${DB:-yang_system}"

cd "$APP_DIR" || { echo "找不到部署目录：$APP_DIR"; exit 1; }

# 从 [mysql].url 取库名（形如 mysql://user:pass@host:port/dbname）。
if [ -f config.cloud.toml ]; then
  URL_DB="$(awk -F'/' '/^[[:space:]]*url[[:space:]]*=/ {gsub(/["\r]/,""); print $NF; exit}' config.cloud.toml)"
  [ -n "$URL_DB" ] && DB="$URL_DB"
fi

mysql_q() {
  docker exec "$MYSQL_CONTAINER" mysql -uroot -p"$(cat .mysql-root-password)" \
    --default-character-set=utf8mb4 -N -B -e "$1" 2>/dev/null
}
mysql_t() {
  docker exec "$MYSQL_CONTAINER" mysql -uroot -p"$(cat .mysql-root-password)" \
    --default-character-set=utf8mb4 -t -e "$1" 2>/dev/null
}

echo "----------------------------------------------------------"
echo "1) 配置与镜像"
echo "----------------------------------------------------------"
if grep -q '^[[:space:]]*oauth_redirect_uri' config.cloud.toml 2>/dev/null; then
  echo "  oauth_redirect_uri : 已配置"
  grep '^[[:space:]]*oauth_redirect_uri' config.cloud.toml | sed 's/^/    /'
else
  echo "  oauth_redirect_uri : **未配置** —— 派发会直接报「未授权」，不用往下看了"
fi
echo "  后端镜像           : $(docker inspect "$BACKEND_CONTAINER" --format '{{.Config.Image}} @ {{.Created}}' 2>/dev/null || echo '取不到')"

echo
echo "----------------------------------------------------------"
echo "2) 授权行（凭证本体不打印）"
echo "----------------------------------------------------------"
mysql_t "SELECT id, user_name, open_id, scope,
                CHAR_LENGTH(access_token) AS at_len,
                CHAR_LENGTH(refresh_token) AS rt_len,
                FROM_UNIXTIME(authorized_at) AS authorized_at,
                FROM_UNIXTIME(access_token_expires_at) AS at_expires,
                FROM_UNIXTIME(refresh_token_expires_at) AS rt_expires
         FROM ${DB}.feishu_user_token"

TOKEN="$(mysql_q "SELECT access_token FROM ${DB}.feishu_user_token LIMIT 1")"
if [ -z "$TOKEN" ]; then
  echo "  没有授权行 —— 先去控制台点「去授权」。"
  exit 1
fi

EXPIRES_AT="$(mysql_q "SELECT access_token_expires_at FROM ${DB}.feishu_user_token LIMIT 1")"
NOW="$(date +%s)"
if [ -n "$EXPIRES_AT" ] && [ "$EXPIRES_AT" -lt "$NOW" ] 2>/dev/null; then
  echo
  echo "  [!] 库里的 access_token 已过期。应用会在下次用到时**自动刷新**，所以这不代表"
  echo "      功能坏了 —— 但下面的裸探针会拿到凭证过期错误。要验真，先触发一次派发。"
fi

NAME="${1:-}"
[ -z "$NAME" ] && NAME="$(mysql_q "SELECT user_name FROM ${DB}.feishu_user_token LIMIT 1")"
echo
echo "  搜索姓名：${NAME}"

echo
echo "----------------------------------------------------------"
echo "3) 决定性一步：用该凭证调 contact/v3/users/search"
echo "----------------------------------------------------------"
RESP="$(curl -s --noproxy '*' -X POST \
  'https://open.feishu.cn/open-apis/contact/v3/users/search?page_size=5' \
  -H "Authorization: Bearer ${TOKEN}" \
  -H 'Content-Type: application/json; charset=utf-8' \
  -d "{\"query\":\"${NAME}\"}")"

echo "$RESP" | head -c 2000
echo
echo

CODE="$(printf '%s' "$RESP" | sed -n 's/.*"code"[[:space:]]*:[[:space:]]*\(-\?[0-9]*\).*/\1/p' | head -1)"
case "$CODE" in
  0)
    echo "  [OK] code=0 —— 用户凭证能调通讯录搜索，**发起人参数化可用**。"
    echo "       下一步：从飞书工作流发一条带 applicant=<姓名> 的派发，确认解析出 open_id。"
    ;;
  99991672)
    echo "  [X] 99991672 应用级缺 scope：开发者后台「权限管理」没开通通讯录权限，或还在审核。"
    ;;
  99991679)
    echo "  [X] 99991679 用户凭证缺 scope：授权时没要够权限。去控制台点「重新授权」并勾选通讯录权限。"
    ;;
  99991663)
    echo "  [X] 99991663 凭证类型不对：库里存的不是 user_access_token（应不会发生）。"
    ;;
  99991668|99991671)
    echo "  [X] 凭证形态非法：多半是过期的裸探针。先触发一次派发让应用刷新，再重跑。"
    ;;
  20005|20037|99991677)
    echo "  [!] 凭证已失效/过期（code=$CODE）。应用会在下次用到时自动刷新 —— 先触发一次派发再重跑。"
    ;;
  "")
    echo "  [!] 没解析出 code，原始响应见上。"
    ;;
  *)
    echo "  [X] code=$CODE，原始响应见上。对照 docs/architecture/2026-10-13-feishu-user-identity-oauth-design.md 第 7 节的失败文案表。"
    ;;
esac
