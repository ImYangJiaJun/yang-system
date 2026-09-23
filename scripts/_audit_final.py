#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""G1 最终汇总核验（只读、不联网）。"""
import json, os, re, collections, hashlib, importlib.util

ROOTDIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
R = os.path.join(ROOTDIR, "docs", "reference", "feishu")
BASE = "https://open.feishu.cn"
spec = importlib.util.spec_from_file_location("ffd", os.path.join(ROOTDIR, "scripts", "fetch_feishu_docs.py"))
ffd = importlib.util.module_from_spec(spec); spec.loader.exec_module(ffd)

man = json.load(open(os.path.join(R, "_manifest.json"), encoding="utf-8"))
items = man["items"]
aliases = json.load(open(os.path.join(R, "_aliases.json"), encoding="utf-8"))
unavail = json.load(open(os.path.join(R, "_unavailable.json"), encoding="utf-8"))
um = [l.split("\t") for l in open(os.path.join(R, "_urlmap.tsv"), encoding="utf-8").read().splitlines()[1:] if l.strip()]

def ex(rel): return os.path.isfile(os.path.join(R, rel.replace("/", os.sep)))

out = []
out.append("[1] 模块索引覆盖（脚本自带门禁，内存运行）")
slug = {}
for fn in os.listdir(os.path.join(R, "_index")):
    if fn.startswith("llms-") and fn.endswith(".txt") and fn != "llms.txt":
        slug[fn[5:-4]] = open(os.path.join(R, "_index", fn), encoding="utf-8").read()
out.append("    coverage_gate 返回 rc=%d（0=通过）" % ffd.coverage_gate(items, slug))

out.append("[2] _urlmap.tsv: %d 行, 指向不存在文件 %d" % (len(um), sum(1 for u, p, o in um if not ex(p))))
out.append("[3] manifest ok/alias 路径不存在: %d | alias 悬空: %d" %
           (sum(1 for e in items.values() if e.get("path") and (e["status"].startswith("ok") or e["status"] == "alias") and not ex(e["path"])),
            sum(1 for t in aliases.values() if not ex(t))))
disk = set()
for dp, _, fs in os.walk(R):
    for n in fs:
        if n.endswith(".md") and n not in ("00-index.md", "README.md") and not dp.startswith(os.path.join(R, "_index")):
            disk.add(os.path.relpath(os.path.join(dp, n), R).replace(os.sep, "/"))
reach = {e["path"] for e in items.values() if e.get("path")} | set(aliases.values()) | {p for _, p, _ in um}
out.append("[4] 磁盘 md %d 篇 | 死文件(任何索引都到不了) %d" % (len(disk), len(set(disk) - reach)))
known = {e["path"] for e in items.values() if e.get("path") and (e["status"].startswith("ok") or e["status"] == "alias")}
out.append("[5] 对账: 台账有磁盘无 %d | 磁盘有台账无 %d" % (len(known - disk), len(disk - known)))

# F1
tdoc = {u.replace(BASE + "/document/", ""): e["path"] for u, e in items.items() if e["status"].startswith("ok") and e.get("path")}
akeys = {u for u, _, _ in um}
miss = sorted(set(tdoc) - akeys)
out.append("[F1] 已落盘但 _urlmap 查不到: %d" % len(miss))
for m in miss:
    out.append("      %s -> %s" % (m, tdoc[m]))

# F2
tslash = {u.replace(BASE + "/", "") for u, e in items.items() if e["status"] == "ok" and e.get("path")}
out.append("[F2] 脚本键变换(BASE+'/')命中实际 urlmap 行: %d/%d" % (len(tslash & akeys), len(akeys)))
src = open(os.path.join(ROOTDIR, "scripts", "fetch_feishu_docs.py"), encoding="utf-8").read()
badst = [s for s in collections.Counter(e["status"] for e in items.values()) if ('"%s"' % s) not in src]
out.append("      脚本源码中不存在的状态词: %s" % badst)
want_un = sum(1 for e in items.values() if e["status"] not in ("ok", "alias"))
out.append("      脚本逻辑应写 _unavailable.json %d 条 vs 实际 %d 条" % (want_un, len(unavail)))
want_um = sum(1 for e in items.values() if e["status"] == "ok" and e.get("path"))
out.append("      脚本逻辑应写 _urlmap.tsv %d 行 vs 实际 %d 行" % (want_um, len(um)))

# F3
out.append("[F3] CRLF: _index/*.txt %d/%d 个文件含 CRLF; _urlmap %d; manifest %d" % (
    sum(1 for f in os.listdir(os.path.join(R, "_index")) if b"\r\n" in open(os.path.join(R, "_index", f), "rb").read()),
    len(os.listdir(os.path.join(R, "_index"))),
    open(os.path.join(R, "_urlmap.tsv"), "rb").read().count(b"\r\n"),
    open(os.path.join(R, "_manifest.json"), "rb").read().count(b"\r\n")))

# F4
h = collections.defaultdict(list)
for rel in disk:
    h[hashlib.sha256(open(os.path.join(R, rel.replace("/", os.sep)), "rb").read()).hexdigest()].append(rel)
dup = {k: v for k, v in h.items() if len(v) > 1}
out.append("[F4] 内容重复组 %d, 冗余文件 %d (两条都 status=ok)" % (len(dup), sum(len(v) - 1 for v in dup.values())))

# F5
gp = [e["path"] for e in items.values() if (e.get("path") or "").startswith("general/")]
out.append("[F5] general/ 落盘 %d 篇, 但 general 不在 manifest.modules(module_count=%d)" % (len(gp), man["module_count"]))

# F6
for d in (".tmp-f14-probe", ".tmp-fs"):
    p = os.path.join(R, d)
    if os.path.isdir(p):
        out.append("[F6] 镜像根内残留探针目录 %s: %d 个文件" % (d, sum(len(f) for _, _, f in os.walk(p))))

print("\n".join(out))
