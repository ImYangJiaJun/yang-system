#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""G1: 用脚本自身的写盘逻辑重算台账，与实际落盘产物逐行比对（不联网、不写盘）。"""
import json, os, re, sys, importlib.util, collections

ROOTDIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
R = os.path.join(ROOTDIR, "docs", "reference", "feishu")
sys.path.insert(0, os.path.join(ROOTDIR, "scripts"))
spec = importlib.util.spec_from_file_location("ffd", os.path.join(ROOTDIR, "scripts", "fetch_feishu_docs.py"))
ffd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ffd)
BASE = ffd.BASE

man = json.load(open(os.path.join(R, "_manifest.json"), encoding="utf-8"))
items = man["items"]
aliases = json.load(open(os.path.join(R, "_aliases.json"), encoding="utf-8"))
actual_um = [l.split("\t") for l in
             open(os.path.join(R, "_urlmap.tsv"), encoding="utf-8").read().splitlines()[1:] if l.strip()]

print("=== M. 脚本逻辑重算 _urlmap.tsv vs 实际文件 ===")
want = sorted((u.replace(BASE + "/", ""), e["path"], e.get("origin", ""))
              for u, e in items.items() if e.get("status") == "ok" and e.get("path"))
print("脚本应写行数:", len(want), "| 实际行数:", len(actual_um))
wset = set(map(tuple, want))
aset = set(map(tuple, actual_um))
extra = sorted(aset - wset)
lack = sorted(wset - aset)
print("实际有但脚本逻辑不产出:", len(extra))
for t in extra[:20]:
    print("   EXTRA:", t)
print("脚本逻辑应有但实际缺:", len(lack))
for t in lack[:20]:
    print("   LACK:", t)

print("\n=== N. 覆盖门禁（脚本函数，纯内存）===")
index_slugs = {}
for fn in os.listdir(os.path.join(R, "_index")):
    if fn.startswith("llms-") and fn.endswith(".txt") and fn != "llms.txt":
        index_slugs[fn[5:-4]] = open(os.path.join(R, "_index", fn), encoding="utf-8").read()
rc = ffd.coverage_gate(items, index_slugs)
print("coverage_gate 返回:", rc)

print("\n=== O. 台账<->磁盘对账（脚本 reconcile 的判定部分，不复用其写盘）===")
disk = set()
for dp, _, fs in os.walk(R):
    for n in fs:
        if n.endswith(".md") and n not in ("00-index.md", "README.md"):
            disk.add(os.path.relpath(os.path.join(dp, n), R).replace(os.sep, "/"))
known = {e["path"] for e in items.values()
         if e.get("path") and (e["status"].startswith("ok") or e["status"] == "alias")}
missing = sorted(known - disk)
orphan = sorted(disk - known)
print("台账有磁盘无:", len(missing), missing[:10])
print("磁盘有台账无(孤儿):", len(orphan), orphan[:10])

print("\n=== P. _unavailable.json vs 脚本逻辑 ===")
want_un = [{"url": u, "status": e["status"]} for u, e in sorted(items.items())
           if e["status"] not in ("ok", "alias")]
actual_un = json.load(open(os.path.join(R, "_unavailable.json"), encoding="utf-8"))
print("应写:", len(want_un), "实际:", len(actual_un))
print("差异:", len(set(map(json.dumps, want_un)) ^ set(map(json.dumps, actual_un))))

print("\n=== Q. 脚本从不产出的状态词 ===")
produced = set()
src = open(os.path.join(ROOTDIR, "scripts", "fetch_feishu_docs.py"), encoding="utf-8").read()
for st in collections.Counter(e["status"] for e in items.values()):
    if ('"%s"' % st) not in src:
        print("   状态 %r 在脚本源码中不存在 -> 无法由当前脚本复现" % st)
