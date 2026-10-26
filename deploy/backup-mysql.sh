#!/usr/bin/env bash
# 逻辑备份 → 公钥加密 → 异机上传并核对密文摘要；成功后才更新 last-success。
set -euo pipefail
umask 077

main() {
  : "${MYSQL_PWD:?需要由受控环境注入 MYSQL_PWD}"
  : "${MYSQL_CONTAINER:?需要指定源容器}"
  : "${MYSQL_DATABASE:?需要指定源库}"
  : "${BACKUP_USER:?需要指定备份账号}"
  : "${BACKUP_DIR:?需要指定本地备份目录}"
  : "${BACKUP_RECIPIENT_CERT:?需要指定恢复方公钥证书}"
  : "${BACKUP_SSH_TARGET:?需要指定异机 SSH 目标}"
  : "${BACKUP_REMOTE_DIR:?需要指定异机备份目录}"
  # 限制远端 shell 拼接字符，不把任意参数当成命令。
  [[ "$BACKUP_REMOTE_DIR" =~ ^/[A-Za-z0-9_./-]+$ ]] || return 1
  [[ "$MYSQL_DATABASE" =~ ^[A-Za-z0-9_]+$ ]] || return 1
  [[ "$BACKUP_SSH_TARGET" =~ ^[A-Za-z0-9_@.-]+$ ]] || return 1
  [ -r "$BACKUP_RECIPIENT_CERT" ] || return 1
  export MYSQL_PWD
  mkdir -p "$BACKUP_DIR"
  chmod 700 "$BACKUP_DIR"
  local temporary name digest
  temporary=$(mktemp -d "$BACKUP_DIR/.pending.XXXXXXXX")
  # 包含明文 SQL 的临时目录始终清理；不使用外部输入拼接删除路径。
  trap 'rm -f "$temporary/dump.sql.gz" "$temporary/archive.cms"; rmdir "$temporary"' EXIT
  name="${MYSQL_DATABASE}_$(date -u +%Y%m%dT%H%M%SZ)_${temporary##*.}.cms"
  docker exec -e MYSQL_PWD "$MYSQL_CONTAINER" mysqldump --user="$BACKUP_USER" \
    --single-transaction --routines --triggers --source-data=2 --databases "$MYSQL_DATABASE" \
    | gzip > "$temporary/dump.sql.gz"
  gzip -t "$temporary/dump.sql.gz"
  # 读完整条流，避免 grep -q 提前关闭管道使 pipefail 假失败。
  gzip -dc "$temporary/dump.sql.gz" | awk '
    /CREATE TABLE/ { schema=1 }
    /-- Dump completed on / { complete=1 }
    END { exit !(schema && complete) }'
  openssl cms -encrypt -binary -aes-256-cbc -in "$temporary/dump.sql.gz" \
    -outform DER -out "$temporary/archive.cms" "$BACKUP_RECIPIENT_CERT"
  openssl cms -cmsout -inform DER -in "$temporary/archive.cms" -noout
  digest=$(sha256sum "$temporary/archive.cms")
  digest=${digest%% *}
  ssh -o BatchMode=yes "$BACKUP_SSH_TARGET" "umask 077; mkdir -p '$BACKUP_REMOTE_DIR'"
  scp -o BatchMode=yes "$temporary/archive.cms" "$BACKUP_SSH_TARGET:$BACKUP_REMOTE_DIR/$name.partial"
  ssh -o BatchMode=yes "$BACKUP_SSH_TARGET" \
    "cd '$BACKUP_REMOTE_DIR' && printf '%s  %s\\n' '$digest' '$name.partial' | sha256sum -c - && mv '$name.partial' '$name'"
  mv "$temporary/archive.cms" "$BACKUP_DIR/$name"
  printf '%s  %s\n' "$digest" "$name" > "$BACKUP_DIR/last-success.tmp"
  mv "$BACKUP_DIR/last-success.tmp" "$BACKUP_DIR/last-success"
  rm -f "$temporary/dump.sql.gz"
  rmdir "$temporary"
  trap - EXIT
  printf 'backup published: %s\n' "$name"
}

main "$@"
