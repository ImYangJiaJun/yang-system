/* 从已生成的单文件原型里把底座与各方案源码拆回来，放进 variants/_src/。
   这样底座改了还能重跑 gen.mjs，而不是只剩一堆不可再生的 HTML。
   用法：node variants/_src/extract.mjs                                        */

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const OUT = join(HERE, "..");

// 底座与方案之间的分界：方案样式/脚本都以这个注释起手
const CSS_SEAM = /\n\n(?=\/\* 方案 )/;
const JS_SEAM = /\n\n(?=\/\* ={5,}\n   飞书数据源控制台 · 原型共享运行时)/;

const IDS = ["a", "b", "c", "d"];
let baseCss = null;
let appJs = null;

for (const id of IDS) {
  const html = readFileSync(join(OUT, id.toUpperCase() + "-" + suffix(id) + ".html"), "utf8");

  const style = html.slice(html.indexOf("<style>") + 7, html.indexOf("</style>")).trim();
  const script = html.slice(html.indexOf("<script>") + 8, html.lastIndexOf("</script>")).trim();
  const body = script.replace(/^'use strict';\n/, "");

  const cssParts = style.split(CSS_SEAM);
  if (cssParts.length !== 2) throw new Error(id.toUpperCase() + "：样式分界不唯一，找到 " + cssParts.length + " 段");
  const jsParts = body.split(JS_SEAM);
  if (jsParts.length !== 2) throw new Error(id.toUpperCase() + "：脚本分界不唯一，找到 " + jsParts.length + " 段");

  if (baseCss === null) baseCss = cssParts[0].trim() + "\n";
  else if (baseCss.trim() !== cssParts[0].trim()) throw new Error("底座样式在各方案间不一致");
  if (appJs === null) appJs = jsParts[1].trim() + "\n";
  else if (appJs.trim() !== jsParts[1].trim()) throw new Error("共享运行时在各方案间不一致");

  writeFileSync(join(HERE, "proto-" + id + ".css"), cssParts[1].trim() + "\n", "utf8");
  writeFileSync(join(HERE, "proto-" + id + ".js"), jsParts[0].trim() + "\n", "utf8");
}

writeFileSync(join(HERE, "base.css"), baseCss, "utf8");
writeFileSync(join(HERE, "app.js"), appJs, "utf8");

function suffix(id) {
  return { a: "card-gallery", b: "compact-ledger", c: "master-detail", d: "wizard-first" }[id];
}

console.log("已拆出: base.css / app.js / proto-{a,b,c,d}.{css,js}");
