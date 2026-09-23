/* ============================================================================
   飞书数据源控制台 · 原型共享运行时
   由 proto-*.js 先定义 LAYOUT，再执行本文件（同一 <script> 内顺序拼接）。
   ========================================================================= */

/* ------------------------------- 图标（lucide） ------------------------------ */

const ICONS = {
  plus: '<path d="M5 12h14"/><path d="M12 5v14"/>',
  minus: '<path d="M5 12h14"/>',
  x: '<path d="M18 6 6 18"/><path d="m6 6 12 12"/>',
  check: '<path d="M20 6 9 17l-5-5"/>',
  "chevron-down": '<path d="m6 9 6 6 6-6"/>',
  "chevron-right": '<path d="m9 18 6-6-6-6"/>',
  "chevron-left": '<path d="m15 18-6-6 6-6"/>',
  "chevrons-up-down": '<path d="m7 15 5 5 5-5"/><path d="m7 9 5-5 5 5"/>',
  "arrow-left": '<path d="m12 19-7-7 7-7"/><path d="M19 12H5"/>',
  "more-horizontal":
    '<circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/>',
  search: '<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>',
  "refresh-cw":
    '<path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"/><path d="M8 16H3v5"/>',
  copy:
    '<rect x="8" y="8" width="14" height="14" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/>',
  moon: '<path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/>',
  sun:
    '<circle cx="12" cy="12" r="4"/><path d="M12 2v2"/><path d="M12 20v2"/><path d="m4.93 4.93 1.41 1.41"/><path d="m17.66 17.66 1.41 1.41"/><path d="M2 12h2"/><path d="M20 12h2"/><path d="m6.34 17.66-1.41 1.41"/><path d="m19.07 4.93-1.41 1.41"/>',
  "circle-user":
    '<circle cx="12" cy="12" r="10"/><circle cx="12" cy="10" r="3"/><path d="M7 20.662V19a5 5 0 0 1 10 0v1.662"/>',
  "log-out":
    '<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><path d="m16 17 5-5-5-5"/><path d="M21 12H9"/>',
  "building-2":
    '<path d="M6 22V4a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v18Z"/><path d="M6 12H4a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h2"/><path d="M18 9h2a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2h-2"/><path d="M10 6h4"/><path d="M10 10h4"/><path d="M10 14h4"/><path d="M10 18h4"/>',
  users:
    '<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/>',
  "shield-check":
    '<path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"/><path d="m9 12 2 2 4-4"/>',
  puzzle:
    '<path d="M19.439 7.85c-.049.322.059.648.289.878l1.568 1.568c.47.47.706 1.087.706 1.704s-.235 1.233-.706 1.704l-1.611 1.611a.98.98 0 0 1-.837.276c-.47-.07-.802-.48-.968-.925a2.501 2.501 0 1 0-3.214 3.214c.446.166.855.497.925.968a.979.979 0 0 1-.276.837l-1.61 1.61a2.404 2.404 0 0 1-1.705.707 2.402 2.402 0 0 1-1.704-.706l-1.568-1.568a1.026 1.026 0 0 0-.877-.29c-.493.074-.84.504-1.02.968a2.5 2.5 0 1 1-3.237-3.237c.464-.18.894-.527.967-1.02a1.026 1.026 0 0 0-.289-.877l-1.568-1.568A2.402 2.402 0 0 1 1.998 12c0-.617.236-1.234.706-1.704L4.23 8.77c.24-.24.581-.353.917-.303.515.077.877.528 1.073 1.01a2.5 2.5 0 1 0 3.259-3.259c-.482-.196-.933-.558-1.01-1.073-.05-.336.062-.676.303-.917l1.525-1.525A2.402 2.402 0 0 1 12 1.998c.617 0 1.234.236 1.704.706l1.568 1.568c.23.23.556.338.877.29.493-.074.84-.504 1.02-.968a2.5 2.5 0 1 1 3.237 3.237c-.464.18-.894.527-.967 1.02Z"/>',
  "table-2":
    '<path d="M9 3H5a2 2 0 0 0-2 2v4m6-6h10a2 2 0 0 1 2 2v4M9 3v18m0 0h10a2 2 0 0 0 2-2V9M9 21H5a2 2 0 0 1-2-2V9m0 0h18"/>',
  list:
    '<path d="M3 12h.01"/><path d="M3 18h.01"/><path d="M3 6h.01"/><path d="M8 12h13"/><path d="M8 18h13"/><path d="M8 6h13"/>',
  database:
    '<ellipse cx="12" cy="5" rx="9" ry="3"/><path d="M3 5v14a9 3 0 0 0 18 0V5"/><path d="M3 12a9 3 0 0 0 18 0"/>',
  lock:
    '<rect x="3" y="11" width="18" height="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>',
  "key-round":
    '<path d="M2.586 17.414A2 2 0 0 0 2 18.828V21a1 1 0 0 0 1 1h3a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h1a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h.172a2 2 0 0 0 1.414-.586l.814-.814a6.5 6.5 0 1 0-4-4z"/><circle cx="16.5" cy="7.5" r=".5" fill="currentColor"/>',
  power:
    '<path d="M12 2v10"/><path d="M18.4 6.6a9 9 0 1 1-12.77.04"/>',
  "trash-2":
    '<path d="M3 6h18"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/><path d="M10 11v6"/><path d="M14 11v6"/>',
  "square-pen":
    '<path d="M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.375 2.625a2.121 2.121 0 1 1 3 3L12 15l-4 1 1-4Z"/>',
  "triangle-alert":
    '<path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/>',
  info:
    '<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/>',
  "circle-check":
    '<circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/>',
  "circle-alert":
    '<circle cx="12" cy="12" r="10"/><path d="M12 8v4"/><path d="M12 16h.01"/>',
  "circle-help":
    '<circle cx="12" cy="12" r="10"/><path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3"/><path d="M12 17h.01"/>',
  filter: '<path d="M22 3H2l8 9.46V19l4 2v-8.54L22 3z"/>',
  clock: '<circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/>',
  "external-link":
    '<path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>',
};

