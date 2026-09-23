/* 方案 D · 向导优先
   论点：这个控制台 90% 的使用价值发生在第一次配置上。
   与其在页面顶部贴一段可折叠的说明，不如把说明变成流程本身。 */

const WIZARD_STEPS = ["飞书审批", "建数据源", "填回后台", "配推送"];

const LAYOUT = {
  id: "D",
  name: "飞书数据源 · 向导",

  // 空态就是向导第 1 步；但用户主动「跳过向导」之后不再把人拽回向导。
  empty: function () {
    return STATE.wizardDismissed ? baseEmptyHtml() : wizardHtml();
  },

  list: function () {
    if (STATE.wizardActive) return wizardHtml();
    const total = read.all().length;
    return '<div class="page">' +
      pageHeader(
        "飞书数据源",
        "共 " + total + " 个数据源，其中 " + read.activeCount() + " 个启用",
        (read.canWrite()
          ? '<button type="button" class="btn btn-outline" data-act="open-wizard">' +
            icon("list") + "配置向导</button>"
          : "") + addButton()
      ) +
      guideHtml() +
      '<div id="results">' + LAYOUT.results() + "</div>" +
      "</div>";
  },

  results: function () {
    const rows = read.filtered();
    if (!rows.length) {
      return '<div class="empty" style="margin-top:16px">' +
        '<span class="empty-icon">' + icon("search") + "</span>" +
        "<h2>没有匹配的数据源</h2><p>清空搜索试试。</p></div>";
    }
    return '<div class="ledger-wrap"><table class="ledger">' +
      "<thead><tr><th>名称</th><th>标识</th><th>状态</th><th>默认语言</th><th>选项</th>" +
      (read.canWrite() ? '<th><span class="sr-only">操作</span></th>' : "") +
      "</tr></thead><tbody>" +
      rows.map(function (ds) {
        return '<tr class="ledger-row" data-act="open-detail" data-key="' + esc(ds.source_key) + '" tabindex="0">' +
          '<td class="ledger-name">' + esc(ds.title) + "</td>" +
          '<td class="mono muted">' + esc(ds.source_key) + "</td>" +
          "<td>" + statusBadge(ds) + "</td>" +
          "<td>" + esc(ds.default_locale) + "</td>" +
          '<td class="num">' + ds.option_count + "</td>" +
          (read.canWrite() ? '<td class="ledger-act">' + cardMenu(ds, true) + "</td>" : "") +
          "</tr>";
      }).join("") +
      "</tbody></table></div>";
  },

  detail: function () {
    return detailHtml();
  },

  onAction: function (act, key, trigger) {
    if (act === "open-wizard") {
      STATE.wizardActive = true;
      STATE.wizardDismissed = false;
      STATE.wizardStep = 1;
      STATE.formError = "";
      render();
      return true;
    }
    if (act === "wiz-goto") { STATE.wizardStep = Number(trigger.getAttribute("data-step")); render(); return true; }
    if (act === "wiz-prev") { STATE.wizardStep = Math.max(1, STATE.wizardStep - 1); STATE.formError = ""; render(); return true; }
    if (act === "wiz-next") { STATE.wizardStep = Math.min(4, STATE.wizardStep + 1); STATE.formError = ""; render(); return true; }
    if (act === "wiz-skip") {
      // 退出向导：已经建出数据源就直接进台账，否则落到普通空状态。
      STATE.wizardActive = false;
      STATE.wizardDismissed = true;
      STATE.guideOpen = false;
      if (DATASOURCES.length) STATE.dataState = "data";
      render();
      return true;
    }
    if (act === "wiz-copy") { toast("已复制请求头片段"); return true; }
    if (act === "wiz-create") {
      // 建成功后向导就地前进到第 3 步，而不是跳去详情——这一步才刚把数据源建出来。
      if (submitForm("add")) {
        STATE.wizardActive = true;
        STATE.wizardStep = 3;
        render();
      }
      return true;
    }
    return false;
  },
};

/* -------------------------------- 步骤条 --------------------------------- */

function wizardHtml() {
  const step = STATE.wizardStep;
  const trail = WIZARD_STEPS.map(function (label, index) {
    const number = index + 1;
    const state = number < step ? "done" : number === step ? "current" : "todo";
    return '<li class="wstep" data-state="' + state + '">' +
      '<button type="button" class="wstep-btn" data-act="wiz-goto" data-step="' + number + '" ' +
      'aria-current="' + (number === step ? "step" : "false") + '">' +
      '<span class="wstep-num num">' + (state === "done" ? icon("check", "i-sm") : number) + "</span>" +
      '<span class="wstep-label">' + label + "</span></button></li>";
  }).join('<li class="wstep-line" aria-hidden="true"></li>');

  return '<div class="page page-narrow">' +
    pageHeader("飞书数据源", "第一次配置大约需要 10 分钟，跟着走完就能用", "") +
    '<ol class="wtrail">' + trail + "</ol>" +
    '<div class="wcard">' + stepBody(step) + "</div>" +
    '<div class="wfoot"><span class="muted">已经配过数据源了？</span>' +
    '<button type="button" class="btn btn-link" data-act="wiz-skip">跳过向导，去看台账</button></div>' +
    "</div>";
}

