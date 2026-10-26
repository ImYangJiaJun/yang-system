"""在隔离 Redis DB 15 复现当前 Lua 的版本回灌和门户窗口续期；不接触已有服务。"""
from pathlib import Path
import re
import subprocess
import time
import uuid

root = Path(__file__).resolve().parents[3]
source = (root / "src/infrastructure/authorization/version_cache.rs").read_text(encoding="utf-8")
script = re.search(r'const PUBLISH_SCRIPT: &str = r#"(.*?)"#;', source, re.S).group(1)

def run(*args):
    result = subprocess.run(args, check=True, capture_output=True, text=True)
    return result.stdout.strip()

print("image=" + run("docker", "image", "inspect", "redis:7-alpine", "--format", "{{.Id}}"))
container = run(
    "docker", "run", "--detach", "--rm", "--network", "none",
    "--memory", "64m", "--name", "yang-audit-replay-" + uuid.uuid4().hex[:12],
    "redis:7-alpine", "redis-server", "--save", "", "--appendonly", "no",
)

try:
    def redis(*args):
        return run("docker", "exec", container, "redis-cli", "--raw", "-n", "15", *args)

    for _ in range(30):
        try:
            if redis("PING") == "PONG":
                break
        except subprocess.CalledProcessError:
            pass
        time.sleep(0.1)
    else:
        raise AssertionError("隔离 Redis 未就绪")

    key = "yang-system:audit-isolated:authz:version:42"
    assert redis("EVAL", script, "1", key, "3", "5") == "1"
    assert redis("EVAL", script, "1", key, "2", "5") == "0"
    assert redis("GET", key) == "3"
    print("PASS: existing version 3 rejects old event 2")
    time.sleep(5.2)
    assert redis("EXISTS", key) == "0"
    assert redis("EVAL", script, "1", key, "2", "5") == "1"
    assert redis("GET", key) == "2"
    ttl = int(redis("TTL", key))
    assert 0 < ttl <= 5
    print(f"REPRODUCED: after expiry, old event 2 is accepted with ttl={ttl}s")
    portal_source = (root / "src/addon/feishu/domain/rate_limit.rs").read_text(encoding="utf-8")
    portal_script = re.search(r'const COUNTER_SCRIPT: &str = r#"(.*?)"#;', portal_source, re.S).group(1)
    portal_key = "portal:query:424242:192.0.2.1"
    assert redis("SET", portal_key, "31", "PX", "300") == "OK"
    assert redis("TTL", portal_key) == "0"
    result = redis("EVAL", portal_script, "1", portal_key, "60").splitlines()
    assert result == ["32", "60"], result
    remaining_ms = int(redis("PTTL", portal_key))
    assert remaining_ms > 59000
    print("REPRODUCED: expiring portal count=31 receives a request, count=32 and window resets to 60s")
    print("Boundary: real Redis + production Lua; no HTTP authorization or MySQL integration executed")
finally:
    run("docker", "rm", "--force", container)
    print("Removed only the disposable container created by this check; no volumes or host ports used")
