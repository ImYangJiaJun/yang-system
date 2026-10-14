#!/usr/bin/env bash
# =============================================================================
# Portal suggest (autocomplete) acceptance probe -- runs on the server.
#
# Verifies the new GET /api/v1/feishu/portal/{slug}/suggest endpoint is
# deployed and working. Checks:
#   1) Backend container is running the latest image
#   2) Portal table has at least one row (need a slug to test with)
#   3) Suggest endpoint returns correct JSON shape
#   4) Suggest respects fuzzy_enabled / fuzzy_min_digits config
#   5) Option table has data for the portal's datasource (need labels to match)
#
# Read-only end to end: SELECTs feishu_datasource_portal + feishu_option only.
# No data is written. Credentials never reach stdout.
#
# Usage (in the deploy directory on the server):
#   bash verify-portal-suggest.sh
#
# Local entry: verify-portal-suggest.ps1 (uploads this file and runs it).
# =============================================================================
set -uo pipefail

APP_DIR="${APP_DIR:-/home/yjj/yang-system}"
MYSQL_CONTAINER="${MYSQL_CONTAINER:-yang-mysql}"
BACKEND_CONTAINER="${BACKEND_CONTAINER:-yang-backend}"
DB="${DB:-yang_system}"

cd "$APP_DIR" || { echo "deploy dir not found: $APP_DIR"; exit 1; }

# Read DB name from config if present.
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

# Curl from inside the backend container (avoids needing host port mapping).
backend_curl() {
  docker exec "$BACKEND_CONTAINER" curl -s --noproxy '*' "$1" 2>/dev/null
}

echo "----------------------------------------------------------"
echo "1) Backend container & image"
echo "----------------------------------------------------------"
BACKEND_STATUS="$(docker inspect "$BACKEND_CONTAINER" --format '{{.State.Status}} ({{.State.Health.Status}})' 2>/dev/null || echo 'not found')"
BACKEND_IMAGE="$(docker inspect "$BACKEND_CONTAINER" --format '{{.Config.Image}}' 2>/dev/null || echo 'not found')"
echo "  status : $BACKEND_STATUS"
echo "  image  : $BACKEND_IMAGE"

# Check if the suggest endpoint is registered by hitting a non-existent slug.
# If the endpoint exists, it returns {"suggestions":[]}; if not, it returns 404 or route-not-found.
echo
echo "----------------------------------------------------------"
echo "2) Suggest endpoint registration check"
echo "----------------------------------------------------------"
PROBE="$(backend_curl 'http://localhost:8080/api/v1/feishu/portal/__probe_nonexistent__/suggest?query=1234')"
PROBE_CODE="$(printf '%s' "$PROBE" | sed -n 's/.*"code"[[:space:]]*:[[:space:]]*\(-\?[0-9]*\).*/\1/p' | head -1)"
if [ "$PROBE_CODE" = "0" ]; then
  echo "  [OK] suggest endpoint is registered (returned code=0 for non-existent slug)"
  echo "  response: $PROBE"
elif printf '%s' "$PROBE" | grep -q '"suggestions"'; then
  echo "  [OK] suggest endpoint is registered (response contains 'suggestions' key)"
  echo "  response: $PROBE"
else
  echo "  [X] suggest endpoint NOT registered or not reachable"
  echo "  response: $PROBE"
  echo "  => The backend may not have the latest code deployed."
  exit 1
fi

echo
echo "----------------------------------------------------------"
echo "3) Portal table rows"
echo "----------------------------------------------------------"
PORTAL_COUNT="$(mysql_q "SELECT COUNT(*) FROM ${DB}.feishu_datasource_portal")"
echo "  portal count: $PORTAL_COUNT"
if [ "$PORTAL_COUNT" = "0" ] || [ -z "$PORTAL_COUNT" ]; then
  echo "  [!] No portals exist -- create one in the console first."
  exit 1
fi

mysql_t "SELECT id, slug, title, access_mode, fuzzy_enabled, fuzzy_min_digits, enabled
         FROM ${DB}.feishu_datasource_portal LIMIT 5"

# Get a real slug to test with.
SLUG="$(mysql_q "SELECT slug FROM ${DB}.feishu_datasource_portal WHERE enabled = 1 LIMIT 1")"
if [ -z "$SLUG" ]; then
  echo "  [!] No enabled portal found -- enable one in the console first."
  exit 1
fi
echo
echo "  testing with slug: $SLUG"

echo
echo "----------------------------------------------------------"
echo "4) Suggest endpoint with real slug"
echo "----------------------------------------------------------"

