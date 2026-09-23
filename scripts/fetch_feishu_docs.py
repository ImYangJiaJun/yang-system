#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""拉取飞书开放平台「服务端 API」文档到 docs/reference/feishu/（离线参考镜像）。

飞书每个文档页的 HTML 里都带一条给 AI 用的声明：
    <link rel="alternate" type="text/markdown" href="<文档URL>.md"
          tip="pure markdown version, better for ai" />
即 `<文档URL>.md` 就是该文档的纯 markdown 版本。本脚本按这条规则抓取。

枚举分两步，缺一不可：
  1. llms.txt -> 全部模块索引 llms-*.txt -> 各自的 /document/ 链接
  2. 从已抓正文再挖交叉引用（正文大量引用官方历史 URL），迭代到不动点
第 2 步不可省：官方模块索引并不穷尽，不少接口文档只能从正文交叉引用到达。

三个已知坑（都踩过，别再踩）：
  - 模块索引与正文里的链接形态不止一种：官方有几条 URL **自身含括号**
    （如 step-2:-call-jsapi(optional)），朴素正则会在括号处截断，抓出不存在的路径；
    正文里又有大量**裸 URL 紧挨中文正文**（如 `...get_by_param)接口获取`），
    正则若允许 ')' 就会把右括号和后面的中文一起吞进来。故两种形态分开匹配。
  - 目录级文档（如 /document/client-docs/h5）要用 `<url>/.md` 才拿得到。
  - 少数端点返回 `{"code":-1,"msg":"napi error"}` 这类错误 JSON，长度也过关，
    必须显式拒绝，否则会被当正文落盘。

用法：
    python scripts/fetch_feishu_docs.py                # 增量刷新（默认）
    python scripts/fetch_feishu_docs.py --verify-only  # 只做线上逐字节复核，不写文件
    python scripts/fetch_feishu_docs.py --full         # 全量重抓
