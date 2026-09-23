/* 方案 B · 紧凑目录
   论点：日常工作不是「管理很多数据源」，而是「确认某个数据源对不对」。
   那就让所有数据源的状态在一屏里排完，像台账一样可扫。 */

const LAYOUT = {
  id: "B",
  name: "飞书数据源 · 台账",

  list: function () {
    const total = read.all().length;
    if (!total) {
      return '<div class="page">' +
        pageHeader("飞书数据源", "每个数据源对应飞书审批里一个「外部选项」控件", addButton()) +
        guideHtml({ forceOpen: true }) +
        '<div style="margin-top:16px"><div class="empty">' +
        '<span class="empty-icon">' + icon("database") + "</span>" +
        "<h2>还没有数据源</h2>" +
        "<p>先在飞书审批后台配好控件并拿到 Token，再回来建第一个。</p>" +
        '<div class="empty-actions">' +
        (read.canWrite() ? '<button type="button" class="btn btn-default" data-act="open-add">' + icon("plus") + "添加数据源</button>" : "") +
        '<button type="button" class="btn btn-outline" data-act="toggle-guide">怎么拿到 Token？</button>' +
        "</div></div></div></div>";
    }

    return '<div class="page">' +
      pageHeader(
        "飞书数据源",
        "共 " + total + " 个数据源，其中 " + read.activeCount() + " 个启用",
        addButton()
      ) +
      guideHtml() +
      toolbar() +
      '<div id="results">' + LAYOUT.results() + "</div>" +
      "</div>";
  },

  // 搜索/筛选变化时只重绘这一块
  results: function () {
    const rows = read.filtered();
    if (!rows.length) {
      return '<div class="empty" style="margin-top:16px">' +
        '<span class="empty-icon">' + icon("search") + "</span>" +
        "<h2>没有匹配的数据源</h2><p>换个名称或标识再试，或把状态筛选切回「全部」。</p></div>";
    }
    return '<div class="ledger-wrap"><table class="ledger">' +
      "<thead><tr>" +
      "<th>名称</th><th>标识</th><th>状态</th><th>加密</th><th>默认语言</th><th>选项</th>" +
      (read.canWrite() ? '<th><span class="sr-only">操作</span></th>' : "") +
      "</tr></thead><tbody>" +
      rows.map(function (ds) { return row(ds); }).join("") +
      "</tbody></table></div>" +
      '<div class="ledger-foot">' +
      '<span class="muted num">共 ' + rows.length + " 个数据源</span>" +
      '<span class="muted num">第 1 / 1 页</span>' +
      "</div>";
  },

  detail: function () {
    return detailHtml();
  },
};

function toolbar() {
  const statuses = [["all", "全部"], ["active", "启用"], ["disabled", "停用"]];
  const canWrite = read.canWrite();
  return '<div class="ledger-bar">' +
    '<span class="search-box">' + icon("search", "i-sm") +
    '<input class="search-input" type="search" data-live="q" data-focus-id="q" ' +
    'value="' + esc(STATE.query.q) + '" placeholder="搜索名称或标识" aria-label="搜索数据源"></span>' +
    '<span class="seg"' + (canWrite ? "" : ' style="margin-left:0"') + ">" +
    statuses.map(function (item) {
      return '<button type="button" data-act="filter-status" data-value="' + item[0] + '" ' +
        'data-focus-id="st-' + item[0] + '" aria-pressed="' + (STATE.query.status === item[0]) + '">' +
        item[1] + "</button>";
    }).join("") + "</span>" +
    (canWrite
      ? '<button type="button" class="btn btn-link ledger-guide" data-act="toggle-guide">首次使用？</button>'
      : "") +
    "</div>";
}

function row(ds) {
  const canWrite = read.canWrite();
  return '<tr class="ledger-row" data-act="open-detail" data-key="' + esc(ds.source_key) + '" tabindex="0">' +
    '<td class="ledger-name">' + esc(ds.title) + "</td>" +
    '<td class="mono muted">' + esc(ds.source_key) + "</td>" +
    "<td>" + statusBadge(ds) + "</td>" +
    "<td>" + (ds.encrypt_enabled ? '<span class="badge tone-info">' + icon("lock", "i-sm") + "加密</span>" : '<span class="muted">—</span>') + "</td>" +
    "<td>" + esc(ds.default_locale) + "</td>" +
    '<td class="num">' + ds.option_count + "</td>" +
    (canWrite ? '<td class="ledger-act">' + cardMenu(ds, true) + "</td>" : "") +
    "</tr>";
}
