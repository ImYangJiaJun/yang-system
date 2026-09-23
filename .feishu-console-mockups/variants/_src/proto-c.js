/* 方案 C · 主从分栏
   论点：看数据源不是终点，看它下面有哪些选项才是。详情是高频动作，
   就把它和列表放在同一屏，省掉一次导航。 */

const LAYOUT = {
  id: "C",
  name: "飞书数据源 · 分栏",
  split: true,

  list: function () {
    const rows = read.filtered();
    const total = read.all().length;
    const ds = read.selected();
    const showingDetail = Boolean(ds);

    return '<div class="page page-wide">' +
      pageHeader(
        "飞书数据源",
        total ? "共 " + total + " 个数据源；在左边选一个，右边看它的选项" : "每个数据源对应飞书审批里一个「外部选项」控件",
        ""
      ) +
      '<div class="split" data-narrow-detail="' + showingDetail + '">' +
      '<aside class="split-left">' +
      '<div class="split-left-head"><span>数据源</span><span class="muted num">' + rows.length + "</span></div>" +
      '<span class="search-box split-search">' + icon("search", "i-sm") +
      '<input class="search-input" type="search" data-live="q" data-focus-id="q" ' +
      'value="' + esc(STATE.query.q) + '" placeholder="搜索" aria-label="搜索数据源"></span>' +
      '<ul class="split-list">' +
      (rows.length ? rows.map(splitItem).join("") : '<li class="split-none muted">没有匹配的数据源</li>') +
      "</ul>" +
      (read.canWrite()
        ? '<div class="split-left-foot"><button type="button" class="btn btn-outline btn-sm btn-block" data-act="open-add">' +
          icon("plus") + "添加数据源</button></div>"
        : "") +
      "</aside>" +
      '<section class="split-right">' + rightPane(ds) + "</section>" +
      "</div></div>";
  },

  detail: function () {
    return LAYOUT.list();
  },

  // 分栏右栏用不带外框的选项区，避免「卡中卡」
  optionsArea: function (ds) {
    return optionsBlock(ds, false);
  },

  empty: function () {
    return '<div class="page page-wide">' +
      pageHeader("飞书数据源", "每个数据源对应飞书审批里一个「外部选项」控件", "") +
      '<div class="split">' +
      '<aside class="split-left">' +
      '<div class="split-left-head"><span>数据源</span><span class="muted num">0</span></div>' +
      '<div class="split-empty">' +
      '<p class="muted" style="font-size:14px">还没有数据源。</p>' +
      (read.canWrite()
        ? '<button type="button" class="btn btn-default btn-sm" data-act="open-add" style="margin-top:8px">' +
          icon("plus") + "添加数据源</button>"
        : "") +
      "</div></aside>" +
      '<section class="split-right"><div class="split-guide">' +
      guideHtml({ forceOpen: true }) +
      "</div></section>" +
      "</div></div>";
  },
};

function splitItem(ds) {
  const selected = STATE.route.key === ds.source_key;
  return '<li><button type="button" class="split-item" data-act="select-ds" data-key="' +
    esc(ds.source_key) + '" aria-current="' + (selected ? "true" : "false") + '">' +
    '<span class="split-item-main">' +
    '<span class="split-item-title truncate">' + esc(ds.title) +
    (ds.status === "disabled" ? '<span class="split-off" title="已停用">' + icon("power", "i-sm") + "</span>" : "") +
    "</span>" +
    '<span class="split-item-key mono truncate">' + esc(ds.source_key) + "</span>" +
    "</span>" +
    '<span class="badge tone-neutral num">' + ds.option_count + "</span>" +
    "</button></li>";
}

function rightPane(ds) {
  if (!ds) {
    return '<div class="split-placeholder">' +
      '<span class="empty-icon">' + icon("database") + "</span>" +
      "<h2>从左边选一个数据源</h2>" +
      "<p>这里会列出它的选项；选项由飞书多维表格自动推送，控制台只读。</p></div>";
  }
  const stale = ds.status === "disabled"
    ? '<div class="banner" style="margin-bottom:16px">' + icon("info", "i-sm") +
      " 这个数据源已停用，飞书审批取选项会失败；下面的选项数据仍然保留。</div>"
    : "";
  return '<div class="split-right-inner">' +
    '<button type="button" class="btn btn-ghost btn-sm narrow-back" data-act="clear-selection">' +
    icon("arrow-left", "i-sm") + "返回列表</button>" +
    stale +
    '<div class="detail-head"><div>' +
    '<h2 class="page-title">' + esc(ds.title) + "</h2>" +
    '<p class="mono page-sub">' + esc(ds.source_key) + "</p>" +
    '<div class="detail-flags">' + statusBadge(ds) + encryptFlag(ds) +
    '<span class="badge tone-neutral">' + esc(ds.default_locale) + "</span></div>" +
    "</div>" +
    (read.canWrite() ? '<div class="page-actions">' + cardMenu(ds, false) + "</div>" : "") +
    "</div>" +
    '<div style="margin-top:20px">' + optionsBlock(ds, false) + "</div>" +
    "</div>";
}
