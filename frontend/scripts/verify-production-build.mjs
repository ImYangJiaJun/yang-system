import { readdir, readFile } from "node:fs/promises";
import { extname, relative, resolve } from "node:path";
import { stdout } from "node:process";

const buildRoot = resolve("dist/spa");
const forbiddenWorkbenchMarkers = [
  "YANG 接口工作台",
  "后端注册即可演示，复杂场景允许覆盖",
];

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
  `production build verification: ${files.length} files, no Workbench chunk or public source map\n`,
);