function icon(name, cls) {
  const body = ICONS[name] || ICONS.puzzle;
  return '<svg class="i' + (cls ? " " + cls : "") + '" viewBox="0 0 24 24" aria-hidden="true">' + body + "</svg>";
}

/* --------------------------------- 数据 ---------------------------------- */

const DATASOURCES = [
  { source_key: "expense-category", title: "报销事由分类", status: "active", encrypt_enabled: true, default_locale: "zh-CN", option_count: 8, updated_at: "09-18 14:02" },
  { source_key: "leave-type", title: "请假类型", status: "active", encrypt_enabled: false, default_locale: "zh-CN", option_count: 6, updated_at: "09-17 09:31" },
  { source_key: "purchase-dept", title: "采购归口部门", status: "active", encrypt_enabled: true, default_locale: "zh-CN", option_count: 12, updated_at: "09-16 18:44" },
  { source_key: "vendor-list", title: "供应商名录", status: "disabled", encrypt_enabled: false, default_locale: "zh-CN", option_count: 23, updated_at: "08-29 11:07" },
  { source_key: "it-asset-class", title: "IT 资产分类", status: "active", encrypt_enabled: false, default_locale: "en-US", option_count: 9, updated_at: "09-15 10:20" },
  { source_key: "travel-city", title: "差旅城市", status: "active", encrypt_enabled: false, default_locale: "zh-CN", option_count: 31, updated_at: "09-14 16:55" },
  { source_key: "contract-type", title: "合同类型", status: "disabled", encrypt_enabled: true, default_locale: "zh-CN", option_count: 4, updated_at: "07-21 08:12" },
  { source_key: "cost-center", title: "成本中心", status: "active", encrypt_enabled: false, default_locale: "zh-CN", option_count: 17, updated_at: "09-19 13:05" },
];

const OPTIONS_BY_KEY = {
  "expense-category": [
    ["差旅费", true, true, 10], ["办公用品", false, true, 20], ["业务招待", false, true, 30],
    ["培训费", false, true, 40], ["通讯费", false, true, 50], ["房租水电", false, false, 60],
    ["咨询服务费", false, true, 70], ["其他", false, true, 999],
  ],
  "leave-type": [
    ["年假", true, true, 10], ["事假", false, true, 20], ["病假", false, true, 30],
    ["调休", false, true, 40], ["婚假", false, true, 50], ["产假", false, true, 60],
  ],
  "purchase-dept": [
    ["行政部", true, true, 10], ["信息科技部", false, true, 20], ["市场部", false, true, 30],
    ["人力资源部", false, true, 40], ["财务部", false, true, 50], ["法务部", false, true, 60],
    ["研发中心", false, true, 70], ["供应链管理部", false, true, 80],
    ["质量管理部", false, true, 90], ["生产制造部", false, true, 100],
    ["安全环保部", false, false, 110], ["战略投资部", false, true, 120],
  ],
  "it-asset-class": [
    ["Laptop", true, true, 10], ["Desktop", false, true, 20], ["Monitor", false, true, 30],
    ["Mobile Device", false, true, 40], ["Network Gear", false, true, 50],
    ["Peripheral", false, true, 60], ["Server", false, true, 70],
    ["Software License", false, true, 80], ["Other", false, true, 90],
  ],
  "travel-city": [
    ["北京", true, true, 10], ["上海", false, true, 20], ["广州", false, true, 30],
    ["深圳", false, true, 40], ["杭州", false, true, 50], ["成都", false, true, 60],
    ["武汉", false, true, 70], ["西安", false, true, 80],
  ],
  "cost-center": [
    ["CC-1001 总部行政", true, true, 10], ["CC-1002 华东大区", false, true, 20],
    ["CC-1003 华南大区", false, true, 30], ["CC-1004 研发投入", false, true, 40],
    ["CC-1005 市场推广", false, true, 50],
  ],
  "vendor-list": [
    ["中远物流有限公司", true, false, 10], ["恒信办公设备", false, false, 20],
    ["云启技术服务", false, false, 30], ["华宇建筑工程", false, false, 40],
  ],
  "contract-type": [
    ["采购合同", true, true, 10], ["服务合同", false, true, 20],
    ["租赁合同", false, true, 30], ["框架协议", false, true, 40],
  ],
};