function stepBody(step) {
  if (step === 1) {
    return '<div class="wbody">' +
      '<h2 class="wtitle">第 1 步 · 在飞书审批后台配置控件</h2>' +
      '<p class="wtext">打开你要用的那个审批定义，找到单选或多选控件，' +
      '把取值方式改成「使用外部选项」，然后自定义一个 Token。</p>' +
      '<div class="wbanner">' + icon("triangle-alert") +
      "<div>这个 Token 只在飞书那边显示一次。服务端只保存它的摘要，之后无法回显——" +
      "请先复制到安全的地方。</div></div>" +
      '<dl class="wspec">' +
      "<dt>控件类型</dt><dd>单选 / 多选</dd>" +
      '<dt>取值方式</dt><dd>使用外部选项<span class="muted"> ← 不是「自定义选项」</span></dd>' +
      "<dt>自定义 Token</dt><dd>随便起，记住它</dd>" +
      "</dl>" +
      '<div class="wactions"><button type="button" class="btn btn-default" data-act="wiz-next">' +
      "我配好了，下一步" + icon("chevron-right") + "</button></div></div>";
  }

  if (step === 2) {
    return '<div class="wbody">' +
      '<h2 class="wtitle">第 2 步 · 在这里建一个数据源</h2>' +
      '<p class="wtext">数据源标识会进接口 URL，创建后不可修改；把上一步拿到的 Token 粘进来。</p>' +
      '<div class="wform">' +
      '<div class="field"><label for="wz-key">数据源标识（1–64 字符）</label>' +
      '<input id="wz-key" class="input mono" data-field="source_key" data-focus-id="wz-key" ' +
      'placeholder="例如 expense-category"></div>' +
      '<div class="field"><label for="wz-title">展示名</label>' +
      '<input id="wz-title" class="input" data-field="title" data-focus-id="wz-title" ' +
      'placeholder="例如 报销事由分类"></div>' +
      '<div class="field"><label for="wz-token">接口 Token</label>' +
      '<input id="wz-token" class="input mono" type="password" data-field="token" ' +
      'autocomplete="new-password" placeholder="粘贴第 1 步那个 Token">' +
      '<p class="field-hint">服务端只保存摘要，提交后无法回显——忘了就回飞书重设一个。</p></div>' +
      "</div>" +
      (STATE.formError ? '<p class="field-error" role="alert">' + esc(STATE.formError) + "</p>" : "") +
      '<div class="wactions">' +
      '<button type="button" class="btn btn-ghost" data-act="wiz-prev">' + icon("chevron-left") + "上一步</button>" +
      '<button type="button" class="btn btn-default" data-act="wiz-create"' + (STATE.busy ? " disabled" : "") + ">" +
      (STATE.busy ? "创建中…" : "创建并继续") + '</button></div></div>';
  }

  if (step === 3) {
    return '<div class="wbody">' +
      '<h2 class="wtitle">第 3 步 · 把接口地址与 Token 填回审批后台</h2>' +
      '<p class="wtext">回到飞书审批那个控件，把外部选项的接口地址与 Token 填上，' +
      "然后点那边的「校验数据」确认能拉到选项。</p>" +
      '<ul class="wcheck">' +
      '<li>' + icon("check", "i-sm") + "接口地址已填进控件的「外部选项接口」</li>" +
      '<li>' + icon("check", "i-sm") + "Token 与第 1 步自定义的完全一致</li>" +
      '<li>' + icon("check", "i-sm") + "控件里能搜到你在台账里看到的选项名</li>" +
      "</ul>" +
      '<div class="wbanner muted-banner">' + icon("info") +
      "<div>控制台<b>没有</b>「校验数据」按钮：服务端不保存 Token 明文，" +
      "无法代替飞书发起这次校验。这一步只能在飞书那边做。</div></div>" +
      '<div class="wactions">' +
      '<button type="button" class="btn btn-ghost" data-act="wiz-prev">' + icon("chevron-left") + "上一步</button>" +
      '<button type="button" class="btn btn-default" data-act="wiz-next">下一步' + icon("chevron-right") + "</button>" +
      "</div></div>";
  }

  return '<div class="wbody">' +
    '<h2 class="wtitle">第 4 步 · 在多维表格配自动化推送选项</h2>' +
    '<p class="wtext">在选项所在的多维表格里加一条自动化：当记录变化时，' +
    "用 HTTP 节点把选项推给控制台。选项从此随表格变动自动更新。</p>" +
    '<dl class="wspec">' +
    "<dt>触发条件</dt><dd>记录新增或字段变化</dd>" +
    "<dt>请求方式</dt><dd>POST</dd>" +
    "<dt>请求头</dt><dd><code class=\"mono wcode\">Authorization: Bearer &lt;管理 Token&gt;</code>" +
    '<button type="button" class="btn btn-ghost btn-sm wcopy" data-act="wiz-copy">' +
    icon("copy", "i-sm") + "复制</button></dd>" +
    "</dl>" +
    '<div class="wbanner muted-banner">' + icon("info") +
    "<div>管理 Token 由运维在服务端配置，和上面那个数据源 Token 不是一回事，" +
    "不要混用。</div></div>" +
    '<div class="wactions">' +
    '<button type="button" class="btn btn-ghost" data-act="wiz-prev">' + icon("chevron-left") + "上一步</button>" +
    (DATASOURCES.length
      ? '<button type="button" class="btn btn-default" data-act="wiz-skip">完成，去看台账</button>'
      : '<button type="button" class="btn btn-outline" data-act="wiz-goto" data-step="2">先回去建数据源</button>') +
    "</div></div>";
}
