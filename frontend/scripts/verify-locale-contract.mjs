import { readdir, readFile } from "node:fs/promises";
import { extname, resolve } from "node:path";
import { stdout } from "node:process";

/**
 * 单语言产品合同校验（对齐旧 frontend/scripts/verify-locale-contract.mjs 语义）：
 * - HTML 文档语言固定 zh-CN；
 * - 产品 locale 唯一权威常量（src/lib/product-locale.ts）；
 * - 大小写归一/文本排序/Intl 格式化必须显式使用产品 locale；
 * - 应用代码与单元测试（src/ 与 tests/ 下 .ts/.tsx）禁止依赖浏览器环境的隐式 locale API；
 * - docs/LOCALE.md 携带三个机器标记；
 * 并对每个断言做破坏性变异自检（变异必须被拒绝）。
 */

const supportedLocale = "zh-CN";
const frontendRoot = resolve(".");
const sources = {
  index: await readFile(resolve(frontendRoot, "index.html"), "utf8"),
  locale: await readFile(
    resolve(frontendRoot, "src/lib/product-locale.ts"),
    "utf8",
  ),
  cell: await readFile(
    resolve(frontendRoot, "src/renderers/table/business-cell-model.ts"),
    "utf8",
  ),
  document: await readFile(resolve(frontendRoot, "docs/LOCALE.md"), "utf8"),
  application: [
    await applicationSource(resolve(frontendRoot, "src")),
    await applicationSource(resolve(frontendRoot, "tests")),
  ].join("\n"),
};

verifyLocaleContract(sources);

const mutations = [
  ["HTML language", "index", 'lang="zh-CN"', 'lang="en-US"'],
  [
    "product locale",
    "locale",
    'PRODUCT_LOCALE = "zh-CN"',
    'PRODUCT_LOCALE = "en-US"',
  ],
  [
    "unpinned locale API",
    "application",
    "export const PRODUCT_LOCALE",
    '"x".toLocaleLowerCase();\nexport const PRODUCT_LOCALE',
  ],
  [
    "reopen trigger",
    "document",
    "<!-- locale-contract: reopen-trigger second-locale -->",
    "",
  ],
];

for (const [name, key, target, replacement] of mutations) {
  const value = sources[key];
  if (!value.includes(target)) {
    throw new Error(`语言合同变异目标不存在：${name}`);
  }
  let rejected = false;
  try {
    verifyLocaleContract({
      ...sources,
      [key]: value.replace(target, replacement),
    });
  } catch {
    rejected = true;
  }
  if (!rejected) {
    throw new Error(`语言合同未拒绝破坏性变异：${name}`);
  }
}

stdout.write(
  `locale contract verification: ${supportedLocale} single-locale product, explicit browser locale, ${mutations.length} adversarial mutations rejected\n`,
);

function verifyLocaleContract(candidate) {
  requireText(
    candidate.index,
    `<html lang="${supportedLocale}">`,
    "HTML 文档语言必须固定为 zh-CN",
  );
  requireText(
    candidate.locale,
    `export const PRODUCT_LOCALE = "${supportedLocale}" as const;`,
    "产品 locale 必须只有一个权威常量",
  );
  requireText(
    candidate.locale,
    "value.toLocaleLowerCase(PRODUCT_LOCALE)",
    "大小写归一必须使用显式产品 locale",
  );
  requireText(
    candidate.locale,
    "left.localeCompare(right, PRODUCT_LOCALE)",
    "文本排序必须使用显式产品 locale",
  );
  requireText(
    candidate.cell,
    "Intl.DateTimeFormat(PRODUCT_LOCALE",
    "日期格式必须使用显式产品 locale",
  );
  for (const marker of [
    "<!-- locale-contract: supported zh-CN -->",
    "<!-- locale-contract: runtime-switch disabled -->",
    "<!-- locale-contract: reopen-trigger second-locale -->",
  ]) {
    requireText(
      candidate.document,
      marker,
      `语言产品合同缺少机器标记 ${marker}`,
    );
  }
  if (/\.toLocale(?:Lower|Upper)Case\(\s*\)/.test(candidate.application)) {
    throw new Error("应用代码禁止使用依赖浏览器环境的无参数 locale 大小写转换");
  }
  if (/\.localeCompare\([^,)]*\)/.test(candidate.application)) {
    throw new Error("应用代码禁止使用依赖浏览器环境的无 locale 文本排序");
  }
  if (
    /Intl\.(?:DateTime|Number)Format\(\s*(?:\)|undefined)/.test(
      candidate.application,
    )
  ) {
    throw new Error("应用代码禁止使用依赖浏览器环境的无 locale Intl 格式化");
  }
}

async function applicationSource(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const values = await Promise.all(
    entries.map(async (entry) => {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) return applicationSource(path);
      return [".ts", ".tsx"].includes(extname(path))
        ? readFile(path, "utf8")
        : "";
    }),
  );
  return values.join("\n");
}

function requireText(source, expected, message) {
  if (!source.includes(expected)) throw new Error(message);
}
