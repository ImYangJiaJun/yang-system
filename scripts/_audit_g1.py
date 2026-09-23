#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""G1 独立补漏审计：模块索引 / _urlmap / 磁盘 三方交叉穷举。只读。"""
import json, os, re, sys, collections

R = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                 "docs", "reference", "feishu")
BASE = "https://open.feishu.cn"

_MD_LINK = re.compile(r"\]\((https://open\.feishu\.cn/document/(?:[^\s()]|\([^\s()]*\))+?)\)")
_MD_INDEX = re.compile(r"\]\((https://open\.feishu\.cn/llms-docs/zh-CN/llms-(?:[^\s()]|\([^\s()]*\))*\.txt)\)")
_BARE_DOC = re.compile(r"https://open\.feishu\.cn/document/[^\s)\"'<>`\\]+")
_TRAILING = set(".,;:!?、，。）】」》")


def normalize(raw):
    url = raw.split("#")[0].split("?")[0]
    if url.endswith(".md"):
        url = url[:-3]
    while url and url[-1] in _TRAILING:
        url = url[:-1]
    return url.rstrip("/")


man = json.load(open(os.path.join(R, "_manifest.json"), encoding="utf-8"))
items = man["items"]
rows = [l.split("\t") for l in open(os.path.join(R, "_urlmap.tsv"), encoding="utf-8").read().splitlines()[1:] if l.strip()]
aliases = json.load(open(os.path.join(R, "_aliases.json"), encoding="utf-8"))
unavail = json.load(open(os.path.join(R, "_unavailable.json"), encoding="utf-8"))


def exists(rel):
    return os.path.isfile(os.path.join(R, rel.replace("/", os.sep)))


print("=== A. _urlmap.tsv -> 文件存在性 ===")
bad = [(u, p) for u, p, o in rows if not exists(p)]
print("rows:", len(rows), "| 指向不存在文件:", len(bad))
for t in bad[:25]:
    print("   MISSING:", t)

print("\n=== B. manifest ok/alias 路径 -> 文件存在性 ===")
miss = [(u, e["status"], e["path"]) for u, e in items.items()
        if (e["status"].startswith("ok") or e["status"] == "alias") and e.get("path") and not exists(e["path"])]
print("missing:", len(miss))
for t in miss[:25]:
    print("   ", t)

print("\n=== C. _aliases.json 目标存在性 ===")
bada = [(u, t) for u, t in aliases.items() if not exists(t)]
print("aliases:", len(aliases), "| 悬空目标:", len(bada))
for t in bada[:25]:
    print("   ", t)

print("\n=== D. 磁盘死文件（任何索引都到不了）===")
reach = set()
for u, e in items.items():
    if e.get("path"):
        reach.add(e["path"])
for t in aliases.values():
    reach.add(t)
for u, p, o in rows:
    reach.add(p)
disk = []
for dp, _, fs in os.walk(R):
    for n in fs:
        if n.endswith(".md") and n not in ("00-index.md", "README.md"):
            rel = os.path.relpath(os.path.join(dp, n), R).replace(os.sep, "/")
            if rel.startswith("_index/"):
                continue
            disk.append(rel)
dead = sorted(set(disk) - reach)
print("磁盘 md:", len(disk), "| 死文件:", len(dead))
for t in dead[:50]:
    print("   DEAD:", t)

print("\n=== E. manifest ok/alias 但 path 为空 ===")
print(sum(1 for u, e in items.items() if (e["status"].startswith("ok") or e["status"] == "alias") and not e.get("path")))

print("\n=== F. _unavailable.json 与 manifest 非 ok 集合比对 ===")
unas = {x["url"] for x in unavail}
nonok = {u for u, e in items.items() if e["status"] not in ("ok", "alias")}
print("unavailable:", len(unas), "| manifest 非 ok:", len(nonok))
print("manifest 非 ok 但不在 unavailable:", len(nonok - unas))
for t in sorted(nonok - unas)[:15]:
    print("   ", repr(t))
print("unavailable 但不在 manifest 非 ok:", len(unas - nonok))
for t in sorted(unas - nonok)[:15]:
    print("   ", repr(t))

print("\n=== G. 模块索引条目 是否 100% 落盘或登记 ===")
_REG = frozenset({"http403", "http404", "not-public", "not-found", "not-markdown",
                  "error-body", "unavailable", "discarded", "missing"})
gap = []
total = 0
per_slug_gap = collections.Counter()
for fn in sorted(os.listdir(os.path.join(R, "_index"))):
    if not (fn.startswith("llms-") and fn.endswith(".txt")) or fn == "llms.txt":
        continue
    slug = fn[5:-4]
    text = open(os.path.join(R, "_index", fn), encoding="utf-8").read()
    for raw in _MD_LINK.findall(text):
        total += 1
        url = normalize(raw)
        e = items.get(url)
        if e is None:
            gap.append((slug, url, "NOT-IN-MANIFEST"))
            per_slug_gap[slug] += 1
            continue
        st = e["status"]
        if st.startswith("ok") or st == "alias":
            if not e.get("path") or not exists(e["path"]):
                gap.append((slug, url, "OK-BUT-NO-FILE"))
                per_slug_gap[slug] += 1
            continue
        if st not in _REG:
            gap.append((slug, url, "UNREGISTERED-STATUS:" + st))
            per_slug_gap[slug] += 1
print("模块索引条目:", total, "| 缺口:", len(gap))
print("按模块:", dict(per_slug_gap))
for t in gap[:40]:
    print("   GAP:", t)

print("\n=== H. 索引快照里存在但 manifest 完全没有的 URL（用宽正则）===")
wide = []
for fn in sorted(os.listdir(os.path.join(R, "_index"))):
    if not fn.endswith(".txt"):
        continue
    text = open(os.path.join(R, "_index", fn), encoding="utf-8").read()
    for raw in _BARE_DOC.findall(text):
        u = normalize(raw)
        if u not in items:
            wide.append((fn, u))
print("宽正则命中但 manifest 缺失:", len(wide))
for t in wide[:30]:
    print("   WIDE:", t)

print("\n=== I. 磁盘重复内容（同 sha 多文件）===")
h = collections.defaultdict(list)
for rel in disk:
    with open(os.path.join(R, rel.replace("/", os.sep)), "rb") as fh:
        h[__import__("hashlib").sha256(fh.read()).hexdigest()].append(rel)
dup = {k: v for k, v in h.items() if len(v) > 1}
print("内容重复组:", len(dup), "| 冗余文件:", sum(len(v) - 1 for v in dup.values()))
for k, v in list(dup.items())[:15]:
    print("   DUP:", v)