function optionsFor(key) {
  const rows = OPTIONS_BY_KEY[key] || [];
  return rows.map(function (row, index) {
    return {
      option_id: "opt_" + key.replace(/-/g, "_") + "_" + String(index + 1).padStart(3, "0"),
      source_key: key,
      label: row[0],
      i18n: null,
      sort_order: row[3],
      is_default: row[1],
      enabled: row[2],
    };
  });
}

/* --------------------------------- 状态 ---------------------------------- */

const STATE = {
  dataState: "data", // data | empty | loading | error
  role: "ops", // ops | biz
  theme: null, // null = 跟随系统 | 'light' | 'dark'
  route: { name: "list", key: null },
  query: { q: "", status: "all" },
  guideOpen: false,
  menuFor: null,
  dialog: null,
  busy: false,
  formError: "",
  toast: "",
  wizardStep: 1,
  wizardActive: false,
  wizardDismissed: false,
  density: "standard",
};

const read = {
  canWrite: function () { return STATE.role === "ops"; },
  canRead: function () { return true; },
  canReadOptions: function () { return true; },
  all: function () { return DATASOURCES; },
  filtered: function () {
    const q = STATE.query.q.trim().toLowerCase();
    return DATASOURCES.filter(function (ds) {
      if (STATE.query.status !== "all" && ds.status !== STATE.query.status) return false;
      if (!q) return true;
      return ds.title.toLowerCase().indexOf(q) >= 0 || ds.source_key.toLowerCase().indexOf(q) >= 0;
    });
  },
  byKey: function (key) {
    for (let i = 0; i < DATASOURCES.length; i += 1) {
      if (DATASOURCES[i].source_key === key) return DATASOURCES[i];
    }
    return null;
  },
  selected: function () {
    return STATE.route.key ? read.byKey(STATE.route.key) : null;
  },
  options: function (key) { return optionsFor(key); },
  activeCount: function () {
    return DATASOURCES.filter(function (ds) { return ds.status === "active"; }).length;
  },
};

/* -------------------------------- 小工具 --------------------------------- */

function esc(value) {
  return String(value == null ? "" : value)
    .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}

function statusLabel(ds) { return ds.status === "active" ? "启用" : "已停用"; }
function statusTone(ds) { return ds.status === "active" ? "positive" : "warning"; }

function statusBadge(ds) {
  return '<span class="badge tone-' + statusTone(ds) + '"><i class="dot"></i>' +
    statusLabel(ds) + "</span>";
}

function encryptFlag(ds) {
  return ds.encrypt_enabled
    ? '<span class="badge tone-info">' + icon("lock") + "加密</span>"
    : '<span class="badge tone-neutral">未加密</span>';
}

function toast(message) {
  STATE.toast = message;
  render();
  window.setTimeout(function () {
    if (STATE.toast === message) { STATE.toast = ""; render(); }
  }, 2400);
}

/* --------------------------------- 指引 ---------------------------------- */

const GUIDE_STEPS = [
  ["在飞书审批后台配置控件", "单选或多选控件把取值方式改成「使用外部选项」，自定义一个 Token 并记住它。"],
  ["在这里建一个数据源", "数据源标识会进接口 URL；把上一步的 Token 粘进来。"],
  ["把接口地址与 Token 填回审批后台", "在飞书那边点「校验数据」，确认能拉到选项。"],
  ["在多维表格配自动化推送选项", "HTTP 节点带 Authorization 头调写入接口，选项随表格变动自动更新。"],
];

function guideHtml(options) {
  const open = options && options.forceOpen ? true : STATE.guideOpen;
  const steps = GUIDE_STEPS.map(function (step, index) {
    return '<div class="guide-step"><span class="guide-num num">' + (index + 1) + "</span>" +
      "<div><h3>" + step[0] + "</h3><p>" + step[1] + "</p></div></div>";
  }).join("");
  return '<section class="guide" data-open="' + open + '">' +
    '<button type="button" class="guide-head" data-act="toggle-guide">' +
    icon("circle-help", "i-sm") +
    "<span>从飞书审批后台到多维表格，四步接起来</span>" +
    '<span class="guide-caret">' + icon("chevron-down", "i-sm") + "</span>" +
    "</button>" +
    '<div class="guide-body"><div class="guide-steps">' + steps + "</div></div></section>";
}

/* ------------------------------- 详情选项表 ------------------------------- */

