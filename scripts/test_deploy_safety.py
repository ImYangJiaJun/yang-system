#!/usr/bin/env python3
"""在临时目录执行真实部署函数；Docker/curl 仅用命令替身，不连接服务。"""
from __future__ import annotations

import os
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile
import unittest

from check_deploy_syntax import ROOT, shell_binary


MUTATIONS = {
    "curl-exit": ("DeploySafety.test_partial_200_is_not_healthy", "2>/dev/null); then\n    printf '000'", "2>/dev/null) && false; then\n    printf '000'"),
    "ready-body": ("DeploySafety.test_transport_and_body_must_both_be_ready", 'r.get("data", {}).get("status") == "ready"', "True"),
    "writer-state": ("DeploySafety.test_reset_rechecks_writers", "  assert_writers_stopped\n", ""),
    "release-config": ("DeploySafety.test_release_rejects_changed_config_and_keeps_image_ids", '[ "${actual%% *}" = "$expected" ] || { err "配置与', 'true || { err "配置与'),
    "snapshot-hash": ("DeploySafety.test_corrupt_snapshot_cannot_replace_config", '[ "${actual%% *}" = "$expected" ] || { err "配置快照校验失败"', 'true || { err "配置快照校验失败"'),
    "immutable-image": ("DeploySafety.test_cutover_uses_smoke_image_ids", '  load_release green || die', '  true || die'),
    "public-binding": ("DeploySafety.test_production_binding_plan_rejects_public_ipv4_ipv6_before_changes", 'a.is_loopback for x in sys.argv[1:]', 'True for x in sys.argv[1:]'),
    "pipeline": ("BackupSafety.test_dump_failure_is_not_published", "set -euo pipefail", "set -eu"),
    "upload-exit": ("BackupSafety.test_upload_failure_is_not_published", '"$BACKUP_SSH_TARGET:$BACKUP_REMOTE_DIR/$name.partial"\n', '"$BACKUP_SSH_TARGET:$BACKUP_REMOTE_DIR/$name.partial" || true\n'),
    "url-redaction": ("DeploySafety.test_invalid_mysql_url_does_not_expose_password", '（不回显凭据）', '（$url）'),
    "smoke-mode": ("DeploySafety.test_smoke_rejects_unsupported_mode_or_failed_process", 'r.get("mode") == "read_only"', 'True'),
    "smoke-exit": ("DeploySafety.test_smoke_process_failure_cannot_publish_success", '"$BACKEND_IMAGE" --smoke-check > "$report"; then', '"$BACKEND_IMAGE" --smoke-check > "$report" && false; then'),
    "smoke-state": ("DeploySafety.test_cutover_requires_successful_smoke_exit_and_exact_args", '[ "$(docker inspect -f \'{{.State.Status}}:{{.State.ExitCode}}\' "$GREEN_BACKEND")" = exited:0 ]', 'true'),
    "smoke-args": ("DeploySafety.test_cutover_requires_successful_smoke_exit_and_exact_args", '[ "$(docker inspect -f \'{{json .Args}}\' "$GREEN_BACKEND")" = \'["--smoke-check"]\' ]', 'true'),
    "shared-probe-deadline": ("DeploySafety.test_health_probes_share_one_deadline", '15 "$deadline"; then', '15; then'),
    "smoke-create-database": ("DeploySafety.test_smoke_prepare_cannot_create_database", '  # 冒烟不得建库；首次部署先运行 infra-up，库缺失由只读检查明确失败。', '  ensure_database'),
}


