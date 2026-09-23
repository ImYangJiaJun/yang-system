/* 在 jsdom 里真跑一遍四个原型：渲染、切身份、开对话框、删数据源。
   静态检查证明不了脚本能跑，这一步才算数。
   用法：node variants/_src/smoke.mjs                                          */

import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const OUT = join(HERE, "..");

// jsdom 装在 frontend/ 里，用绝对路径动态 import（NODE_PATH 对 ESM 不生效）
const jsdomEntry = "D:/code/lib_yang/project/yang-system/frontend/node_modules/.pnpm/jsdom@29.1.1/node_modules/jsdom/lib/api.js";
const { JSDOM } = await import(pathToFileURL(jsdomEntry).href);

let failed = 0;
function check(label, ok, detail) {
  if (!ok) failed += 1;
  console.log("  " + (ok ? "ok   " : "FAIL ") + label + (detail ? "  — " + detail : ""));
}

const files = readdirSync(OUT).filter((n) => n.endsWith(".html") && n !== "index.html").sort();

for (const file of files) {
  const html = readFileSync(join(OUT, file), "utf8");
  console.log("\n" + file);

  // jsdom 没有 matchMedia，按浏览器真实行为补一个，让 prefers-color-scheme 分支也能跑到
  const dom = new JSDOM(html, {
    runScripts: "dangerously",
    pretendToBeVisual: true,
    beforeParse(win) {
      win.matchMedia = (query) => ({
        matches: false,
        media: query,
        onchange: null,
        addListener() {}, removeListener() {},
        addEventListener() {}, removeEventListener() {}, dispatchEvent: () => false,
      });
    },
  });
  const { window } = dom;
  const doc = window.document;
  const $ = (sel) => doc.querySelector(sel);
  const $$ = (sel) => [...doc.querySelectorAll(sel)];
  const click = (el) => el.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));

  // 1. 首屏：外壳与主区都渲染出来了
  check("侧边栏渲染", $$(".navlink").length > 0);
  check("有页面标题", Boolean($(".page-title")), $(".page-title") && $(".page-title").textContent);
  check("有原型控制条", $$(".protoctl .seg").length === 3);
  check("默认有数据行/卡片", $$(".tile, .ledger-row, .split-item").length >= 5,
    "found " + $$(".tile, .ledger-row, .split-item").length);

  // 2. 分栏布局（C）要先选中一个，右栏的 ⋯ 才存在
  const firstItem = $('[data-act="select-ds"]');
  if (firstItem) click(firstItem);

  const menuSelector = '[data-act="toggle-menu"][data-key]:not([data-key="__density"])';

  // 3. 运维态：写入口在
  check("运维态有「添加数据源」", $$('[data-act="open-add"]').length > 0);
  check("运维态有卡片/行上的 ⋯ 菜单", $$(menuSelector).length > 0);

  // 4. 切到业务态：写入口整块消失（不是 disabled）
  click($$('.protoctl [data-act="set"][data-name="role"][data-value="biz"]')[0]);
  check("业务态隐藏「添加数据源」", $$('[data-act="open-add"]').length === 0);
  check("业务态隐藏卡片/行上的 ⋯ 菜单", $$(menuSelector).length === 0);
  check(
    "业务态写操作不是 disabled 而是不渲染",
    [...doc.querySelectorAll("button[disabled]")].every((b) => !["open-add", "open-delete"].includes(b.getAttribute("data-act")))
  );

  // 5. 切回运维态，开「添加数据源」对话框
  click($$('.protoctl [data-act="set"][data-name="role"][data-value="ops"]')[0]);
  if (firstItem) { const again = $('[data-act="select-ds"]'); if (again) click(again); }
  const addBtn = $('[data-act="open-add"]');
  click(addBtn);
  check("对话框打开", Boolean($(".dialog")));
  check("对话框 Token 是密码型", $('.dialog [data-field="token"]')?.type === "password");
  check("对话框有 role=dialog", $(".dialog")?.getAttribute("role") === "dialog");

  // 5. Esc 关对话框
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  check("Esc 能关对话框", !$(".dialog"));

  // 6. 校验失败要留在对话框里并给出后端原文
  click($('[data-act="open-add"]'));
  $('.dialog [data-field="title"]').value = "报销事由分类";
  click($('[data-act="submit-add"]'));
  check("空标识被拦下", Boolean($(".dialog")), "对话框仍在");
  check("空标识提示", ($(".field-error")?.textContent || "").includes("数据源标识"));
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));

  // 7. 删除确认对话框用后端原文，且说明连带停用
  click($(menuSelector));
  check("⋯ 菜单展开", Boolean($('[data-act="open-delete"]')));
  click($('[data-act="open-delete"]'));
  const dlgText = $(".dialog")?.textContent || "";
  check("删除确认用后端原文", dlgText.includes("删除后其下全部选项会被同时停用，且不可恢复。确认删除？"));
  check("删除确认提到连带停用数量", /个选项将被连带停用/.test(dlgText));

  const before = $$(".tile, .ledger-row, .split-item").length;
  click($('[data-act="confirm-delete"]'));
  const after = $$(".tile, .ledger-row, .split-item").length;
  check("删除真的生效", after === before - 1, before + " -> " + after);

  // 8. 空态：给出配置路径（A/B/C 走可折叠指引，D 走向导），且主行动明确
  click($$('.protoctl [data-act="set"][data-name="dataState"][data-value="empty"]')[0]);
  check("空态有引导文案（不是「暂无数据」）", !/暂无数据/.test($("#main").textContent));
  check(
    "空态给出配置路径（指引展开或向导）",
    $('.guide[data-open="true"]') !== null || $(".wtrail") !== null
  );
  check("空态有明确主行动", $$('#main [data-act="open-add"], #main [data-act="wiz-next"]').length > 0);

  // 8b. 方案 D 的向导要真的能走完第 1→2 步并建出数据源
  if ($(".wtrail")) {
    check("向导有四步", $$(".wstep").length === 4);
    check("从第 1 步开始", $('.wstep[data-state="current"] .wstep-num').textContent.trim() === "1");
    click($('[data-act="wiz-next"]'));
    check("走到第 2 步", $('.wstep[data-state="current"] .wstep-num').textContent.trim() === "2");
    check("第 2 步是内联表单（不是对话框）", Boolean($('#main [data-field="token"]')) && !$(".dialog"));
    check("内联 Token 也是密码型", $('#main [data-field="token"]').type === "password");

    // 校验：空标识应拦下
    $('#main [data-field="title"]').value = "新数据源";
    click($('[data-act="wiz-create"]'));
    check("向导内校验生效", $('.wstep[data-state="current"] .wstep-num').textContent.trim() === "2");

    // 填对之后应前进到第 3 步，且数据源真的建出来了
    $('#main [data-field="source_key"]').value = "new-source";
    $('#main [data-field="title"]').value = "新数据源";
    $('#main [data-field="token"]').value = "t-1234567890";
    click($('[data-act="wiz-create"]'));
    check("建成功后前进到第 3 步", $('.wstep[data-state="current"] .wstep-num').textContent.trim() === "3");
    click($('[data-act="wiz-skip"]'));
    check("跳过后台账里有新数据源", $("#main").textContent.includes("新数据源"));
  }

  // 9. 加载态 / 错误态
  click($$('.protoctl [data-act="set"][data-name="dataState"][data-value="loading"]')[0]);
  check("加载态是骨架屏", $$(".skel").length >= 3);
  click($$('.protoctl [data-act="set"][data-name="dataState"][data-value="error"]')[0]);
  check("错误态有重试", Boolean($('[data-act="retry"]')));
  check("错误态用 role=alert", $('[role="alert"]') !== null);
  click($('[data-act="retry"]'));
  check("重试回到有数据", $$(".tile, .ledger-row, .split-item").length >= 5);

  // 10. 进详情：选项区只读，且没有增改按钮
  click($$('.protoctl [data-act="set"][data-name="dataState"][data-value="data"]')[0]);
  const open = $('[data-act="open-detail"], [data-act="select-ds"]');
  click(open);
  const optText = $("#main").textContent;
  check("详情出现选项表", $(".opt-table") !== null);
  check("详情声明只读", /控制台只读/.test(optText));
  check("详情没有任何写选项的入口", $$('[data-act="add-option"], [data-act="edit-option"], [data-act="delete-option"]').length === 0);

  // 11. 明暗切换落到 html[data-theme]
  click($('[data-act="toggle-theme"]'));
  check("明暗切换生效", ["dark", "light"].includes(doc.documentElement.getAttribute("data-theme")),
    doc.documentElement.getAttribute("data-theme"));

  dom.window.close();
}

console.log("\n" + (failed ? failed + " 项未通过" : "四个原型的功能冒烟测试全部通过"));
process.exit(failed ? 1 : 0);