function optionsBlock(ds, compact) {
  const rows = read.options(ds.source_key);
  const body = rows.map(function (opt) {
    const dim = opt.enabled ? "" : ' style="opacity:.55"';
    return "<tr" + dim + ">" +
      '<td class="mono num">' + esc(opt.option_id) + "</td>" +
      "<td>" + esc(opt.label) + (opt.is_default ? ' <span class="badge tone-info">默认</span>' : "") + "</td>" +
      '<td class="num">' + opt.sort_order + "</td>" +
      "<td>" + (opt.enabled ? '<span class="badge tone-positive">启用</span>' : '<span class="badge tone-warning">已停用</span>') + "</td>" +
      "</tr>";
  }).join("");

  return '<div class="opt-wrap">' +
    '<div class="opt-head">' +
    '<h2 class="opt-title">选项 ' + rows.length + " 个</h2>" +
    '<span class="badge tone-neutral">' + icon("lock", "i-sm") + "只读</span>" +
    "</div>" +
    '<p class="opt-note">' + icon("info", "i-sm") +
    "选项由飞书多维表格自动推送，控制台只读——增改请回多维表格操作。</p>" +
    '<div class="opt-scroll"' + (compact ? "" : "") + ">" +
    '<table class="opt-table"><thead><tr>' +
    "<th>option_id</th><th>名称</th><th>排序</th><th>状态</th>" +
    "</tr></thead><tbody>" + body + "</tbody></table>" +
    "</div></div>";
}

/* ------------------------------- 页面骨架 -------------------------------- */

function pageHeader(title, subtitle, actions) {
  return '<div class="page-head"><div>' +
    '<h1 class="page-title">' + esc(title) + "</h1>" +
    (subtitle ? '<p class="page-sub">' + esc(subtitle) + "</p>" : "") +
    "</div>" + (actions ? '<div class="page-actions">' + actions + "</div>" : "") + "</div>";
}

function addButton() {
  if (!read.canWrite()) return "";
  return '<button type="button" class="btn btn-default" data-act="open-add">' +
    icon("plus") + "添加数据源</button>";
}

/* --------------------------- 数据态：加载 / 错误 --------------------------- */

function skeletonHtml() {
  return '<div class="page">' +
    '<div class="skel" style="height:32px;width:256px"></div>' +
    '<div class="skel" style="height:16px;width:384px;margin-top:12px"></div>' +
    '<div class="skel" style="height:220px;margin-top:24px"></div>' +
    "</div>";
}

function errorHtml() {
  return '<div class="page page-narrow">' +
    '<div class="banner banner-error banner-row" role="alert">' + icon("circle-alert") +
    "<div><strong>数据源加载失败</strong><p>请求 /api/v1/feishu/datasources/query 时服务端返回 500。</p></div></div>" +
    '<div style="margin-top:16px"><button type="button" class="btn btn-outline" data-act="retry">' +
    icon("refresh-cw") + "重试</button></div></div>";
}

/// 通用空状态：一件事情的说明 + 一个明确动作，不是一句「暂无数据」。
/// 方案可以整体替换它（方案 D 用向导），也可以在自己的 empty() 里回退到它。
function baseEmptyHtml() {
  return '<div class="page">' + guideHtml({ forceOpen: true }) +
    '<div style="margin-top:16px"><div class="empty">' +
    '<span class="empty-icon">' + icon("database") + "</span>" +
    "<h2>还没有数据源</h2>" +
    "<p>先在飞书审批后台配好控件并拿到 Token，再回来建第一个。</p>" +
    '<div class="empty-actions">' +
    (read.canWrite() ? '<button type="button" class="btn btn-default" data-act="open-add">' + icon("plus") + "添加数据源</button>" : "") +
    '<button type="button" class="btn btn-outline" data-act="toggle-guide">怎么拿到 Token？</button>' +
    "</div></div></div></div>";
}

function emptyHtml() {
  return LAYOUT.empty ? LAYOUT.empty() : baseEmptyHtml();
}

/* -------------------------------- 详情页 --------------------------------- */

function detailHtml() {
  const ds = read.selected();
  if (!ds) {
    return '<div class="page"><div class="empty"><span class="empty-icon">' + icon("database") + "</span>" +
      "<h2>数据源不存在</h2><p>它可能已被删除，或链接里的标识写错了。</p>" +
      '<div class="empty-actions"><button type="button" class="btn btn-outline" data-act="back">' +
      icon("arrow-left") + "返回列表</button></div></div></div>";
  }

  const stale = ds.status === "disabled"
    ? '<div class="banner" style="margin-bottom:16px">' + icon("info", "i-sm") +
      " 这个数据源已停用，飞书审批取选项会失败；下面的选项数据仍然保留。</div>"
    : "";

  return '<div class="page">' +
    '<button type="button" class="btn btn-ghost btn-sm back-link" data-act="back">' +
    icon("arrow-left", "i-sm") + "全部数据源</button>" +
    stale +
    '<div class="detail-head"><div>' +
    '<h1 class="page-title">' + esc(ds.title) + "</h1>" +
    '<p class="mono page-sub">' + esc(ds.source_key) + "</p>" +
    '<div class="detail-flags">' + statusBadge(ds) + encryptFlag(ds) +
    '<span class="badge tone-neutral">' + esc(ds.default_locale) + "</span></div>" +
    "</div>" +
    (read.canWrite() ? '<div class="page-actions">' + cardMenu(ds, false) + "</div>" : "") +
    "</div>" +
    '<div style="margin-top:20px">' + (LAYOUT.optionsArea ? LAYOUT.optionsArea(ds) : optionsBlock(ds, false)) + "</div>" +
    "</div>";
}

/* -------------------------------- 菜单 ---------------------------------- */

