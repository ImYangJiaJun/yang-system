#!/usr/bin/env bash
# =============================================================================
# 飞书用户身份（OAuth）真机验收 —— 只读探针（在服务器上跑）
#
# 用途：确认「发起人姓名 → open_id」这条路径**真的能走通**。分两步：
#       1) 系统持有的 user_access_token 有没有通讯录搜索权限；
#       2) 两个候选接口各返回什么形状——代码解析的字段必须与真实响应对得上。
#
# 为什么第 2 步必要：仓库文档《搜索用户》写的是 `GET /open-apis/search/v1/user`
#       返回 `data.users[].{name,open_id}`，而代码打的是
#       `POST /open-apis/contact/v3/users/search`；2026-10-13 真机取到的响应是
#       `data.items[].{id,display_info,meta_data}` —— 既没有 `open_id` 也没有 `name`。
#       两条路都要看真报文才能定改哪边。
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
  MYSQL_PWD="$(cat .mysql-root-password)" docker exec -e MYSQL_PWD "$MYSQL_CONTAINER" \
    mysql -uroot --default-character-set=utf8mb4 -N -B -e "$1"
}
mysql_t() {
  MYSQL_PWD="$(cat .mysql-root-password)" docker exec -e MYSQL_PWD "$MYSQL_CONTAINER" \
    mysql -uroot --default-character-set=utf8mb4 -t -e "$1"
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

code_of() {
  printf '%s' "$1" | sed -n 's/.*"code"[[:space:]]*:[[:space:]]*\(-\?[0-9]*\).*/\1/p' | head -1
}
verdict() {
  case "$1" in
    0) echo "code=0（接口通）" ;;
    99991672) echo "99991672 应用级缺 scope（后台没开通或审核中）" ;;
    99991679) echo "99991679 用户凭证缺 scope（授权时没要够）" ;;
    99991663) echo "99991663 凭证类型不对（不是 user_access_token）" ;;
    99991668|99991671) echo "$1 凭证形态非法（多半是过期的裸探针）" ;;
    20005|20037|99991677) echo "$1 凭证已失效/过期（应用下次用到会自动刷新）" ;;
    "") echo "没解析出 code" ;;
    *) echo "code=$1" ;;
  esac
}

echo
echo "----------------------------------------------------------"
echo "3a) 代码当前用的：POST /open-apis/contact/v3/users/search"
echo "----------------------------------------------------------"
A="$(curl -s --noproxy '*' -X POST \
  'https://open.feishu.cn/open-apis/contact/v3/users/search?page_size=20' \
  -H "Authorization: Bearer ${TOKEN}" \
  -H 'Content-Type: application/json; charset=utf-8' \
  -d "{\"query\":\"${NAME}\"}")"
echo "$A" | head -c 1600
echo
echo "  => $(verdict "$(code_of "$A")")"
echo "     响应含 \"open_id\"? $(printf '%s' "$A" | grep -c '"open_id"')   含 \"name\"? $(printf '%s' "$A" | grep -c '"name"')"

echo
echo "----------------------------------------------------------"
echo "3b) 文档《搜索用户》描述的：GET /open-apis/search/v1/user"
echo "----------------------------------------------------------"
B="$(curl -s --noproxy '*' -G 'https://open.feishu.cn/open-apis/search/v1/user' \
  --data-urlencode "query=${NAME}" \
  --data-urlencode 'page_size=20' \
  -H "Authorization: Bearer ${TOKEN}")"
echo "$B" | head -c 1600
echo
echo "  => $(verdict "$(code_of "$B")")"
echo "     响应含 \"open_id\"? $(printf '%s' "$B" | grep -c '"open_id"')   含 \"name\"? $(printf '%s' "$B" | grep -c '"name"')"

echo
echo "----------------------------------------------------------"
echo "判读"
echo "----------------------------------------------------------"
CA="$(code_of "$A")"
CB="$(code_of "$B")"
# 代码打的是 3b 那条（`applicant_search_url`，2026-10-13 按这两段实测定的）。
if [ "$CB" = "0" ] && printf '%s' "$B" | grep -q '"open_id"'; then
  echo "  [OK] 代码打的 search/v1/user 通，且返回 name + open_id ——"
  echo "       发起人参数化可用。接着从飞书工作流发一条带 applicant=<姓名> 的派发。"
elif [ "$CA" = "0" ] && printf '%s' "$A" | grep -q '"open_id"'; then
  echo "  [!] 只有 contact/v3/users/search 返回 open_id，而代码打的是 search/v1/user ——"
  echo "      说明上游换了形态，把两份真报文发出来重新对齐。"
elif [ "$CA" = "0" ] || [ "$CB" = "0" ]; then
  echo "  [!] 至少一条通了，但都不返回 open_id/name ——"
  echo "      把上面两份真报文发出来，按真实字段名改解析。"
else
  echo "  [X] 两条都没通，见上面的错误码。"
fi
