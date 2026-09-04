import { readdir, readFile } from "node:fs/promises";
import { extname, relative, resolve } from "node:path";
import { stdout } from "node:process";

/// 生产构建校验（旧 verify-production-build.mjs 的 Vite 产物适配版：dist/ 布局）。

const buildRoot = resolve("dist");
const forbiddenWorkbenchMarkers = ["发起真实调用", "接口演示"];

async function filesUnder(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = await Promise.all(
    entries.map((entry) => {
      const path = resolve(directory, entry.name);
      return entry.isDirectory() ? filesUnder(path) : [path];
    }),
  );
  return files.flat();
}

const files = await filesUnder(buildRoot);
const indexHtml = await readFile(resolve(buildRoot, "index.html"), "utf8");
const cspMatch = indexHtml.match(
  /<meta\b[^>]*http-equiv=(?:["']Content-Security-Policy["']|Content-Security-Policy)[^>]*content="([^"]+)"/i,
);
if (!cspMatch) {
  throw new Error("生产入口必须包含 enforce 模式 Content-Security-Policy");
}
const csp = cspMatch[1]
  .replaceAll("&#39;", "'")
  .replaceAll("&quot;", '"')
  .replaceAll("&amp;", "&");
for (const directive of [
  "default-src 'self'",
  "base-uri 'none'",
  "object-src 'none'",
  "script-src 'self'",
  "connect-src 'self'",
]) {
  if (!csp.includes(directive)) {
    throw new Error(`生产 CSP 缺少必要指令：${directive}`);
  }
}
for (const forbidden of ["'unsafe-eval'", "https:", "http:"]) {
  if (csp.includes(forbidden)) {
    throw new Error(`生产 CSP 包含禁止的脚本或外部源能力：${forbidden}`);
  }
}
const publicSourceMaps = files.filter((file) => extname(file) === ".map");
if (publicSourceMaps.length) {
  throw new Error(
    `生产包不得公开 source map：${publicSourceMaps
      .map((file) => relative(buildRoot, file))
      .join(", ")}`,
  );
}

for (const file of files.filter((candidate) =>
  [".html", ".js", ".css"].includes(extname(candidate)),
)) {
  const content = await readFile(file, "utf8");
  const marker = forbiddenWorkbenchMarkers.find((value) =>
    content.includes(value),
  );
  if (marker) {
    throw new Error(
      `生产包包含 Workbench 标记 ${JSON.stringify(marker)}：${relative(
        buildRoot,
        file,
      )}`,
    );
  }
}

stdout.write(
  `production build verification: ${files.length} files, enforce CSP, no Workbench chunk or public source map\n`,
);