function cardMenu(ds, compact) {
  const open = STATE.menuFor === ds.source_key;
  const isActive = ds.status === "active";
  return '<span class="menu-wrap' + (open ? " is-open" : "") + '">' +
    '<button type="button" class="btn btn-ghost btn-icon' + (compact ? " menu-btn-sm" : "") + '" ' +
    'aria-label="更多操作" aria-haspopup="menu" aria-expanded="' + open + '" ' +
    'data-act="toggle-menu" data-key="' + esc(ds.source_key) + '">' + icon("more-horizontal") + "</button>" +
    '<span class="menu" role="menu"' + (open ? "" : " hidden") + ">" +
    '<button type="button" class="menu-item" role="menuitem" data-act="open-rename" data-key="' + esc(ds.source_key) + '">' +
    icon("square-pen", "i-sm") + "重命名</button>" +
    '<button type="button" class="menu-item" role="menuitem" data-act="toggle-status" data-key="' + esc(ds.source_key) + '">' +
    icon(isActive ? "power" : "circle-check", "i-sm") + (isActive ? "停用" : "启用") + "</button>" +
    '<span class="menu-sep"></span>' +
    '<button type="button" class="menu-item is-danger" role="menuitem" data-act="open-delete" data-key="' + esc(ds.source_key) + '">' +
    icon("trash-2", "i-sm") + "删除</button>" +
    "</span></span>";
}

/* -------------------------------- 对话框 --------------------------------- */

function dialogHtml() {
  const dialog = STATE.dialog;
  if (!dialog) return "";

  if (dialog.kind === "add" || dialog.kind === "rename") {
    const isRename = dialog.kind === "rename";
    const ds = isRename ? read.byKey(dialog.key) : null;
    const titleValue = isRename && ds ? ds.title : "";
    return '<div class="overlay" data-act="overlay"><div class="dialog" role="dialog" aria-modal="true" ' +
      'aria-label="' + (isRename ? "重命名数据源" : "添加数据源") + '">' +
      '<button type="button" class="dialog-close" data-act="close-dialog" aria-label="关闭">' + icon("x") + "</button>" +
      '<div><h2 class="dialog-title">' + (isRename ? "重命名数据源" : "添加数据源") + "</h2>" +
      '<p class="dialog-desc">' + (isRename
        ? "只改展示名，标识与 Token 保持不变。"
        : "数据源标识会进接口 URL，且创建后不可修改。") + "</p></div>" +
      '<div class="field"><label for="dlg-key">数据源标识（1–64 字符，小写字母、数字与连字符）</label>' +
      '<input id="dlg-key" class="input mono" data-field="source_key" ' +
      (isRename ? 'value="' + esc(ds.source_key) + '" disabled' : 'placeholder="例如 expense-category"') +
      ' value="' + (isRename ? esc(ds.source_key) : "") + '"></div>' +
      '<div class="field"><label for="dlg-title">展示名（1–100 字符）</label>' +
      '<input id="dlg-title" class="input" data-field="title" value="' + esc(titleValue) + '" placeholder="例如 报销事由分类"></div>' +
      (isRename ? "" :
        '<div class="field"><label for="dlg-token">接口 Token</label>' +
        '<input id="dlg-token" class="input mono" type="password" data-field="token" autocomplete="new-password" placeholder="粘贴飞书审批后台里那个 Token">' +
        '<p class="field-hint">服务端只保存摘要，提交后无法回显。忘了就去飞书审批后台重设一个。</p></div>' +
        '<div class="field"><label for="dlg-locale">默认语言</label>' +
        '<select id="dlg-locale" class="input" data-field="default_locale">' +
        '<option value="zh-CN">简体中文（zh-CN）</option><option value="en-US">English（en-US）</option></select></div>') +
      (STATE.formError ? '<p class="field-error" role="alert">' + esc(STATE.formError) + "</p>" : "") +
      '<div class="dialog-foot">' +
      '<button type="button" class="btn btn-outline" data-act="close-dialog">取消</button>' +
      '<button type="button" class="btn btn-default" data-act="submit-' + dialog.kind + '"' +
      (STATE.busy ? " disabled" : "") + ">" + (STATE.busy ? "提交中…" : (isRename ? "保存" : "创建")) + "</button>" +
      "</div></div></div>";
  }

  if (dialog.kind === "delete") {
    const ds = read.byKey(dialog.key);
    if (!ds) return "";
    return '<div class="overlay" data-act="overlay"><div class="dialog" role="dialog" aria-modal="true" aria-label="删除数据源">' +
      '<button type="button" class="dialog-close" data-act="close-dialog" aria-label="关闭">' + icon("x") + "</button>" +
      '<div><h2 class="dialog-title">删除数据源</h2>' +
      '<p class="dialog-desc">删除「' + esc(ds.title) + '」（<span class="mono">' + esc(ds.source_key) + "</span>）。</p></div>" +
      '<div class="banner banner-error banner-row" role="alert">' + icon("triangle-alert") +
      "<div>删除后其下全部选项会被同时停用，且不可恢复。确认删除？</div></div>" +
      '<p class="field-hint">该数据源下现有 <strong>' + ds.option_count + "</strong> 个选项将被连带停用。</p>" +
      '<div class="dialog-foot">' +
      '<button type="button" class="btn btn-outline" data-act="close-dialog">取消</button>' +
      '<button type="button" class="btn btn-destructive" data-act="confirm-delete"' +
      (STATE.busy ? " disabled" : "") + ">" + (STATE.busy ? "删除中…" : "删除") + "</button>" +
      "</div></div></div>";
  }

  return "";
}

