/* 方案 A · 卡片画廊
   论点：一个数据源是一个有边界的「东西」，值得占一块地方。 */

const LAYOUT = {
  id: "A",
  name: "飞书数据源",

  list: function () {
    const rows = read.filtered();
    const total = read.all().length;
    return '<div class="page">' +
      pageHeader(
        "飞书数据源",
        total
          ? "共 " + total + " 个数据源，其中 " + read.activeCount() + " 个启用；点卡片查看其中的选项"
          : "每个数据源对应飞书审批里一个「外部选项」控件",
        addButton()
      ) +
      guideHtml() +
      (rows.length ? '<div class="tiles">' + rows.map(tile).join("") + "</div>" : noMatch()) +
      "</div>";
  },

  detail: function () {
    return detailHtml();
  },
};

function tile(ds) {
  const canWrite = read.canWrite();
  return '<article class="tile">' +
    '<button type="button" class="tile-hit" data-act="open-detail" data-key="' + esc(ds.source_key) +
    '" aria-label="打开 ' + esc(ds.title) + '"></button>' +
    '<div class="tile-body">' +
    '<div class="tile-head"><span class="tile-title">' + esc(ds.title) + "</span>" +
    (canWrite ? cardMenu(ds, true) : "") + "</div>" +
    '<span class="tile-key mono">' + esc(ds.source_key) + "</span>" +
    '<div class="tile-badges">' + statusBadge(ds) + encryptFlag(ds) + "</div>" +
    '<div class="tile-meta">' +
    "<span>" + esc(ds.default_locale) + "</span>" +
    '<span class="tile-sep">·</span>' +
    '<span class="num">' + ds.option_count + " 个选项</span>" +
    "</div>" +
    (canWrite ? "" : '<div class="tile-hint">查看选项' + icon("chevron-right", "i-sm") + "</div>") +
    "</div></article>";
}

function noMatch() {
  return '<div class="empty">' +
    '<span class="empty-icon">' + icon("search") + "</span>" +
    "<h2>没有匹配的数据源</h2>" +
    "<p>换个名称或标识再试；也可以清空筛选看全部。</p>" +
    "</div>";
}