退出码非 0 表示覆盖门禁或对账未通过。
"""

from __future__ import annotations

import argparse
import collections
import concurrent.futures
import hashlib
import json
import os
import re
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

BASE = "https://open.feishu.cn"
ROOT = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "docs", "reference", "feishu"
)
HEADERS = {"User-Agent": "Mozilla/5.0 (compatible; yang-system-docs-mirror/2.0)"}
WORKERS = 12
RETRIES = 4

# ---- 两种链接形态 ----
# markdown 链接：平衡括号，兼顾官方几条 URL 自身含括号的情况
_MD_LINK = re.compile(r"\]\((https://open\.feishu\.cn/document/(?:[^\s()]|\([^\s()]*\))+?)\)")
# 模块索引链接（文件名含括号，同样要平衡括号）
_MD_INDEX = re.compile(
    r"\]\((https://open\.feishu\.cn/llms-docs/zh-CN/llms-(?:[^\s()]|\([^\s()]*\))*\.txt)\)")
# 裸 URL：必须排除 ')'，否则会吞掉右括号及其后的中文正文
_BARE_DOC = re.compile(r"https://open\.feishu\.cn/document/[^\s)" + "'" + r'"<>`\\]+')
_TRAILING = set(".,;:!?、，。）】」》")

_MARKDOWN_MIN_BYTES = 20
_BOM = b"\xef\xbb\xbf"


def looks_like_error_body(body: bytes) -> bool:
    """响应体是不是服务端的错误 JSON 而不是文档正文。

    只匹配 '{"code":' 这一种序列化形式是不够的——实测同一错误体有多个变体：
    键序颠倒（{"msg":…,"code":…}）、带 BOM、被包成数组（[{…}]）、多一层包装。
    所以这里做结构化判定：去掉 BOM/空白后若能解析成 JSON 且（自身或任意嵌套层）
    含有 code/msg 这类错误字段，就判为错误体。
    """
    stripped = body.lstrip(_BOM + b" \t\r\n")
    if not stripped[:1] in (b"{", b"["):
        return False
    try:
        parsed = json.loads(stripped.decode("utf-8"))
    except (ValueError, UnicodeDecodeError):
        # 不是合法 JSON，但以 { 或 [ 开头且带 code/msg 字样——保守起见仍判为错误体
        return bool(re.search(rb'"(code|msg)"\s*:', stripped[:200]))
    stack = [parsed]
    while stack:
        node = stack.pop()
        if isinstance(node, dict):
            if "code" in node and ("msg" in node or "message" in node):
                return True
            stack.extend(node.values())
        elif isinstance(node, list):
            stack.extend(node)
    return False


def encode_url(url: str) -> str:
    """URL 里可能有非 ASCII（如中文路径段），发请求前必须 percent-encode。"""
    return urllib.parse.quote(url, safe=":/?#[]@!$&'*+,;=%~-._")


def normalize(raw: str) -> str:
    url = raw.split("#")[0].split("?")[0]
    if url.endswith(".md"):
        url = url[:-3]
    while url and url[-1] in _TRAILING:
        url = url[:-1]
    return url.rstrip("/")


def sanitize(segment: str) -> str:
    """Windows 文件名非法字符统一换成 _（'|' 等；':' 会产生 NTFS 备用数据流）。"""
    return re.sub(r'[<>:"\\|?*\x00-\x1f]', "_", segment).strip(". ") or "_"


def _read(url: str, tries: int = RETRIES) -> tuple[str, bytes]:
    for attempt in range(tries):
        try:
            req = urllib.request.Request(encode_url(url), headers=HEADERS)
            return ("ok", urllib.request.urlopen(req, timeout=75).read())
        except urllib.error.HTTPError as exc:
            if exc.code in (403, 404, 410):
                return ("http%d" % exc.code, b"")
        except Exception:  # noqa: BLE001 - 网络异常种类多，统一重试
            pass
        time.sleep(0.6 * (attempt + 1))
    return ("unknown", b"")


def fetch_markdown(url: str) -> tuple[str, bytes]:
    """取一篇文档的正文；返回 (状态, 内容)。状态见 _STATUS_NOTES。"""
    seen = []
    # 目录级文档要用 <url>/.md
    for candidate in (url + ".md", url.rstrip("/") + "/.md"):
        status, body = _read(candidate)
        if status != "ok":
            seen.append(status)
            continue
        head = body[:220].decode("utf-8", "replace")
        if "This document is not public" in head:
            seen.append("not-public")
            continue
        if "This document is not found" in head:
            seen.append("not-found")
            continue
        if len(body) < _MARKDOWN_MIN_BYTES or body[:300].lstrip()[:1] == b"<":
            seen.append("not-markdown")
            continue
        if looks_like_error_body(body):
            seen.append("error-body")
            continue
        return ("ok", body)
    # 403（服务端「非公开」）与 404（不存在）语义不同，优先保留更有信息量的那个
    for preferred in ("not-public", "http403", "not-found", "http404"):
        if preferred in seen:
            return (preferred, b"")
    return (seen[0] if seen else "unknown", b"")


# ---------------------------------------------------------------- 枚举


def enumerate_sources() -> tuple[dict[str, set[str]], dict[str, str], str]:
    """llms.txt -> 模块索引 -> 文档 URL。返回 (url -> 模块集合, slug -> 索引原文, llms.txt 原文)。"""
    status, raw_top = _read(BASE + "/llms.txt")
    if status != "ok":
        sys.exit("无法获取 " + BASE + "/llms.txt")
    top_text = raw_top.decode("utf-8", "replace")

    index_urls = sorted(set(_MD_INDEX.findall(top_text)))
    print(f"[枚举] 模块索引 {len(index_urls)} 个")

    docs: dict[str, set[str]] = {}
    raw: dict[str, str] = {}

    def one(index_url: str):
        st, body = _read(index_url)
        return index_url.split("llms-")[-1][:-4], (body.decode("utf-8", "replace") if st == "ok" else "")

    with concurrent.futures.ThreadPoolExecutor(WORKERS) as pool:
        for slug, text in pool.map(one, index_urls):
            raw[slug] = text
            for url in _MD_LINK.findall(text):
                docs.setdefault(normalize(url), set()).add(slug)
    for url in _MD_LINK.findall(top_text):
        docs.setdefault(normalize(url), set()).add("_top")

    print(f"[枚举] 模块索引共列出 {len(docs)} 篇")
    return docs, raw, top_text


def module_of(url: str, modules: set[str]) -> str:
    return next((m for m in sorted(modules) if m != "_top"), "general")


def canonical_path(url: str, module: str) -> str:
    """本地路径 = 模块名 / 官方 URL 去掉 /document/ 前缀。不做任何裁剪，保证可反推。"""
    parts = [sanitize(p) for p in url.split("/document/", 1)[1].split("/") if p]
    return module + "/" + "/".join(parts) + ".md"


def legacy_path(url: str) -> str:
    """交叉引用补入的文档统一收在 _legacy/，路径保留其原始 URL 形态。"""
    parts = [sanitize(p) for p in url.split("/document/", 1)[1].split("/") if p]
    return "_legacy/" + "/".join(parts) + ".md"


# ---------------------------------------------------------------- 抓取


def download(targets: dict[str, str], verb: str) -> dict[str, dict]:
    total = len(targets)
    results: dict[str, dict] = {}
    lock = threading.Lock()
    done = [0]
    started = time.time()

    def work(item):
        url, rel = item
        full = os.path.join(ROOT, rel.replace("/", os.sep))
        status, body = fetch_markdown(url)
        if status == "ok":
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "wb") as fh:
                fh.write(body)
        rec = {
            "url": url,
            "path": rel if status == "ok" else None,
            "status": status,
            "bytes": len(body) if status == "ok" else 0,
            "sha256": hashlib.sha256(body).hexdigest() if status == "ok" else None,
        }
        with lock:
            results[url] = rec
            done[0] += 1
            if done[0] % 500 == 0:
                print(f"  ..{verb} {done[0]}/{total}", flush=True)
        return rec

    with concurrent.futures.ThreadPoolExecutor(WORKERS) as pool:
        list(pool.map(work, sorted(targets.items())))

    counts = collections.Counter(r["status"] for r in results.values())
    print(f"[{verb}] {dict(counts)} {time.time() - started:.0f}s")
    return results


def scan_crosslinks(known: set[str]) -> list[str]:
    fresh: collections.Counter[str] = collections.Counter()
    for dirpath, _, files in os.walk(ROOT):
        for name in files:
            if not name.endswith(".md"):
                continue
            text = open(os.path.join(dirpath, name), encoding="utf-8", errors="replace").read()
            for raw in _MD_LINK.findall(text) + _BARE_DOC.findall(text):
                url = normalize(raw)
                if url not in known:
                    fresh[url] += 1
    return sorted(fresh)


def mine_to_fixed_point(items: dict[str, dict], aliases: dict[str, str], max_rounds: int = 10):
    """迭代挖掘正文交叉引用，直到不再出现新 URL。写回 items / aliases。"""
    known = set(items)
    by_hash = {r["sha256"]: r["path"] for r in items.values()
               if r.get("sha256") and r.get("path")}
    for round_no in range(1, max_rounds + 1):
        pending = [u for u in scan_crosslinks(known) if u not in known]
        if not pending:
            print(f"[交叉引用] 第 {round_no} 轮无新增，收敛")
            return
        print(f"[交叉引用] 第 {round_no} 轮: {len(pending)} 条候选")
        fresh = download({u: legacy_path(u) for u in pending}, verb="补抓")
        for url, rec in fresh.items():
            known.add(url)
            if rec["status"] != "ok":
                items[url] = rec
                continue
            same = by_hash.get(rec["sha256"])
            if same:
                # 与既有文档字节相同 -> 只是同一篇的另一个 URL，不重复落盘
                aliases[url] = same
                try:
                    os.remove(os.path.join(ROOT, rec["path"].replace("/", os.sep)))
                except OSError:
                    pass
                rec["status"] = "alias"
                rec["path"] = same
            else:
                by_hash[rec["sha256"]] = rec["path"]
            items[url] = rec
    print(f"[交叉引用] 已达 {max_rounds} 轮上限，未继续挖掘")


# ---------------------------------------------------------------- 门禁


# 已落盘（ok / alias）之外的合法归宿：都是「已登记、可解释」的失败态。
# 只要不在这个集合里，就说明条目被静默吞掉了。
_REGISTERED_FAILURES = frozenset({
    "http403", "http404", "not-public", "not-found", "not-markdown",
    "error-body", "unavailable", "discarded", "missing",
})


def coverage_gate(items: dict[str, dict], index_slugs: dict[str, str]) -> int:
    """硬门禁：模块索引列出的每一条，要么已落盘，要么明确登记为不可用。不允许静默丢失。"""
    missing: list[tuple[str, str]] = []
    total = 0
    for slug, text in sorted(index_slugs.items()):
        if not text:
            continue
        for raw in _MD_LINK.findall(text):
            total += 1
            url = normalize(raw)
            entry = items.get(url)
            if entry is None:
                missing.append((slug, url))
                continue
            status = entry["status"]
            # 'ok'、'ok(via-alt)' 等一律算已落盘；其余必须是登记过的失败态
            if not status.startswith("ok") and status != "alias" and status not in _REGISTERED_FAILURES:
                missing.append((slug, url))
    if missing:
        print(f"[门禁] 失败：{len(missing)}/{total} 条模块索引条目既未落盘也未登记为不可用")
        for slug, url in missing[:30]:
            print(f"    [{slug}] {url}")
        return 1
    print(f"[门禁] 通过：{total} 条模块索引条目 100% 已落盘或已登记")
    return 0


def reconcile(items: dict[str, dict]) -> int:
    """台账 <-> 磁盘对账：补抓缺失文件，报告孤儿文件。"""
    disk = set()
    for dirpath, _, files in os.walk(ROOT):
        for name in files:
            if name.endswith(".md") and name not in ("00-index.md", "README.md"):
                disk.add(os.path.relpath(os.path.join(dirpath, name), ROOT).replace(os.sep, "/"))
    known = {e["path"] for e in items.values()
             if e.get("path") and (e["status"].startswith("ok") or e["status"] == "alias")}
    missing = sorted(known - disk)
    orphan = sorted(disk - known)
    if missing:
        print(f"[对账] 台账有而磁盘无 {len(missing)} 条，尝试补抓")
        repaired = download({u: e["path"] for u, e in items.items()
                             if e.get("path") in set(missing) and e["status"] == "ok"}, verb="补抓")
        items.update(repaired)
    if orphan:
        print(f"[对账] 磁盘有而台账无 {len(orphan)} 条（孤儿）：")
        for path in orphan[:20]:
            print("    ", path)
    if not missing and not orphan:
        print(f"[对账] 通过：{len(disk)} 篇，台账与磁盘一致")
    return 1 if orphan else 0


# ---------------------------------------------------------------- 主流程


def verify_only() -> int:
    manifest_path = os.path.join(ROOT, "_manifest.json")
    if not os.path.exists(manifest_path):
        sys.exit("缺少 _manifest.json，请先执行一次完整抓取")
    items = json.load(open(manifest_path, encoding="utf-8"))["items"]
    # 'ok' 与 'ok(via-alt)' 都要复核
    ok = {u: e for u, e in items.items() if e.get("status", "").startswith("ok") and e.get("path")}
    print(f"[复核] {len(ok)} 篇")
    drift: list[tuple[str, str]] = []
    lock = threading.Lock()

    def work(item):
        url, entry = item
        with open(os.path.join(ROOT, entry["path"].replace("/", os.sep)), "rb") as fh:
            local = fh.read()
        # ok(via-alt) 的规范 URL 本来就 404，要按当初实际取到的 source URL 复核
        probe = entry.get("source") or url
        status, live = fetch_markdown(probe)
        if status != "ok" or live != local:
            with lock:
                drift.append((entry["path"], status))
        return None

    with concurrent.futures.ThreadPoolExecutor(WORKERS) as pool:
        list(pool.map(work, sorted(ok.items())))

    if drift:
        print(f"[复核] {len(drift)} 篇不一致：")
        for path, status in drift[:50]:
            print(f"   [{status}] {path}")
        return 1
    print(f"[复核] 全部一致（{len(ok)}/{len(ok)}）")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--full", action="store_true", help="全量重抓")
    parser.add_argument("--verify-only", action="store_true", help="只做线上逐字节复核")
    args = parser.parse_args()

    if args.verify_only:
        return verify_only()

    os.makedirs(ROOT, exist_ok=True)
    docs, index_slugs, top_text = enumerate_sources()
    paths = {u: canonical_path(u, module_of(u, mods)) for u, mods in docs.items()}

    targets = paths
    if not args.full:
        targets = {u: p for u, p in paths.items()
                   if not os.path.exists(os.path.join(ROOT, p.replace("/", os.sep)))}
        print(f"[增量] 待抓 {len(targets)} / 共 {len(paths)}")

    items: dict[str, dict] = {}
    if targets:
        items = download(targets, verb="抓取")
    for url, rec in items.items():
        rec["origin"] = "module-index"
    for url, path in paths.items():
        items.setdefault(url, {"url": url, "path": path if os.path.exists(
            os.path.join(ROOT, path.replace("/", os.sep))) else None,
            "status": "ok" if os.path.exists(os.path.join(ROOT, path.replace("/", os.sep)))
            else "missing", "bytes": 0, "sha256": None, "origin": "module-index"})

    # 校验已存在文件的哈希，保证增量模式下 also 可去重
    for rec in items.values():
        if rec["status"] == "ok" and rec.get("path") and not rec.get("sha256"):
            with open(os.path.join(ROOT, rec["path"].replace("/", os.sep)), "rb") as fh:
                body = fh.read()
            rec["bytes"] = len(body)
            rec["sha256"] = hashlib.sha256(body).hexdigest()

    aliases: dict[str, str] = {}
    mine_to_fixed_point(items, aliases)

    # 落盘产物
    with open(os.path.join(ROOT, "_aliases.json"), "w", encoding="utf-8") as fh:
        json.dump(aliases, fh, ensure_ascii=False, indent=1)

    # url 列写**完整 URL**（与 _manifest.json 的键一致），便于与其他台账直接 join
    rows = sorted((u, e["path"], e.get("origin", ""))
                  for u, e in items.items()
                  if e.get("status", "").startswith("ok") and e.get("path"))
    with open(os.path.join(ROOT, "_urlmap.tsv"), "w", encoding="utf-8", newline="\n") as fh:
        fh.write("url\tlocal_path\torigin\n")
        for row in rows:
            fh.write("\t".join(row) + "\n")
    print(f"[urlmap] {len(rows)} 条")

    unavailable = [{"url": u, "status": e["status"]} for u, e in sorted(items.items())
                   if e["status"] not in ("ok", "alias")]
    with open(os.path.join(ROOT, "_unavailable.json"), "w", encoding="utf-8") as fh:
        json.dump(unavailable, fh, ensure_ascii=False, indent=1)

    disk_files, total_bytes = [], 0
    for dirpath, _, files in os.walk(ROOT):
        for name in files:
            if name.endswith(".md") and name not in ("00-index.md", "README.md"):
                path = os.path.join(dirpath, name)
                disk_files.append(os.path.relpath(path, ROOT).replace(os.sep, "/"))
                total_bytes += os.path.getsize(path)

    with open(os.path.join(ROOT, "_manifest.json"), "w", encoding="utf-8") as fh:
        json.dump({
            "generated": time.strftime("%Y-%m-%d"),
            "source_index": BASE + "/llms.txt",
            "mirror_root": "docs/reference/feishu",
            "entries": len(items),
            "files": len(disk_files),
            "total_bytes": total_bytes,
            "unavailable": len(unavailable),
            "aliases": len(aliases),
            "module_count": len(index_slugs),
            "path_rule": "<模块名>/<官方文档 URL 去掉 https://open.feishu.cn/document/ 前缀>",
            "items": items,
        }, fh, ensure_ascii=False, indent=1)

    index_dir = os.path.join(ROOT, "_index")
    os.makedirs(index_dir, exist_ok=True)
    with open(os.path.join(index_dir, "llms.txt"), "w", encoding="utf-8") as fh:
        fh.write(top_text)
    for slug, text in index_slugs.items():
        with open(os.path.join(index_dir, f"llms-{slug}.txt"), "w", encoding="utf-8") as fh:
            fh.write(text)

    print(f"[完成] 落盘 {len(disk_files)} 篇 / {total_bytes / 1048576:.1f} MB ｜ "
          f"别名 {len(aliases)} ｜ 不可用 {len(unavailable)}")
    rc = coverage_gate(items, index_slugs)
    rc |= reconcile(items)
    print("        提示：`00-index.md` 由文档目录反查生成，刷新后请同步更新")
    return rc


if __name__ == "__main__":
    raise SystemExit(main())
