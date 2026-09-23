/* 校验生成的单文件原型：文档骨架、自包含性、语法、契约要点、响应式。
   用法：node variants/_src/verify.mjs                                          */

import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const HERE = dirname(fileURLToPath(import.meta.url));
const OUT = join(HERE, "..");

let failed = 0;
function check(label, ok, detail) {
  if (!ok) failed += 1;
  console.log((ok ? "  ok   " : "  FAIL ") + label + (detail ? "  — " + detail : ""));
}

const files = readdirSync(OUT).filter((name) => name.endsWith(".html")).sort();
const PROTOS = files.filter((name) => name !== "index.html");

for (const file of PROTOS) {
  const html = readFileSync(join(OUT, file), "utf8");
  console.log("\n" + file + "  (" + Math.round(html.length / 1024) + " kB)");

  check("doctype + lang=zh-CN", /^<!doctype html>\s*<html lang="zh-CN">/.test(html));
  check("viewport 含 viewport-fit", html.includes("viewport-fit=cover"));
  check("有 <title>", /<title>[^<]+<\/title>/.test(html));

  // 自包含：不得引用除 #锚点 之外的任何外部资源
  const refs = [...html.matchAll(/(?:src|href)\s*=\s*"([^"]*)"/gi)].map((m) => m[1]);
  const outside = refs.filter((ref) => !ref.startsWith("#"));
  check("零外部依赖", outside.length === 0, outside.join(", "));
  check("无网络字体 / @import", !/@import|fonts\.googleapis|cdn\.|https?:\/\//i.test(html));

  // 脚本与样式：各恰好一段内联，且语法可解析
  const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((m) => m[1]);
  check("恰好一段内联 <script>", scripts.length === 1, "found " + scripts.length);
  if (scripts.length === 1) {
    try { new vm.Script(scripts[0]); check("脚本可解析", true); }
    catch (error) { check("脚本可解析", false, error.message); }
  }

  const styles = [...html.matchAll(/<style>([\s\S]*?)<\/style>/g)].map((m) => m[1]);
  check("恰好一段内联 <style>", styles.length === 1, "found " + styles.length);
  for (const css of styles) {
    let depth = 0, ok = true;
    for (const ch of css) {
      if (ch === "{") depth += 1;
      else if (ch === "}") { depth -= 1; if (depth < 0) { ok = false; break; } }
    }
    check("CSS 花括号配平", ok && depth === 0, "depth=" + depth);
  }

  // 可挂载点与原型控制条
  check('含 id="root"', html.includes('id="root"'));
  check("含原型控制条", html.includes("protoctl"));
  check("控制条标注为原型控制", html.includes("原型控制"));

  // 契约要点
  check("删除确认用后端原文", html.includes("删除后其下全部选项会被同时停用，且不可恢复。确认删除？"));
  check("Token 字段是密码型", /data-field="token"[^>]*type="password"|type="password"[^>]*data-field="token"/.test(html));
  check("写入一律走 data-act（无内联 onclick）", !/\son(click|change|input)\s*=/i.test(html));
  check("无 TODO 注释", !/\/\/\s*TODO|<!--\s*TODO/.test(html));
  check("无占位符文案", !/lorem ipsum|xxx+|待填写/i.test(html));

  // 响应式与可访问性
  check("有窄屏断点", /@media\s*\(max-width:\s*900px\)|@media\s*\(max-width:\s*700px\)/.test(html));
  check("尊重 prefers-reduced-motion", html.includes("prefers-reduced-motion"));
  check("明暗跟随系统", html.includes("prefers-color-scheme: dark"));
  check("有 aria 标注", /aria-(label|current|pressed|modal|live)/.test(html));
}

// 索引页：轻量检查即可，它不是原型
{
  const html = readFileSync(join(OUT, "index.html"), "utf8");
  console.log("\nindex.html  (" + Math.round(html.length / 1024) + " kB)");
  check("doctype + lang=zh-CN", /^<!doctype html>\s*<html lang="zh-CN">/.test(html));
  check("只链接这四个原型", PROTOS.every((name) => html.includes('href="' + name + '"')));
  check("无外站链接", !/href\s*=\s*"https?:/i.test(html));
  check("自包含样式", html.includes("<style>") && !/@import/.test(html));
}

console.log("\n" + (failed ? failed + " 项未通过" : "全部通过（" + PROTOS.length + " 个原型 + index）"));
process.exit(failed ? 1 : 0);
