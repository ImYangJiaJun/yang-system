import { readFile } from "node:fs/promises";
import { gzipSync } from "node:zlib";
import { resolve } from "node:path";
import { stdout } from "node:process";

/**
 * Bundle 预算（ADR-5 §2.4）：首屏 JS gzip 硬上限 450 kB（超限退出码 1），
 * 目标值 350 kB（超出打警告）。
 *
 * 首屏口径：入口 chunk + 其传递静态依赖（manifest 的 imports 闭包），
 * 不含 dynamicImports（路由级 lazy chunk，如 workbench/custom view）。
 * 只统计 JS；CSS/图片体积小且由浏览器并行流式处理，不纳入硬上限。
 */

const HARD_LIMIT_KB = 450;
const TARGET_KB = 350;

const manifest = JSON.parse(
  await readFile(resolve("dist/.vite/manifest.json"), "utf8"),
);

const entry = Object.values(manifest).find((chunk) => chunk.isEntry);
if (!entry) throw new Error("manifest 缺少入口 chunk");

const firstScreen = new Set();
const visit = (key) => {
  if (firstScreen.has(key)) return;
  firstScreen.add(key);
  for (const imported of manifest[key].imports ?? []) visit(imported);
};
visit(entry.src ?? "index.html");

let totalGzipBytes = 0;
const breakdown = [];
for (const key of firstScreen) {
  const chunk = manifest[key];
  const content = await readFile(resolve("dist", chunk.file));
  const gzipBytes = gzipSync(content).length;
  totalGzipBytes += gzipBytes;
  breakdown.push(`${chunk.file} ${(gzipBytes / 1024).toFixed(1)} kB`);
}

const totalKb = totalGzipBytes / 1024;
stdout.write(
  `bundle budget: first-screen JS gzip = ${totalKb.toFixed(1)} kB ` +
    `(target ≤ ${TARGET_KB} kB, hard limit ≤ ${HARD_LIMIT_KB} kB)\n${breakdown.map((line) => `  ${line}`).join("\n")}\n`,
);
if (totalKb > HARD_LIMIT_KB) {
  throw new Error(
    `首屏 JS gzip ${totalKb.toFixed(1)} kB 超过硬上限 ${HARD_LIMIT_KB} kB`,
  );
}
if (totalKb > TARGET_KB) {
  stdout.write(
    `bundle budget warning: 超过目标值 ${TARGET_KB} kB，请评估是否继续分包\n`,
  );
}
