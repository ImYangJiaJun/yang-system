"""只提取函数并注入替身；不执行部署脚本 main、不访问 Docker/SSH/数据库。"""
import pathlib
import shutil
import subprocess

root = pathlib.Path(__file__).resolve().parents[3]
source = (root / "deploy/deploy-blue-green.sh").read_text(encoding="utf-8")
git_bash = pathlib.Path(r"C:\Program Files\Git\bin\bash.exe")
bash = str(git_bash) if git_bash.exists() else shutil.which("bash")

def function(name):
    start = source.index(f"{name}() {{")
    end = source.index("\n}", start) + 2
    return source[start:end]

def run(script):
    result = subprocess.run([bash, "-c", script], text=True, capture_output=True, check=True)
    print(result.stdout.strip())

run(function("probe_code") + r'''
curl() { printf 200; return 28; }
result=$(probe_code http://fixture.invalid)
test "$result" = 200
printf 'probe_code with curl exit=28 and status=200 returns=%s\n' "$result"
''')

run(function("stop_all_pairs") + r'''
LIVE_FRONTEND=frontend; LIVE_BACKEND=backend; GREEN_FRONTEND=green-frontend; GREEN_BACKEND=green-backend
running() { case "$1" in backend|frontend) return 0;; *) return 1;; esac; }
stop_pair() { :; }
container_state() { printf running; }
info() { :; }; warn() { :; }
stop_all_pairs
printf 'stop_all_pairs with live backend still running returns=%s (caller can continue reset)\n' "$?"
''')

run(r'''
set +o pipefail
false | gzip | gzip -t
printf 'failed dump pipeline without pipefail returns=%s\n' "$?"
''')