/* --------------------------------- 外壳 ---------------------------------- */

function shellHtml() {
  const ops = STATE.role === "ops";
  const isDetail = STATE.route.name === "detail" && STATE.route.key;
  const feishuActive = STATE.route.name === "list" || isDetail;
  return "" +
    '<aside class="sidebar">' +
    '<div class="brand"><span class="brand-mark">Y</span><span class="brand-name">YANG System 控制台</span></div>' +
    '<div class="identity"><button type="button" class="identity-btn" data-act="noop">' +
    icon("circle-user") +
    '<span class="identity-name">' + (ops ? "运维 · 集成负责人" : "业务 · 审批管理员") +
    '<span class="identity-role">' + (ops ? "可读可写" : "只读") + "</span></span>" +
    icon("chevrons-up-down", "i-sm") + "</button></div>" +
    '<nav class="nav"><div><p class="nav-label">个人</p><ul>' +
    '<li><a class="navlink" href="#" data-act="noop">' + icon("circle-user") + "账号设置</a></li>" +
    "</ul></div><div><p class=\"nav-label\">业务</p><ul>" +
    '<li><a class="navlink" href="#" data-act="go-list"' + (feishuActive ? ' aria-current="page"' : "") + ">" +
    icon("database") + "飞书数据源</a></li>" +
    '<li><a class="navlink" href="#" data-act="noop">' + icon("table-2") + "用户管理</a></li>" +
    '<li><a class="navlink" href="#" data-act="noop">' + icon("shield-check") + "授权与角色</a></li>" +
    "</ul></div></nav>" +
    '<div class="sidebar-foot"><button type="button" class="btn btn-ghost btn-sm btn-block" data-act="noop">' +
    icon("log-out") + "退出登录</button></div></aside>" +

    '<div class="col"><header class="topbar">' +
    '<div class="topbar-left">' + icon("database") + "<span>" + LAYOUT.name + "</span></div>" +
    densityMenu() +
    '<button type="button" class="btn btn-outline btn-icon" data-act="toggle-theme" aria-label="切换明暗主题">' +
    icon(isDark() ? "sun" : "moon") + "</button>" +
    "</header><main id=\"main\"></main></div>";
}