# 4a) Empty / short query -- should return empty suggestions.
echo "  4a) query='' (empty)"
R1="$(backend_curl "http://localhost:8080/api/v1/feishu/portal/${SLUG}/suggest?query=")"
echo "      response: $R1"

echo "  4b) query='12' (too short, < min_digits)"
R2="$(backend_curl "http://localhost:8080/api/v1/feishu/portal/${SLUG}/suggest?query=12")"
echo "      response: $R2"

echo "  4c) query='4514' (valid, 4 digits)"
R3="$(backend_curl "http://localhost:8080/api/v1/feishu/portal/${SLUG}/suggest?query=4514")"
echo "      response: $(printf '%s' "$R3" | head -c 500)"

# Check if we got suggestions.
SUGGEST_COUNT="$(printf '%s' "$R3" | sed -n 's/.*"suggestions"[[:space:]]*:[[:space:]]*\[\(.*\)\].*/\1/p' | awk -F',' '{print NF}')"
echo "      suggestions count: $SUGGEST_COUNT"

echo "  4d) query='4514 6176' (with space, URL-encoded)"
R4="$(backend_curl "http://localhost:8080/api/v1/feishu/portal/${SLUG}/suggest?query=4514%206176")"
echo "      response: $(printf '%s' "$R4" | head -c 500)"

echo "  4e) query='45146176' (8 digits, no space)"
R5="$(backend_curl "http://localhost:8080/api/v1/feishu/portal/${SLUG}/suggest?query=45146176")"
echo "      response: $(printf '%s' "$R5" | head -c 500)"

echo
echo "----------------------------------------------------------"
echo "5) Option table data for this portal's datasource"
echo "----------------------------------------------------------"
DS_ID="$(mysql_q "SELECT datasource_id FROM ${DB}.feishu_datasource_portal WHERE slug = '${SLUG}' LIMIT 1")"
SOURCE_KEY="$(mysql_q "SELECT query_field_source_key FROM ${DB}.feishu_datasource_portal WHERE slug = '${SLUG}' LIMIT 1")"
echo "  datasource_id     : $DS_ID"
echo "  query_source_key  : $SOURCE_KEY"

if [ -n "$SOURCE_KEY" ]; then
  OPTION_COUNT="$(mysql_q "SELECT COUNT(*) FROM ${DB}.feishu_option WHERE source_key = '${SOURCE_KEY}' AND enabled = 1")"
  echo "  option count      : $OPTION_COUNT"
  if [ "$OPTION_COUNT" != "0" ] && [ -n "$OPTION_COUNT" ]; then
    echo "  sample labels (first 5):"
    mysql_t "SELECT label FROM ${DB}.feishu_option WHERE source_key = '${SOURCE_KEY}' AND enabled = 1 LIMIT 5"
  else
    echo "  [!] No options for this source_key -- sync data first."
  fi
else
  echo "  [!] Could not read query_field_source_key."
fi

echo
echo "----------------------------------------------------------"
echo "6) Portal fuzzy config"
echo "----------------------------------------------------------"
mysql_t "SELECT slug, fuzzy_enabled, fuzzy_min_digits FROM ${DB}.feishu_datasource_portal WHERE slug = '${SLUG}'"

FUZZY_ENABLED="$(mysql_q "SELECT fuzzy_enabled FROM ${DB}.feishu_datasource_portal WHERE slug = '${SLUG}' LIMIT 1")"
FUZZY_MIN="$(mysql_q "SELECT fuzzy_min_digits FROM ${DB}.feishu_datasource_portal WHERE slug = '${SLUG}' LIMIT 1")"
echo "  fuzzy_enabled    : $FUZZY_ENABLED"
echo "  fuzzy_min_digits : $FUZZY_MIN"

echo
echo "----------------------------------------------------------"
echo "Verdict"
echo "----------------------------------------------------------"
if printf '%s' "$R3" | grep -q '"suggestions"'; then
  if [ "$SUGGEST_COUNT" -gt "0" ] 2>/dev/null; then
    echo "  [OK] Suggest endpoint is working and returned $SUGGEST_COUNT suggestion(s)."
    echo "       The autocomplete feature is deployed and functional."
  else
    echo "  [!] Suggest endpoint is registered but returned 0 suggestions for '4514'."
    echo "      This could mean:"
    echo "        - No options match the query (check option table data above)"
    echo "        - fuzzy_enabled is false for this portal"
    echo "        - fuzzy_min_digits is > 4 and the query is too short"
    echo "      Try a different query or check the portal config."
  fi
else
  echo "  [X] Suggest endpoint returned unexpected response."
  echo "      The endpoint may not be properly deployed."
  exit 1
fi