class DeploySafety(unittest.TestCase):
    script_path = "deploy/deploy-blue-green.sh"

    def run_shell(self, body: str) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory(prefix="yang-deploy-test-") as directory:
            path = Path(directory)
            source = (ROOT / self.script_path).read_text(encoding="utf-8")
            mutation = os.environ.get("YANG_DEPLOY_TEST_MUTATION")
            if mutation:
                _, old, new = MUTATIONS[mutation]
                self.assertIn(old, source, "变异目标漂移，需更新测试")
                source = source.replace(old, new, 1)
            library = path / "deploy.sh"
            library.write_text(source.rsplit('main "$@"', 1)[0], encoding="utf-8")
            script = 'export PATH="/usr/bin:/bin:$PATH"\nsource "$APP_DIR/deploy.sh"\n' + body
            env = {**os.environ, "APP_DIR": path.as_posix(), "MSYS_NO_PATHCONV": "1"}
            return subprocess.run([shell_binary(), "-c", script], env=env, text=True,
                                  encoding="utf-8", capture_output=True, timeout=15)

    def test_partial_200_is_not_healthy(self):
        result = self.run_shell("curl() { printf 200; return 28; }; probe_code http://fixture")
        self.assertEqual(result.stdout.strip(), "000")

    def test_smoke_requires_read_only_executable_contract(self):
        result = self.run_shell('''
docker() {
  case "$*" in
    *'--smoke-check'*)
      [[ "$*" == *'--no-healthcheck'* ]] || return 9
      [[ "$*" != *'--restart unless-stopped'* ]] || return 9
      printf '{"interface":"yang-system-smoke-v1","mode":"read_only","status":"ok","schema_changes":2}' ;;
    *) return 9 ;;
  esac
}
timeout() { shift 2; "$@"; }
run_read_only_smoke
''')
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_smoke_rejects_unsupported_mode_or_failed_process(self):
        for body in ('{"interface":"yang-system-smoke-v1","mode":"serve","status":"ok","schema_changes":0}',
                     '{"interface":"yang-system-smoke-v1","mode":"read_only","status":"error","schema_changes":0}'):
            result = self.run_shell('''
docker() { printf '%s' ''' + "'" + body + "'" + '''; }
timeout() { shift 2; "$@"; }
run_read_only_smoke
printf UNSAFE_SUCCESS
''')
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("UNSAFE_SUCCESS", result.stdout)

    def test_health_probes_share_one_deadline(self):
        result = self.run_shell('''
wait_ready() { printf '%s' "$4" > readiness-deadline; }
wait_http() { [ "$3" = "$(cat readiness-deadline)" ]; }
health_check 8081 9090 fixture
''')
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_smoke_process_failure_cannot_publish_success(self):
        result = self.run_shell('''
docker() { printf '{"interface":"yang-system-smoke-v1","mode":"read_only","status":"ok","schema_changes":0}'; return 1; }
timeout() { shift 2; "$@"; }
run_read_only_smoke
printf UNSAFE_SUCCESS
''')
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("UNSAFE_SUCCESS", result.stdout)

    def test_smoke_prepare_cannot_create_database(self):
        result = self.run_shell('''
need() { :; }
require_config() { :; }
assert_binding_policy() { :; }
ensure_network() { :; }
load_mysql_env() { :; }
preflight_release_config() { :; }
preflight_release_data() { :; }
verify_runtime_config() { :; }
ensure_database() { printf CREATED_DATABASE; }
docker() { :; }
prepare
''')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("CREATED_DATABASE", result.stdout)

    def test_cutover_requires_successful_smoke_exit_and_exact_args(self):
        for state, args, accepted in (("exited:0", '["--smoke-check"]', True),
                                      ("running:0", '["--smoke-check"]', False),
                                      ("exited:1", '["--smoke-check"]', False),
                                      ("exited:0", '[]', False)):
            result = self.run_shell('''
mkdir fixture
printf '{"interface":"yang-system-smoke-v1","mode":"read_only","status":"ok","schema_changes":0}' > fixture/smoke.json
release_directory() { printf fixture; }
wait_http() { :; }
docker() {
  case "$*" in
    *State.Status*) printf '%s' ''' + repr(state) + ''';;
    *json*) printf '%s' ''' + repr(args) + ''';;
    *) return 9;;
  esac
}
verify_smoke_evidence
''')
            self.assertEqual(result.returncode == 0, accepted, result.stderr)

    def test_expired_probe_deadline_sends_no_request(self):
        result = self.run_shell('''
curl() { printf UNEXPECTED_REQUEST >&2; }
sleep() { :; }
if wait_http http://fixture 40 "$SECONDS"; then exit 9; fi
if wait_ready 9090 40 127.0.0.1 "$SECONDS"; then exit 9; fi
''')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("UNEXPECTED_REQUEST", result.stderr)

    def test_ready_rejects_invalid_body(self):
        result = self.run_shell("curl() { printf 200; }; sleep() { :; }; wait_ready 9154 1")
        self.assertNotEqual(result.returncode, 0)

    def test_transport_and_body_must_both_be_ready(self):
        result = self.run_shell('''
sleep() { :; }
curl() { printf '{"data":{"status":"ready"}}'; return 28; }
if wait_ready 9154 1; then exit 9; fi
curl() { printf '{"data":{"status":"not_ready"}}'; }
if wait_ready 9154 1; then exit 9; fi
curl() { printf '{"data":{"status":"ready"}}'; }
wait_ready 9154 1
''')
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_stop_failure_cannot_reach_reset(self):
        result = self.run_shell('''
running() { [ "$1" = "$LIVE_BACKEND" ]; }
exists() { return 0; }
docker() { return 1; }
container_state() { printf running; }
stop_all_pairs
printf 'DESTROYED'
''')
        self.assertNotIn("DESTROYED", result.stdout)
        self.assertNotEqual(result.returncode, 0)

    def test_reset_rechecks_writers(self):
        result = self.run_shell('''
running() { return 0; }
exists() { return 0; }
docker() { printf running; }
mysql_scalar() { printf 1; }
mysql_exec() { printf 'DESTROYED'; }
MYSQL_DATABASE=fixture
reset_database
''')
        self.assertNotIn("DESTROYED", result.stdout)
        self.assertNotEqual(result.returncode, 0)

    def test_invalid_mysql_url_does_not_expose_password(self):
        result = self.run_shell("toml_get() { printf 'oops://admin:fake-secret@fixture/db'; }; load_mysql_env")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("fake-secret", result.stdout + result.stderr)

    def test_placeholder_line_does_not_expose_other_credentials(self):
        result = self.run_shell('''
printf '[mysql]\\nurl = "mysql://user:fake-secret@replace-with-host/db"\\n[authorization]\\n' > "$CONFIG_FILE"
require_config
''')
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("fake-secret", result.stdout + result.stderr)

    def test_mysql_write_error_does_not_expose_driver_echoed_secret(self):
        result = self.run_shell('''
MYSQL_ROOT_PASSWORD=fixture
docker() { printf 'SQL error near fake-secret' >&2; return 1; }
mysql_exec "ALTER USER 'fixture' IDENTIFIED BY 'fake-secret'"
''')
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("fake-secret", result.stdout + result.stderr)

    def test_release_rejects_changed_config_and_keeps_image_ids(self):
        result = self.run_shell('''
printf old > "$CONFIG_FILE"
docker() { case "$*" in *frontend*) printf 'sha256:frontend-tested\\n';; *) printf 'sha256:backend-tested\\n';; esac; }
record_release green "$GREEN_BACKEND" "$GREEN_FRONTEND"
BACKEND_IMAGE=mutable-new-tag
load_release green
[ "$BACKEND_IMAGE" = sha256:backend-tested ]
printf PINNED_IMAGE
printf changed > "$CONFIG_FILE"
assert_release_config green
printf 'ACCEPTED_CHANGED_CONFIG'
''')
        self.assertNotIn("command not found", result.stderr)
        self.assertIn("PINNED_IMAGE", result.stdout)
        self.assertNotIn("ACCEPTED_CHANGED_CONFIG", result.stdout)
        self.assertNotEqual(result.returncode, 0)

    def test_release_config_restore_matches_old_binary(self):
        result = self.run_shell('''
printf old-contract > "$CONFIG_FILE"
docker() { printf 'sha256:old-image\\n'; }
record_release blue "$BLUE_BACKEND" "$BLUE_FRONTEND"
printf new-unknown-key > "$CONFIG_FILE"
restore_release_config blue
cat "$CONFIG_FILE"
''')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.endswith("old-contract"))

    def test_second_publisher_is_rejected(self):
        result = self.run_shell('''
acquire_deploy_lock
( acquire_deploy_lock; printf 'SECOND_PUBLISHER' )
''')
        self.assertNotIn("command not found", result.stderr)
        self.assertNotIn("SECOND_PUBLISHER", result.stdout)
        self.assertNotEqual(result.returncode, 0)

    def test_config_permission_failure_aborts(self):
        result = self.run_shell('''
printf private > config.toml
chmod() { return 1; }
secure_runtime_config config.toml
printf 'IGNORED_PERMISSION_FAILURE'
''')
        self.assertNotIn("command not found", result.stderr)
        self.assertNotIn("IGNORED_PERMISSION_FAILURE", result.stdout)
        self.assertNotEqual(result.returncode, 0)

    def cutover(self, fail: str = ""):
        return self.run_shell('''
printf old > "$CONFIG_FILE"
mkdir containers
printf 'sha256:old-backend' > containers/yang-backend
printf 'sha256:old-frontend' > containers/yang-frontend
printf 'sha256:tested-backend' > containers/yang-backend-green
printf 'sha256:tested-frontend' > containers/yang-frontend-green
exists() { [ -f "containers/$1" ]; }
running() { exists "$1"; }
container_state() { printf exited; }
health_check() { return 0; }
verify_smoke_evidence() { return 0; }
docker() {
  local action="$1" last="${!#}" name="$3"
  case "$action" in
    inspect)
      case "$3" in
        '{{.Image}}') cat "containers/$last"; printf '\\n';;
        '{{.Id}}') printf 'id-%s' "$last";;
      esac;;
    rm) rm -f "containers/$last";;
    rename)
      [ "$2" != "''' + fail + '''" ] || return 1
      mv "containers/$2" "containers/$3";;
    run)
      while [ "$1" != --name ]; do shift; done
      name="$2"
      [ "$name" != "''' + fail + '''" ] || return 1
      printf '%s' "$last" > "containers/$name"
      printf 'START_IMAGE=%s\\n' "$last" >&2;;
    stop) printf 'STOP=%s\\n' "$last" >&2;;
    *) return 77;;
  esac
}
record_release live "$LIVE_BACKEND" "$LIVE_FRONTEND"
record_release green "$GREEN_BACKEND" "$GREEN_FRONTEND"
BACKEND_IMAGE=changed-tag
cmd_cutover
''')

    def test_cutover_uses_smoke_image_ids(self):
        result = self.cutover()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("START_IMAGE=sha256:tested-backend", result.stderr)
        self.assertIn("START_IMAGE=sha256:tested-frontend", result.stderr)
        self.assertNotIn("START_IMAGE=changed-tag", result.stderr)

    def test_cutover_frontend_rename_failure_preserves_recovery_identity(self):
        result = self.cutover("yang-frontend")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("id-yang-backend", result.stderr)
        self.assertNotIn("START_IMAGE=", result.stderr)

    def test_rollback_refuses_unproven_schema_compatibility(self):
        result = self.run_shell('''
docker() { printf 'TOUCHED_CONTAINER'; }
cmd_rollback
''')
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("TOUCHED_CONTAINER", result.stdout)

    def test_corrupt_snapshot_cannot_replace_config(self):
        result = self.run_shell('''
printf old > "$CONFIG_FILE"
docker() { printf 'sha256:old-image\\n'; }
record_release blue "$BLUE_BACKEND" "$BLUE_FRONTEND"
directory=$(release_directory blue)
printf corrupted > "$directory/config.toml"
if restore_release_config blue; then exit 8; fi
[ "$(cat "$CONFIG_FILE")" = old ]
''')
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_runtime_config_is_checked_as_non_root_without_network(self):
        result = self.run_shell('''
printf private > "$CONFIG_FILE"
secure_runtime_config() { :; }
docker() {
  [[ "$*" == *'--network none --user 10001:10001 --entrypoint sh'* ]] || return 9
  return 1
}
verify_runtime_config
printf 'IGNORED_UNREADABLE_CONFIG'
''')
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("IGNORED_UNREADABLE_CONFIG", result.stdout)

    def test_production_binding_plan_rejects_public_ipv4_ipv6_before_changes(self):
        for address in ("0.0.0.0", "::", "203.0.113.10"):
            with self.subTest(address=address):
                result = self.run_shell('''
toml_get() { printf production; }
BIND_ADDR=''' + address + '''
assert_binding_policy
printf 'EXPOSED'
''')
                self.assertNotIn("command not found", result.stderr)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("EXPOSED", result.stdout)

    def test_loopback_and_explicit_test_binding_are_supported(self):
        result = self.run_shell('''
toml_get() { printf production; }
BIND_ADDR=127.0.0.1
assert_binding_policy
toml_get() { printf test; }
BIND_ADDR=0.0.0.0
assert_binding_policy
''')
        self.assertEqual(result.returncode, 0, result.stderr)


