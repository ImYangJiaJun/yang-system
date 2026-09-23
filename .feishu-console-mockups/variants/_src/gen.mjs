/* 把 base.css + proto-x.css + proto-x.js + app.js 拼成 4 个自包含单文件 HTML。
   产物零外部依赖、file:// 双击可开。
   用法：node variants/_src/gen.mjs                                            */

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const OUT = join(HERE, "..");

const BASE_CSS = readFileSync(join(HERE, "base.css"), "utf8").trim();
const APP_JS = readFileSync(join(HERE, "app.js"), "utf8").trim();

const PROTOTYPES = [
  {
    file: "A-card-gallery.html",
    id: "A",
    name: "卡片画廊",
    title: "飞书数据源控制台 · 方案 A 卡片画廊",
    blurb: "一个数据源是一个有边界的「东西」，值得占一块地方。",
    best: "文档原案；两类角色都能接受",
  },
  {
    file: "B-compact-ledger.html",
    id: "B",
    name: "紧凑目录",
    title: "飞书数据源控制台 · 方案 B 紧凑目录",
    blurb: "日常不是管很多数据源，而是确认某一个对不对——让状态一屏扫完。",
    best: "业务态日常查看；密度可调是卖点",
  },
  {
    file: "C-master-detail.html",
    id: "C",
    name: "主从分栏",
    title: "飞书数据源控制台 · 方案 C 主从分栏",
    blurb: "看数据源不是终点，看它下面有哪些选项才是。",
    best: "业务态频繁钻取；省掉一次导航",
  },
  {
    file: "D-wizard-first.html",
    id: "D",
    name: "向导优先",
    title: "飞书数据源控制台 · 方案 D 向导优先",
    blurb: "90% 的价值发生在第一次配置上，那就把说明变成流程。",
    best: "运维态低频配置；第一次最省心",
  },
];

function build(proto) {
  const css = readFileSync(join(HERE, "proto-" + proto.id.toLowerCase() + ".css"), "utf8").trim();
  const js = readFileSync(join(HERE, "proto-" + proto.id.toLowerCase() + ".js"), "utf8").trim();

  return `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>${proto.title}</title>
<meta name="description" content="${proto.blurb}">
<style>
${BASE_CSS}

${css}
</style>
</head>
<body>
<div id="root"></div>
<script>
'use strict';
${js}

${APP_JS}
</script>
</body>
</html>
`;
}

for (const proto of PROTOTYPES) {
  writeFileSync(join(OUT, proto.file), build(proto), "utf8");
}

const indexHtml = `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>飞书数据源控制台 · 四个候选</title>
<style>
${BASE_CSS}

.index { max-width: 880px; margin: 0 auto; padding: 48px 24px 48px; }
.index h1 { font-size: 24px; font-weight: 600; letter-spacing: -0.01em; }
.index > p { margin-top: 8px; font-size: 14px; color: var(--muted-foreground); max-width: 62ch; }
.index-note {
  margin-top: 20px; padding: 12px 16px;
  border: 1px solid var(--border); border-radius: 10px;
  background: color-mix(in oklab, var(--muted) 40%, transparent);
  font-size: 14px; color: var(--muted-foreground); max-width: 68ch;
}
.index-note b { color: var(--foreground); font-weight: 500; }
.picks { display: grid; gap: 12px; margin-top: 28px; }
.pick {
  display: block; text-decoration: none; color: inherit;
  border: 1px solid var(--border); border-radius: 14px;
  background: var(--card); padding: 18px 20px;
  transition: border-color .12s, background-color .12s;
}
.pick:hover { border-color: var(--ring); background: color-mix(in oklab, var(--accent) 50%, var(--card)); }
.pick-head { display: flex; align-items: baseline; gap: 10px; flex-wrap: wrap; }
.pick-id {
  width: 22px; height: 22px; flex-shrink: 0; border-radius: 6px;
  background: var(--primary); color: var(--primary-foreground);
  display: grid; place-items: center; font-size: 12px; font-weight: 600;
  align-self: center;
}
.pick-name { font-size: 15px; font-weight: 500; }
.pick-blurb { display: block; margin-top: 6px; font-size: 14px; color: var(--muted-foreground); }
.pick-best { display: block; margin-top: 10px; font-size: 12px; color: var(--muted-foreground); }
.pick-best b { color: var(--foreground); font-weight: 500; }
</style>
</head>
<body>
<main class="index">
  <h1>飞书数据源控制台 · 四个候选</h1>
  <p>四个方案共用同一套 token、组件配方与文案，差别只在信息架构：把「确认飞书那边配的选项，这边真的通着」这件事的重心放在哪里。</p>
  <div class="index-note">
    <b>怎么看：</b>每页底部有一条虚线框的「原型控制」条（不属于产品界面），用来切换
    空 / 有数据 / 加载 / 错误、运维 / 业务两种身份、以及明暗主题。卡片、整行、对话框、下拉菜单都真的能点，
    <b>Esc</b> 关浮层，浏览器后退键在方案 A / B / D 里可用。业务身份下，所有写操作入口是<b>整块不出现</b>，不是禁用。
  </div>
  <div class="picks">
${PROTOTYPES.map(function (p) {
  return `    <a class="pick" href="${p.file}">
      <span class="pick-head"><span class="pick-id">${p.id}</span><span class="pick-name">${p.name}</span></span>
      <span class="pick-blurb">${p.blurb}</span>
      <span class="pick-best">最适合：<b>${p.best}</b></span>
    </a>`;
}).join("\n")}
  </div>
</main>
</body>
</html>
`;

writeFileSync(join(OUT, "index.html"), indexHtml, "utf8");

console.log("已生成 4 个原型 + index.html 于 " + OUT);
