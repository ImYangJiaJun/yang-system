#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""G1 差异下钻：别名少 1、磁盘多 1 的定位。"""
import json, os, collections, hashlib

R = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                 "docs", "reference", "feishu")
man = json.load(open(os.path.join(R, "_manifest.json"), encoding="utf-8"))
items = man["items"]
aliases = json.load(open(os.path.join(R, "_aliases.json"), encoding="utf-8"))
rows = [l.split("\t") for l in open(os.path.join(R, "_urlmap.tsv"), encoding="utf-8").read().splitlines()[1:] if l.strip()]

alias_status = {u: e["path"] for u, e in items.items() if e["status"] == "alias"}
print("manifest alias-status entries:", len(alias_status))
print("_aliases.json keys:", len(aliases))
print("manifest['aliases'] header:", man["aliases"])
print("only in manifest alias-status:", sorted(set(alias_status) - set(aliases)))
print("only in _aliases.json:", sorted(set(aliases) - set(alias_status)))
for u in set(alias_status) & set(aliases):
    if alias_status[u] != aliases[u]:
        print("  TARGET MISMATCH:", repr(u), alias_status[u], "!=", aliases[u])

print()
print("status == 'ok':", sum(1 for e in items.values() if e["status"] == "ok"))
print("status startswith ok:", sum(1 for e in items.values() if e["status"].startswith("ok")))
print("urlmap rows:", len(rows))
urlmap_urls = {u for u, p, o in rows}
print("ok-status urls not in urlmap:", len({u for u, e in items.items() if e["status"] == "ok"} - urlmap_urls))
print("urlmap urls not status ok:", sorted(urlmap_urls - {u for u, e in items.items() if e["status"] == "ok"}))

print()
urlmap_paths = {p for u, p, o in rows}
man_paths = {e["path"] for e in items.values() if e.get("path")}
alias_paths = set(aliases.values())
disk = set()
for dp, _, fs in os.walk(R):
    for n in fs:
        if n.endswith(".md") and n not in ("00-index.md", "README.md"):
            rel = os.path.relpath(os.path.join(dp, n), R).replace(os.sep, "/")
            if rel.startswith("_index/"):
                continue
            disk.add(rel)
print("disk:", len(disk), "urlmap_paths:", len(urlmap_paths), "manifest paths:", len(man_paths))
print("disk - urlmap_paths:", sorted(disk - urlmap_paths))
print("urlmap_paths - disk:", sorted(urlmap_paths - disk))
print("manifest paths - disk:", sorted(man_paths - disk))
print("disk - (manifest paths | alias paths):", sorted(disk - (man_paths | alias_paths)))
print("manifest paths - urlmap_paths:", sorted(man_paths - urlmap_paths))