class BackupSafety(unittest.TestCase):
    run_shell = DeploySafety.run_shell
    script_path = "deploy/backup-mysql.sh"

    def backup(self, failure: str):
        return self.run_shell('''
MYSQL_PWD=fake-password
MYSQL_CONTAINER=scratch-mysql
MYSQL_DATABASE=scratch_test
BACKUP_USER=backup
BACKUP_DIR="$APP_DIR/backups"
BACKUP_RECIPIENT_CERT="$APP_DIR/recipient.pem"
BACKUP_SSH_TARGET=fixture
BACKUP_REMOTE_DIR=/isolated/backups
printf certificate > "$BACKUP_RECIPIENT_CERT"
export MYSQL_PWD
docker() {
  [ "$*" = 'exec -e MYSQL_PWD scratch-mysql mysqldump --user=backup --single-transaction --routines --triggers --source-data=2 --databases scratch_test' ] || return 70
  printf 'CREATE TABLE fixture(id int);\\n-- Dump completed on fixture\\n'
  return ''' + ("1" if failure == "dump" else "0") + '''
}
openssl() {
  local input output
  while [ "$#" -gt 0 ]; do
    case "$1" in -in) input="$2"; shift;; -out) output="$2"; shift;; esac
    shift
  done
  [ -z "${output:-}" ] || cp "$input" "$output"
}
scp() { return ''' + ("1" if failure == "upload" else "0") + '''; }
ssh() { return ''' + ("1" if failure == "verify" else "0") + '''; }
main
''')

    def test_dump_failure_is_not_published(self):
        result = self.backup("dump")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("backup published", result.stdout)

    def test_upload_failure_is_not_published(self):
        result = self.backup("upload")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("backup published", result.stdout)

    def test_remote_checksum_failure_is_not_published(self):
        result = self.backup("verify")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("backup published", result.stdout)

    def test_success_requires_dump_credentials_and_remote_verification(self):
        result = self.backup("")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("backup published", result.stdout)