/// 系统是否偏好暗色。matchMedia 在极老的宿主里可能缺席，缺席时按浅色处理。
function prefersDark() {
  if (typeof window.matchMedia !== "function") return false;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

function isDark() {
  if (STATE.theme === "dark") return true;
  if (STATE.theme === "light") return false;
  return prefersDark();
}

const DENSITIES = [["compact", "紧凑"], ["standard", "标准"], ["loose", "宽松"]];

/// 密度下拉：对齐 AppLayout 的 DensityMenu，落到 html[data-density]。
function densityMenu() {
  const open = STATE.menuFor === "__density";
  const current = DENSITIES.filter(function (item) { return item[0] === STATE.density; })[0];
  return '<span class="menu-wrap' + (open ? " is-open" : "") + '">' +
    '<button type="button" class="btn btn-outline btn-sm" data-act="toggle-menu" data-key="__density" ' +
    'aria-haspopup="menu" aria-expanded="' + open + '">密度 · ' + (current ? current[1] : "标准") + "</button>" +
    '<span class="menu" role="menu"' + (open ? "" : " hidden") + ">" +
    DENSITIES.map(function (item) {
      return '<button type="button" class="menu-item" role="menuitem" data-act="set-density" data-value="' + item[0] + '">' +
        '<span style="flex:1">' + item[1] + "</span>" +
        (STATE.density === item[0] ? icon("check", "i-sm") : "") + "</button>";
    }).join("") + "</span></span>";
}

function mainHtml() {
  if (STATE.dataState === "loading") return skeletonHtml();
  if (STATE.dataState === "error") return errorHtml();
  if (STATE.dataState === "empty") return emptyHtml();
  if (LAYOUT.split) return LAYOUT.list();
  if (STATE.route.name === "detail") return LAYOUT.detail ? LAYOUT.detail() : detailHtml();
  return LAYOUT.list();
}

/* ------------------------------- 原型控制条 ------------------------------- */

function controlBarHtml() {
  function seg(name, options, current) {
    return '<span class="seg">' + options.map(function (option) {
      return '<button type="button" data-act="set" data-name="' + name + '" data-value="' + option[0] +
        '" aria-pressed="' + (String(current) === String(option[0])) + '">' + option[1] + "</button>";
    }).join("") + "</span>";
  }
  return '<div class="protoctl">' +
    '<span class="protoctl-label">原型 ' + LAYOUT.id + " · 原型控制</span>" +
    '<span class="protoctl-group"><span>数据</span>' +
    seg("dataState", [["data", "有数据"], ["empty", "空"], ["loading", "加载"], ["error", "错误"]], STATE.dataState) + "</span>" +
    '<span class="protoctl-group"><span>身份</span>' +
    seg("role", [["ops", "运维"], ["biz", "业务"]], STATE.role) + "</span>" +
    '<span class="protoctl-group"><span>主题</span>' +
    seg("theme", [["light", "明"], ["dark", "暗"]], isDark() ? "dark" : "light") + "</span>" +
    '<span class="protoctl-note">这一条是原型控制，不属于产品界面；产品里只有右上角的密度与明暗开关。</span>' +
    "</div>";
}

/* --------------------------------- 渲染 ---------------------------------- */

/// 重绘会换掉整棵 DOM，所以先记住焦点位置，重绘后放回去。
function captureFocus() {
  const el = document.activeElement;
  if (!el || !el.getAttribute) return null;
  const id = el.getAttribute("data-focus-id");
  if (!id) return null;
  let start = null;
  let end = null;
  try { start = el.selectionStart; end = el.selectionEnd; } catch (error) { start = null; }
  return { id: id, start: start, end: end };
}

function restoreFocus(snapshot) {
  if (!snapshot) return;
  const el = document.querySelector('[data-focus-id="' + snapshot.id + '"]');
  if (!el) return;
  el.focus();
  if (snapshot.start != null && el.setSelectionRange) {
    try { el.setSelectionRange(snapshot.start, snapshot.end); } catch (error) { /* 非文本控件 */ }
  }
}

function render() {
  const focus = captureFocus();

  if (STATE.theme === "dark" || STATE.theme === "light") {
    document.documentElement.setAttribute("data-theme", STATE.theme);
  } else {
    document.documentElement.removeAttribute("data-theme");
  }
  if (STATE.density === "standard") document.documentElement.removeAttribute("data-density");
  else document.documentElement.setAttribute("data-density", STATE.density);

  const root = document.getElementById("root");
  root.innerHTML = '<div class="app">' + shellHtml() + "</div>" +
    '<div id="dialog-root">' + dialogHtml() + "</div>" +
    (STATE.toast ? '<div class="toast" role="status">' + icon("circle-check", "i-sm") + esc(STATE.toast) + "</div>" : "") +
    controlBarHtml();

  document.getElementById("main").innerHTML = mainHtml();
  restoreFocus(focus);
}

/* -------------------------------- 交互 ---------------------------------- */

function setRoute(name, key) {
  STATE.route = { name: name, key: key || null };
  STATE.menuFor = null;
  const hash = name === "detail" ? "#/d/" + key : "#/";
  if (window.location.hash !== hash) window.location.hash = hash;
  render();
}

function parseHash() {
  const raw = window.location.hash.replace(/^#\/?/, "");
  if (raw.indexOf("d/") === 0) {
    const key = decodeURIComponent(raw.slice(2));
    if (read.byKey(key)) { STATE.route = { name: "detail", key: key }; return; }
  }
  STATE.route = { name: "list", key: null };
}

/// 读取当前生效的字段：对话框优先，其次是主区里的内联表单（方案 D 的向导第 2 步）。
function readForm() {
  const form = { source_key: "", title: "", token: "", default_locale: "zh-CN" };
  const scope = document.querySelector("#dialog-root [data-field]") ? "#dialog-root" : "#main";
  const nodes = document.querySelectorAll(scope + " [data-field]");
  for (let i = 0; i < nodes.length; i += 1) {
    form[nodes[i].getAttribute("data-field")] = nodes[i].value;
  }
  return form;
}

/// 提交成功返回 true，校验失败返回 false（错误写进 STATE.formError）。
function submitForm(kind) {
  const form = readForm();
  STATE.formError = "";

  if (kind === "add") {
    if (!/^[a-z0-9][a-z0-9-]{0,63}$/.test(form.source_key)) {
      STATE.formError = "数据源标识只能用小写字母、数字与连字符，且以字母或数字开头。";
      render(); return false;
    }
    if (read.byKey(form.source_key)) {
      STATE.formError = "这个数据源标识已经被占用了。";
      render(); return false;
    }
  }
  if (kind === "rename") {
    const ds = read.byKey(STATE.dialog.key);
    if (!ds) { STATE.dialog = null; render(); return false; }
    if (!form.title.trim() || form.title.trim().length > 100) {
      STATE.formError = "title 长度必须在 1..=100";
      render(); return false;
    }
    ds.title = form.title.trim();
    STATE.dialog = null;
    toast("已重命名");
    return true;
  }
  if (!form.title.trim() || form.title.trim().length > 100) {
    STATE.formError = "title 长度必须在 1..=100";
    render(); return false;
  }
  if (!form.token) {
    STATE.formError = "Token 不能为空；不轮换请省略该字段";
    render(); return false;
  }
  DATASOURCES.unshift({
    source_key: form.source_key, title: form.title.trim(), status: "active",
    encrypt_enabled: false, default_locale: form.default_locale,
    option_count: 0, updated_at: "刚刚",
  });
  OPTIONS_BY_KEY[form.source_key] = [];
  STATE.dialog = null;
  toast("已创建");
  return true;
}

/// 搜索框等「边打边筛」控件：只重绘 #results 区域，输入焦点与光标不丢。
document.addEventListener("input", function (event) {
  const el = event.target;
  if (!el || !el.getAttribute) return;
  const live = el.getAttribute("data-live");
  if (!live) return;
  if (live === "q") STATE.query.q = el.value;
  if (LAYOUT.onLive) LAYOUT.onLive(live, el.value);
  const region = document.getElementById("results");
  if (region && LAYOUT.results) region.innerHTML = LAYOUT.results();
});

document.addEventListener("click", function (event) {
  const trigger = event.target.closest ? event.target.closest("[data-act]") : null;
  if (!trigger) {
    if (STATE.menuFor) { STATE.menuFor = null; render(); }
    return;
  }
  const act = trigger.getAttribute("data-act");
  const key = trigger.getAttribute("data-key");

  if (act === "overlay") {
    if (event.target === trigger) { STATE.dialog = null; STATE.formError = ""; render(); }
    return;
  }
  if (act === "noop") { event.preventDefault(); return; }

  switch (act) {
    case "set": {
      const name = trigger.getAttribute("data-name");
      const value = trigger.getAttribute("data-value");
      if (name === "theme") STATE.theme = value;
      else STATE[name] = value;
      if (name === "role" && STATE.role === "biz") STATE.menuFor = null;
      if (name === "dataState" && value === "empty") STATE.route = { name: "list", key: null };
      if (name === "dataState" && value === "data") STATE.guideOpen = false;
      render();
      return;
    }
    case "toggle-theme":
      STATE.theme = isDark() ? "light" : "dark";
      render();
      return;
    case "go-list":
      event.preventDefault();
      setRoute("list", null);
      return;
    case "toggle-guide":
      STATE.guideOpen = !STATE.guideOpen;
      render();
      return;
    case "retry":
      STATE.dataState = "data";
      render();
      return;
    case "toggle-menu": {
      event.stopPropagation();
      STATE.menuFor = STATE.menuFor === key ? null : key;
      render();
      return;
    }
    case "open-add":
      STATE.dialog = { kind: "add" };
      STATE.formError = "";
      render();
      return;
    case "open-rename":
      STATE.dialog = { kind: "rename", key: key };
      STATE.formError = "";
      STATE.menuFor = null;
      render();
      return;
    case "open-delete":
      STATE.dialog = { kind: "delete", key: key };
      STATE.menuFor = null;
      render();
      return;
    case "close-dialog":
      STATE.dialog = null;
      STATE.formError = "";
      render();
      return;
    case "submit-add":
      submitForm("add");
      return;
    case "submit-rename":
      submitForm("rename");
      return;
    case "confirm-delete": {
      const ds = read.byKey(key || (STATE.dialog && STATE.dialog.key));
      if (ds) {
        const at = DATASOURCES.indexOf(ds);
        if (at >= 0) DATASOURCES.splice(at, 1);
        delete OPTIONS_BY_KEY[ds.source_key];
      }
      STATE.dialog = null;
      if (STATE.route.key === (ds && ds.source_key)) STATE.route = { name: "list", key: null };
      toast("已删除");
      if (DATASOURCES.length === 0) STATE.dataState = "empty";
      render();
      return;
    }
    case "toggle-status": {
      const ds = read.byKey(key);
      if (ds) {
        ds.status = ds.status === "active" ? "disabled" : "active";
        toast(ds.status === "active" ? "已启用" : "已停用");
      }
      STATE.menuFor = null;
      return;
    }
    case "open-detail":
      setRoute("detail", key);
      return;
    case "set-density":
      STATE.density = trigger.getAttribute("data-value");
      STATE.menuFor = null;
      render();
      return;
    case "filter-status":
      STATE.query.status = trigger.getAttribute("data-value");
      render();
      return;
    case "select-ds":
      // 分栏布局里选中即就地换右栏，不切路由（窄屏时由 CSS 决定是否整屏显示）。
      STATE.route = LAYOUT.split ? { name: "list", key: key } : { name: "detail", key: key };
      STATE.menuFor = null;
      render();
      return;
    case "clear-selection":
      STATE.route = { name: "list", key: null };
      render();
      return;
    case "back":
      setRoute("list", null);
      return;
    default:
      if (LAYOUT.onAction) LAYOUT.onAction(act, key, trigger, event);
  }
});

document.addEventListener("keydown", function (event) {
  if (event.key === "Escape") {
    if (STATE.dialog) { STATE.dialog = null; STATE.formError = ""; render(); return; }
    if (STATE.menuFor) { STATE.menuFor = null; render(); return; }
    return;
  }
  // 让整行/整卡可点：把 Enter 转发成一次 click（表单控件自身不拦）。
  if (event.key !== "Enter") return;
  const tag = event.target && event.target.tagName;
  if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA" || tag === "BUTTON") return;
  const hit = event.target.closest ? event.target.closest("[data-act]") : null;
  if (!hit) return;
  event.preventDefault();
  hit.click();
});

window.addEventListener("hashchange", function () {
  parseHash();
  render();
});

/* --------------------------------- 启动 ---------------------------------- */

parseHash();
STATE.guideOpen = STATE.dataState === "empty";
render();
