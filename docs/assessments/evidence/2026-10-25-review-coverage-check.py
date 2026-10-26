"""验证本轮审计的编号覆盖、相对链接和材料完整性；不代表源码正确性验证。"""
from pathlib import Path
import re

root = Path(__file__).resolve().parents[3]
folder = root / "docs/assessments"
original = (folder / "2026-10-25-network-infrastructure-audit-adversarial-review-and-roadmap.md").read_text(encoding="utf-8")
main = folder / "2026-10-25-all-infrastructure-adversarial-review.md"
reports = [main, *(folder / f"2026-10-25-infrastructure-review-{part}.md" for part in (
    "edge-delivery", "feishu-outbound", "data-identity", "runtime-frontend",
))]
expected_d = set(re.findall(r"^\| (D\d+-\d+) \|", original, re.M))
expected_s = set(re.findall(r"^\*\*(S\d+) ·", original, re.M))
observed_d = set()
for report in reports:
    text = report.read_text(encoding="utf-8")
    observed_d.update(re.findall(r"^\| (D\d+-\d+) \|", text, re.M))
    assert all(line == line.rstrip() for line in text.splitlines()), f"行尾空白: {report.name}"
    for link in re.findall(r"\]\(([^)]+)\)", text):
        if "://" in link or link.startswith("#"):
            continue
        target = report.parent / link.split("#", 1)[0]
        assert target.is_file(), f"无效相对链接: {report.name}: {link}"

actual_s = re.findall(r"^\| (S\d+) \|", main.read_text(encoding="utf-8"), re.M)
assert len(expected_d) == 68 and observed_d == expected_d, (expected_d - observed_d, observed_d - expected_d)
assert len(expected_s) == 36 and set(actual_s) == expected_s and len(actual_s) == 36
for name in ("Cargo.toml", "Cargo.lock", "src/main.rs", "shell-self-check.py", "rust-run-output.txt", "shell-run-output.txt"):
    assert (folder / "evidence-edge-delivery-2026-10-25" / name).is_file(), name
print("PASS: all 68 original D conclusions have domain-level verdict rows")
print("PASS: all 36 original S definitions have exactly one main-ledger row")
print("PASS: all report-relative links resolve and report lines have no trailing whitespace")
print("PASS: reproducible edge source, dependency lock and raw output artifacts exist")
