#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""G1 独立补漏：拿线上 54 个模块索引与镜像台账逐条对账。只读、不落盘（网络抓取）。"""
import json, os, re, sys, urllib.request, urllib.parse, collections, concurrent.futures

R = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                 "docs", "reference", "feishu")
BASE = "https://open.feishu.cn"
_MD_LINK = re.compile(r"\]\((https://open\.feishu\.cn/document/(?:[^\s()]|\([^\s()]*\))+?)\)")
_MD_INDEX = re.compile(r"\]\((https://open\.feishu\.cn/llms-docs/zh-CN/llms-(?:[^\s()]|\([^\s()]*\))*\.txt)\)")
_TRAIL = set(".,;:!?、，。）】」》")


def normalize(raw):
    url = raw.split("#")[0].split("?")[0]
    if url.endswith(".md"):
        url = url[:-3]
    while url and url[-1] in _TRAIL:
        url = url[:-1]
    return url.rstrip("/")


def read(u, tries=3):
    for i in range(tries):
        try:
            req = urllib.request.Request(urllib.parse.quote(u, safe=":/?#[]@!$&'*+,;=%~-._"),
                                         headers={"User-Agent": "Mozilla/5.0 (audit)"})
            return urllib.request.urlopen(req, timeout=75).read()
        except Exception as e:
            if i == tries - 1:
                return b"__ERR__%s" % type(e).__name__.encode()
            import time
            time.sleep(0.5 * (i + 1))


top = read(BASE + "/llms.txt").decode("utf-8", "replace")
live_idx = sorted(set(_MD_INDEX.findall(top)))
local_idx = sorted(os.listdir(os.path.join(R, "_index")))
local_names = {f for f in local_idx if f.startswith("llms-") and f.endswith(".txt")}
live_names = {u.split("/llms-")[-1].join(["_index/", ""]) for u in live_idx}
# slug -> filename
def fn(u):
    return "_index/llms-" + u.split("llms-docs/zh-CN/llms-")[-1]


print("=== J. 模块索引集合：线上 vs 本地快照 ===")
print("live indices:", len(live_idx))
ln = {fn(u) for u in live_idx}
print("live 有本地无:", sorted(ln - local_names))
print("本地有线上无:", sorted(local_names - ln))

man = json.load(open(os.path.join(R, "_manifest.json"), encoding="utf-8"))
items = man["items"]
unavail = {x["url"] for x in json.load(open(os.path.join(R, "_unavailable.json"), encoding="utf-8"))}

print("\n=== K. 线上模块索引条目 -> 镜像落盘/登记 对账 ===")


def one(u):
    body = read(u)
    if body.startswith(b"__ERR__"):
        return u, None
    return u, body.decode("utf-8", "replace")


gaps = []
per = collections.Counter()
tot = 0
drift = []
with concurrent.futures.ThreadPoolExecutor(8) as pool:
    for u, text in pool.map(one, live_idx):
        if text is None:
            drift.append(("FETCH-FAIL", u))
            continue
        slug = u.split("llms-")[-1][:-4]
        for raw in _MD_LINK.findall(text):
            tot += 1
            url = normalize(raw)
            e = items.get(url)
            if e is None:
                gaps.append((slug, url, "NOT-IN-MANIFEST", url in unavail))
                per[slug] += 1
                continue
            st = e["status"]
            if st.startswith("ok") or st == "alias":
                p = e.get("path")
                if not p or not os.path.isfile(os.path.join(R, p.replace("/", os.sep))):
                    gaps.append((slug, url, "OK-BUT-NO-FILE", False))
                    per[slug] += 1
            elif st not in ("http403", "http404", "not-public", "not-found", "not-markdown",
                            "error-body", "unavailable", "discarded", "missing"):
                gaps.append((slug, url, "UNREG:" + st, False))
                per[slug] += 1
print("线上模块索引条目:", tot, "| 缺口:", len(gaps))
print("按模块:", dict(per))
for g in gaps[:60]:
    print("   GAP:", g)

print("\n=== L. 本地快照是否与线上一致（索引文本）===")
for u in live_idx:
    name = fn(u)
    lp = os.path.join(R, name)
    if not os.path.isfile(lp):
        continue
    body = read(u)
    if body.startswith(b"__ERR__"):
        continue
    local = open(lp, "rb").read()
    if local != body:
        drift.append(("INDEX-DRIFT", name))
print("索引文本漂移:", [d for d in drift if d[0] == "INDEX-DRIFT"])
print("抓取失败:", [d for d in drift if d[0] == "FETCH-FAIL"])