class UploadSafety(unittest.TestCase):
    def test_build_identity_rejects_dirty_sources(self):
        path = (ROOT / "deploy/deploy.ps1").as_posix().replace("'", "''")
        script = r'''
$ErrorActionPreference='Stop'
$ast=[System.Management.Automation.Language.Parser]::ParseFile('__PATH__',[ref]$null,[ref]$null)
$function=$ast.Find({param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Get-CleanRevision'},$true)
if (-not $function) { throw 'missing source identity check' }
Invoke-Expression $function.Extent.Text
function git { $global:LASTEXITCODE=0; if ($args -contains 'status') { ' M src/lib.rs' } else { 'abc123' } }
$rejected=$false
try { Get-CleanRevision fixture } catch { $rejected=$true }
if (-not $rejected) { throw 'dirty build accepted' }
function git { $global:LASTEXITCODE=0; if ($args -notcontains 'status') { 'abc123' } }
if ((Get-CleanRevision fixture) -ne 'abc123') { throw 'wrong commit' }
'''.replace("__PATH__", path)
        result = subprocess.run([shutil.which("pwsh") or "powershell", "-NoProfile", "-Command", script],
                                text=True, encoding="utf-8", capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_upload_is_private_before_transfer_and_installed_under_lock(self):
        path = (ROOT / "deploy/deploy.ps1").as_posix().replace("'", "''")
        script = r'''
$ErrorActionPreference='Stop'
$ast=[System.Management.Automation.Language.Parser]::ParseFile('__PATH__',[ref]$null,[ref]$null)
$function=$ast.Find({param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Send-AtomicArtifact'},$true)
if (-not $function) { throw 'missing secure upload' }
Invoke-Expression $function.Extent.Text
$script:events=[System.Collections.Generic.List[string]]::new()
$SshOpts=@(); $SshKey='fixture'; $sshTarget='fixture'; $RemoteDir='/fixture'
function Exec([scriptblock]$block) { & $block; if ($LASTEXITCODE -ne 0) { throw 'command failed' } }
function ssh { $script:events.Add('ssh '+($args -join ' ')); $global:LASTEXITCODE=0 }
function scp { $script:events.Add('scp '+($args -join ' ')); $global:LASTEXITCODE=0 }
Send-AtomicArtifact fixture config.cloud.toml.upload
if ($events[0] -notmatch 'umask 077.*mkdir.*\.upload-') { throw 'upload directory must already be private' }
if ($events[1] -notmatch '^scp .*\.upload-.*/payload') { throw 'transfer must use private staging' }
if ($events[2] -notmatch 'mkdir.*\.deploy-lock.*mv') { throw 'install must be locked and atomic' }
'''.replace("__PATH__", path)
        result = subprocess.run([shutil.which("pwsh") or "powershell", "-NoProfile", "-Command", script],
                                text=True, encoding="utf-8", capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)



if __name__ == "__main__":
    if sys.argv[1:] == ["--mutations"]:
        for name, (test, _, _) in MUTATIONS.items():
            result = subprocess.run([sys.executable, __file__, test],
                                    env={**os.environ, "YANG_DEPLOY_TEST_MUTATION": name, "PYTHONIOENCODING": "utf-8"},
                                    capture_output=True, text=True, encoding="utf-8")
            if result.returncode == 0 or "变异目标漂移" in result.stderr:
                print(f"mutation survived: {name}\n{result.stderr}", file=sys.stderr)
                raise SystemExit(1)
            print(f"mutation killed: {name}")
    else:
        unittest.main()
