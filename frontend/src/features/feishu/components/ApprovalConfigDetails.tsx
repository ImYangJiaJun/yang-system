import { useRef, useState } from "react";
import { ArrowRight, ExternalLink } from "lucide-react";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { copyText } from "@/shared/lib/clipboard";
import { useToast } from "@/shared/lib/toast";
import { useApprovalActions, useApprovalConfigList } from "../api";
import { ListPagination } from "./ListPagination";
import { StatusBadge } from "./StatusBadge";
import { formatUnixSeconds, type ApprovalConfigItem } from "../types";

const WIDGET_LABELS: Record<string, string> = {
  input: "文本",
  textarea: "多行文本",
  number: "数字",
  amount: "金额",
  date: "日期",
  dateInterval: "日期区间",
  radio: "单选",
  radioV2: "单选",
  checkbox: "多选",
  checkboxV2: "多选",
  contact: "人员",
  department: "部门",
  attachment: "附件",
  telephone: "电话",
  address: "地址",
  fieldList: "明细",
};
const RULE_LABELS: Record<string, string> = {
  direct: "直接填写",
  date: "按表格时区转换日期",
  option: "按名称匹配审批选项",
};
const nameOf = (name: string | null | undefined, fallback: string) =>
  name?.trim() || fallback;

/// 「接入数据」tab 里的两份请求体模板：`token` 占位符会被真实访问 Token 替换
/// （若已签发），其余占位符需在飞书工作流里换成动态变量。`aria-label` 与
/// `tests/.../approval-configs-page.test.tsx` 的断言同源。
const SINGLE_TEMPLATE = {
  token: "<访问 Token>",
  record_id: "<触发记录 ID>",
  applicant: "<发起人姓名>",
  requested_by: "<触发人标识>",
} as const;
const BATCH_TEMPLATE = {
  token: "<访问 Token>",
  applicant: "<发起人姓名>",
  requested_by: "<触发人标识>",
} as const;

export function ApprovalConfigDetails({
  item,
  canWrite,
  pending,
  onEdit,
  onToggle,
  onDelete,
  onRefreshNames,
  onOpenConfig,
  onRequests,
}: {
  item: ApprovalConfigItem;
  canWrite: boolean;
  pending: boolean;
  onEdit: () => void;
  onToggle: () => void;
  onDelete: () => void;
  onRefreshNames: () => void;
  onOpenConfig: (id: number) => void;
  onRequests: () => void;
}) {
  const [tab, setTab] = useState("maps");
  const [search, setSearch] = useState("");
  const [relatedOpen, setRelatedOpen] = useState(false);
  const [relatedPage, setRelatedPage] = useState(1);
  const [relatedSize, setRelatedSize] = useState(10);
  const [rotatingToken, setRotatingToken] = useState(false);
  const [revealingToken, setRevealingToken] = useState(false);
  /// 剪贴板写不进去时的退路：Token 明文只在这一次响应里出现，复制不成
  /// 就必须把它就地摆出来（照抄 CredentialChecklist 的 copyFailure 形态）。
  const [tokenCopyFailure, setTokenCopyFailure] = useState<string | null>(null);
  const refs = useRef(new Map<string, HTMLButtonElement>());
  const toast = useToast();
  const actions = useApprovalActions();
  const related = useApprovalConfigList(
    {
      page: relatedPage,
      pageSize: relatedSize,
      search: "",
      approvalCode: item.approvalCode,
    },
    { enabled: relatedOpen },
  );
  const keyword = search.trim().toLowerCase();
  const maps = item.maps.filter((map) =>
    `${map.bitableFieldName ?? ""} ${map.widgetName ?? ""}`
      .toLowerCase()
      .includes(keyword),
  );
  const tabs = [
    { id: "maps", label: `字段对应（${item.maps.length}）` },
    { id: "access", label: "接入数据" },
    { id: "usage", label: "使用说明" },
    { id: "debug", label: "排查信息" },
  ];
  const validCoordinate = (value: string) => /^[a-zA-Z0-9_-]+$/.test(value);
  const baseUrl =
    validCoordinate(item.baseToken) && validCoordinate(item.tableId)
      ? `https://www.feishu.cn/base/${encodeURIComponent(item.baseToken)}?table=${encodeURIComponent(item.tableId)}`
      : undefined;
  const approvalUrl = validCoordinate(item.approvalCode)
    ? `https://www.feishu.cn/approval/admin/createApproval?definitionCode=${encodeURIComponent(item.approvalCode)}`
    : undefined;
  const dispatchUrl = item.configKey
    ? `${window.location.origin}/api/v1/feishu/approval/dispatch/${item.configKey}`
    : undefined;
  const singleBody = JSON.stringify(SINGLE_TEMPLATE, null, 2);
  const batchBody = JSON.stringify(BATCH_TEMPLATE, null, 2);
  async function copy(label: string, value: string) {
    const outcome = await copyText(value);
    if (outcome === "copied") toast.success(`${label}已复制`);
    else toast.error("无法自动复制，请选中文字手动复制。");
  }
  async function rotateToken() {
    setRotatingToken(true);
    setTokenCopyFailure(null);
    try {
      const token = await actions.rotateConfigToken(item.id);
      if (!token) {
        toast.error("轮换失败：服务端没有返回新的访问 Token。");
        return;
      }
      // 与 datasource 的「复制即取回」同一条纪律：明文只在这里出现一次，
      // 刷新即失，绝不进本地长期状态或日志。
      const outcome = await copyText(token);
      if (outcome === "copied") {
        toast.success(
          "已轮换并复制新 Token；旧 Token 立即失效，请把新值粘回飞书工作流。",
        );
      } else {
        setTokenCopyFailure(token);
        toast.error(
          "已轮换，但无法自动复制。新 Token 已显示在页面上，请选中后手动复制。",
        );
      }
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      toast.error(`轮换访问 Token 失败：${message}`);
    } finally {
      setRotatingToken(false);
    }
  }
  /// 回显**当前**访问 Token（纯读，可反复调用）。与轮换分开：轮换是破坏性的，
  /// 不该被当成「看一眼 Token」的入口。
  async function revealToken() {
    setRevealingToken(true);
    setTokenCopyFailure(null);
    try {
      const token = await actions.revealConfigToken(item.id);
      if (!token) {
        toast.error("回显失败：服务端没有返回访问 Token。");
        return;
      }
      const outcome = await copyText(token);
      if (outcome === "copied") {
        toast.success("已复制当前访问 Token。");
      } else {
        setTokenCopyFailure(token);
        toast.error(
          "无法自动复制。当前 Token 已显示在页面上，请选中后手动复制。",
        );
      }
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      toast.error(`回显访问 Token 失败：${message}`);
    } finally {
      setRevealingToken(false);
    }
  }
  return (
    <>
      <header className="flex flex-wrap justify-between gap-4">
        <div>
          <div className="flex flex-wrap items-center gap-3">
            <h1 className="text-2xl font-semibold tracking-tight">
              {item.title}
            </h1>
            <StatusBadge tone={item.enabled ? "positive" : "neutral"}>
              {item.enabled ? "已启用" : "已停用"}
            </StatusBadge>
          </div>
          <p className="mt-2 text-xs text-muted-foreground">
            {item.maps.length} 项字段对应 · 更新于{" "}
            {formatUnixSeconds(item.updatedAt)}
          </p>
        </div>
        {canWrite ? (
          <div className="flex flex-wrap gap-2">
            <Button
              variant="outline"
              disabled={pending}
              onClick={onRefreshNames}
            >
              补充名称
            </Button>
            <Button variant="outline" disabled={pending} onClick={onEdit}>
              编辑配置
            </Button>
            <Button
              variant="outline"
              disabled={pending || rotatingToken}
              onClick={() => void rotateToken()}
            >
              {rotatingToken ? "轮换中…" : "轮换访问 Token"}
            </Button>
            <Button variant="outline" disabled={pending} onClick={onToggle}>
              {item.enabled ? "停用" : "启用"}
            </Button>
            <Button
              variant="ghost"
              className="text-destructive"
              disabled={pending}
              onClick={onDelete}
            >
              删除
            </Button>
          </div>
        ) : (
          <span className="text-sm text-muted-foreground">只读</span>
        )}
      </header>
      {tokenCopyFailure === null ? null : (
        <div
          role="alert"
          className="space-y-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          <p>
            浏览器这次没允许写剪贴板。下面的 Token 明文只出现这一次，选中后按
            Ctrl/Cmd + C 手动复制。
          </p>
          <code className="block rounded-md border border-border bg-muted/50 px-2 py-1 font-mono text-xs break-all text-foreground select-all">
            {tokenCopyFailure}
          </code>
        </div>
      )}
      <div className="rounded-xl border bg-card p-5 sm:p-6">
        <p className="mb-5 text-xs font-medium text-muted-foreground">
          这条配置如何工作
        </p>
        <div className="grid items-start gap-5 md:grid-cols-[1fr_auto_1fr_auto_1fr]">
          <div className="min-w-0">
            <p className="text-xs text-muted-foreground">来源数据表</p>
            <p className="mt-2 break-words text-lg font-medium">
              {nameOf(item.baseName, "表格名称待补充")}
            </p>
            <p className="mt-1 break-words text-sm text-muted-foreground">
              {nameOf(item.tableName, "数据表名称待补充")}
            </p>
            <ExternalLinkButton href={baseUrl}>打开多维表</ExternalLinkButton>
          </div>
          <ArrowRight
            className="mt-6 hidden size-4 text-muted-foreground md:block"
            aria-hidden="true"
          />
          <div className="min-w-0">
            <p className="text-xs text-muted-foreground">发起飞书审批</p>
            <p className="mt-2 break-words text-lg font-medium">
              {nameOf(item.approvalName, "审批名称待补充")}
            </p>
            <ExternalLinkButton href={approvalUrl}>
              打开审批定义
            </ExternalLinkButton>
            <Button
              size="sm"
              variant="link"
              className="block h-auto p-0 text-xs"
              onClick={() => {
                setRelatedPage(1);
                setRelatedOpen(true);
              }}
            >
              查看关联配置
            </Button>
          </div>
          <ArrowRight
            className="mt-6 hidden size-4 text-muted-foreground md:block"
            aria-hidden="true"
          />
          <div className="min-w-0">
            <p className="text-xs text-muted-foreground">编号写回原表</p>
            <p className="mt-2 break-words text-lg font-medium">
              {nameOf(item.backfillFieldName, "列名称待补充")}
            </p>
            <p className="mt-2 text-sm text-muted-foreground">
              审批创建并完成回写后，编号写回同一条原记录。
            </p>
          </div>
        </div>
        <div className="mt-6 grid gap-5 border-t pt-5 sm:grid-cols-3">
          <div>
            <p className="text-xs text-muted-foreground">谁发起审批</p>
            <p className="mt-2 text-sm font-medium">
              取自「{nameOf(item.applicantFieldName, "列名称待补充")}」列
            </p>
            <p className="mt-1 text-xs text-muted-foreground">
              使用这条记录中的申请人。
            </p>
          </div>
          <div>
            <p className="text-xs text-muted-foreground">如何生成审批</p>
            <p className="mt-2 text-sm font-medium">每条记录单独发起</p>
            <p className="mt-1 text-xs text-muted-foreground">
              不同数据表可以共用同一个审批模板。
            </p>
          </div>
          <div>
            <p className="text-xs text-muted-foreground">日期如何处理</p>
            <p className="mt-2 text-sm font-medium">按表格时区填写</p>
            <p className="mt-1 break-words text-xs text-muted-foreground">
              {item.baseTimezone === "Asia/Shanghai"
                ? "北京时间（UTC+8）"
                : item.baseTimezone === "UTC"
                  ? "协调世界时（UTC）"
                  : `配置时区：${item.baseTimezone}`}
            </p>
          </div>
        </div>
      </div>
      <p className="text-xs text-muted-foreground">
        {item.enabled
          ? "已启用仅表示可受理新的派发请求，仍需飞书工作流触发。"
          : "已停用：新的派发请求不会通过此配置受理。"}
      </p>
      <div
        role="tablist"
        aria-label="配置详情"
        className="flex flex-wrap gap-1 border-b pb-2"
      >
        {tabs.map((entry, index) => (
          <Button
            key={entry.id}
            ref={(node) => {
              if (node) refs.current.set(entry.id, node);
              else refs.current.delete(entry.id);
            }}
            role="tab"
            id={`config-tab-${entry.id}`}
            aria-controls={`config-panel-${entry.id}`}
            aria-selected={tab === entry.id}
            tabIndex={tab === entry.id ? 0 : -1}
            variant={tab === entry.id ? "secondary" : "ghost"}
            onClick={() => setTab(entry.id)}
            onKeyDown={(event) => {
              const nextIndex =
                event.key === "Home"
                  ? 0
                  : event.key === "End"
                    ? tabs.length - 1
                    : event.key === "ArrowRight"
                      ? (index + 1) % tabs.length
                      : event.key === "ArrowLeft"
                        ? (index + tabs.length - 1) % tabs.length
                        : null;
              if (nextIndex === null) return;
              event.preventDefault();
              const next = tabs[nextIndex];
              if (next) {
                setTab(next.id);
                refs.current.get(next.id)?.focus();
              }
            }}
          >
            {entry.label}
          </Button>
        ))}
      </div>
      <section
        role="tabpanel"
        id={`config-panel-${tab}`}
        aria-labelledby={`config-tab-${tab}`}
        tabIndex={0}
        className="space-y-4"
      >
        {tab === "maps" ? (
          <>
            <div className="flex flex-wrap items-center justify-between gap-3">
              <div>
                <h2 className="font-semibold">数据如何填写</h2>
                <p className="mt-1 text-sm text-muted-foreground">
                  从表格列取值，填入对应的审批项目。必填表示审批要求填写。
                </p>
              </div>
              <Input
                className="w-full sm:w-64"
                aria-label="搜索字段对应"
                placeholder="搜索表格列或审批项目"
                value={search}
                onChange={(event) => setSearch(event.target.value)}
              />
            </div>
            {item.maps.length === 0 ? (
              <p className="rounded-lg border p-6 text-sm text-muted-foreground">
                暂无字段对应，请通过编辑配置核对映射。
              </p>
            ) : maps.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                没有符合条件的字段对应，请更换关键词。
              </p>
            ) : (
              <div className="overflow-hidden rounded-lg border">
                <div className="hidden grid-cols-[1fr_1fr_1fr] gap-4 bg-muted/40 px-5 py-3 text-xs text-muted-foreground sm:grid">
                  <span>表格列</span>
                  <span>审批项目</span>
                  <span>填写规则</span>
                </div>
                {maps.map((map) => (
                  <div
                    key={map.widgetId}
                    className="grid gap-3 border-t p-5 first:border-t-0 sm:grid-cols-3"
                  >
                    <div className="break-words">
                      <span className="mb-1 block text-xs text-muted-foreground sm:hidden">
                        表格列
                      </span>
                      {nameOf(map.bitableFieldName, "列名称待补充")}
                    </div>
                    <div className="break-words">
                      <span className="mb-1 block text-xs text-muted-foreground sm:hidden">
                        审批项目
                      </span>
                      <span className="font-medium">
                        {nameOf(map.widgetName, "审批项目名称待补充")}
                      </span>
                      {map.required && (
                        <span className="ml-2 text-xs text-destructive">
                          必填
                        </span>
                      )}
                      <p className="mt-1 text-xs text-muted-foreground">
                        {WIDGET_LABELS[map.widgetType] ?? "类型待确认"}
                      </p>
                    </div>
                    <div className="text-sm">
                      <p>{RULE_LABELS[map.converter] ?? "填写规则待确认"}</p>
                      {map.optionSourceKey && (
                        <p className="mt-1 break-words text-xs text-muted-foreground">
                          选项来自：
                          {nameOf(map.optionSourceName, "来源名称待补充")}
                        </p>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </>
        ) : tab === "access" ? (
          <div className="space-y-5">
            <div>
              <h2 className="font-semibold">接入数据</h2>
              <p className="mt-1 text-sm text-muted-foreground">
                飞书工作流向这个地址发 POST
                请求即可触发派发；鉴权靠请求体里的访问
                Token，每条配置各自一份，按需轮换（旧值立即失效）。
              </p>
              <p className="mt-2 rounded-md border border-border bg-muted/50 p-3 text-xs text-muted-foreground">
                地址按**当前控制台域名**生成。若你现在是
                localhost、内网或开发代理地址，
                飞书服务器访问不到它——请替换为实际部署的公网 API 地址（保留
                <code>/api/v1/feishu/approval/dispatch/&lt;配置标识&gt;</code>
                这一段路径与末尾的配置标识）。
              </p>
            </div>
            <dl className="grid gap-4 sm:grid-cols-2">
              <div className="min-w-0">
                <dt className="text-xs text-muted-foreground">派发链接</dt>
                <dd className="mt-1 flex items-start gap-2">
                  {dispatchUrl ? (
                    <>
                      <code className="min-w-0 flex-1 rounded-md border border-border bg-muted/50 px-2 py-1 font-mono text-xs break-all select-all">
                        {dispatchUrl}
                      </code>
                      <Button
                        size="sm"
                        variant="ghost"
                        aria-label="复制派发链接"
                        onClick={() => void copy("派发链接", dispatchUrl)}
                      >
                        复制
                      </Button>
                    </>
                  ) : (
                    <p className="text-xs text-muted-foreground">
                      该配置还没有工作流接入凭据（configKey 为空）——上方点一次
                      「轮换访问
                      Token」即可签发，签发后这里会出现可复制的派发链接。
                    </p>
                  )}
                </dd>
              </div>
              <div className="min-w-0">
                <dt className="text-xs text-muted-foreground">
                  最近签发/轮换 Token
                </dt>
                <dd className="mt-1 flex flex-wrap items-center gap-2 font-mono text-sm break-all">
                  {item.tokenRotatedAt
                    ? formatUnixSeconds(item.tokenRotatedAt)
                    : "从未签发"}
                  {item.tokenRotatedAt && canWrite ? (
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={revealingToken}
                      onClick={() => void revealToken()}
                    >
                      {revealingToken ? "读取中…" : "复制 Token"}
                    </Button>
                  ) : null}
                </dd>
              </div>
            </dl>
            <div className="grid gap-3 lg:grid-cols-2">
              <div className="min-w-0 space-y-2">
                <h3 className="font-medium">单行按钮：派发当前记录</h3>
                <pre
                  aria-label="单行派发请求体"
                  className="overflow-x-auto rounded-md bg-muted p-3 font-mono text-xs select-all"
                >
                  <code>{singleBody}</code>
                </pre>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => void copy("单行派发请求体", singleBody)}
                >
                  复制单行模板
                </Button>
              </div>
              <div className="min-w-0 space-y-2">
                <h3 className="font-medium">页面按钮：受理批量派发</h3>
                <pre
                  aria-label="批量派发请求体"
                  className="overflow-x-auto rounded-md bg-muted p-3 font-mono text-xs select-all"
                >
                  <code>{batchBody}</code>
                </pre>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => void copy("批量派发请求体", batchBody)}
                >
                  复制批量模板
                </Button>
              </div>
            </div>
            <p className="rounded-md border border-border bg-muted/50 p-3 text-sm">
              替换所有占位符。记录
              ID、发起人与触发人请插入工作流动态变量，不要把占位
              文字作为实际值发送。使用工作流 JSON 模板时，对应引用为
              $.step_btn.recordId、$.step_btn.user（step_btn 为触发节点 ID）。
              <strong>applicant 与 requested_by 都要用 $.step_btn.user</strong>
              ——该变量渲染出的是<strong>姓名</strong>（真机核实），正合
              applicant 所需；两个字段都需要字符串，不要传对象。Content-Type
              请求头填 application/json。
            </p>
            <p className="rounded-md border border-border bg-muted/50 p-3 text-sm">
              省略 record_id
              会唤醒所有启用配置的待处理队列，并非只处理请求中指定的数据表；后台处理回填列为空的记录。批量按钮请仅在需要处理整个队列时使用。
            </p>
            <p className="text-xs text-muted-foreground">
              若提示「配置不存在」，检查派发链接里的配置标识是否正确；若提示「Token
              无效」，说明轮换过 Token
              而工作流里还是旧值，回本页重新复制；若提示「尚未签发访问
              Token」，说明这条配置还没签发过凭据，点上方「轮换访问
              Token」；若处于等待态，补齐申请人及审批必填字段后再查看处理结果。
            </p>
          </div>
        ) : tab === "usage" ? (
          <div className="space-y-5">
            <h2 className="font-semibold">在飞书多维表中使用</h2>
            <ol className="list-decimal space-y-3 pl-5 text-sm">
              <li>填写记录中的申请人和审批必填内容。</li>
              <li>
                在飞书多维表中设置工作流（例如按钮触发），向派发接口发出请求。
              </li>
              <li>审批创建并完成回写后，在原记录查看审批编号。</li>
            </ol>
            <p className="rounded-lg border bg-muted/40 p-4 text-sm">
              本页未检测工作流触发条件、权限或实际运行结果。请在飞书中核对工作流，运行问题通过派发记录排查。
            </p>
            <Button variant="outline" onClick={onRequests}>
              查看派发记录
            </Button>
            <div className="space-y-3 text-sm">
              <h3 className="font-medium">常见疑问</h3>
              <details>
                <summary className="cursor-pointer">
                  多张表能否共用同一个审批？
                </summary>
                <p className="mt-2 text-muted-foreground">
                  可以。不同数据表分别建立配置；每张表独立设置申请人、回填列和字段对应。同一表的不同视图仍属于同一配置。
                </p>
              </details>
              <details>
                <summary className="cursor-pointer">
                  已启用，为什么仍未发起审批？
                </summary>
                <p className="mt-2 text-muted-foreground">
                  启用仅允许受理派发请求。请核对飞书工作流是否触发、访问权限及派发记录中的失败原因。
                </p>
              </details>
              <details>
                <summary className="cursor-pointer">
                  如何更换来源或审批模板？
                </summary>
                <p className="mt-2 text-muted-foreground">
                  当前可编辑标题、时区和字段对应，并可启停。来源表、审批定义、申请人列和编号回填列固定；更换需重新建立配置。同一表重建需先处理原配置。
                </p>
              </details>
            </div>
          </div>
        ) : (
          <div className="space-y-5">
            <p className="text-sm text-muted-foreground">
              名称为展示快照，可能暂旧；稳定标识仍用于派发和回填。此处不展示访问凭据。
            </p>
            <dl className="grid gap-4 rounded-lg border p-5 sm:grid-cols-2">
              {(
                [
                  ["配置编号", String(item.id), false],
                  ["Base Token", item.baseToken, true],
                  ["Table ID", item.tableId, true],
                  ["Approval Code", item.approvalCode, true],
                  ["申请人列标识", item.applicantField, false],
                  ["编号回填列标识", item.backfillField, false],
                  ["配置时区", item.baseTimezone, false],
                  [
                    "表单结构缓存时间",
                    item.formSnapshotAt
                      ? formatUnixSeconds(item.formSnapshotAt)
                      : "尚无结构缓存时间",
                    false,
                  ],
                ] as const
              ).map(([label, value, copyable]) => (
                <div key={label} className="min-w-0">
                  <dt className="text-xs text-muted-foreground">{label}</dt>
                  <dd className="mt-1 break-all font-mono text-sm">
                    {value}
                    {copyable && (
                      <Button
                        size="sm"
                        variant="ghost"
                        aria-label={`复制${label}`}
                        onClick={() => void copy(label, value)}
                      >
                        复制
                      </Button>
                    )}
                  </dd>
                </div>
              ))}
            </dl>
            <p className="text-xs text-muted-foreground">
              表单结构缓存时间是最近保存审批控件结构的时间；不代表配置检查通过，也不代表审批已经运行成功。
            </p>
            <details className="rounded-lg border p-4">
              <summary className="cursor-pointer text-sm font-medium">
                原始字段对应
              </summary>
              <div className="mt-4 space-y-4">
                {item.maps.map((map) => (
                  <dl
                    key={map.widgetId}
                    className="grid gap-2 border-t pt-3 text-xs sm:grid-cols-2"
                  >
                    {[
                      ["控件标识", map.widgetId],
                      ["来源列标识", map.bitableField],
                      ["控件原始类型", map.widgetType],
                      ["转换规则", map.converter],
                      ["外部选项来源 key", map.optionSourceKey ?? "未设置"],
                    ].map(([label, value]) => (
                      <div key={label}>
                        <dt className="text-muted-foreground">{label}</dt>
                        <dd className="mt-1 break-all font-mono">
                          {value || "未缓存"}
                        </dd>
                      </div>
                    ))}
                  </dl>
                ))}
                {item.maps.length === 0 && (
                  <p className="text-sm text-muted-foreground">暂无字段对应</p>
                )}
              </div>
            </details>
          </div>
        )}
      </section>
      <Dialog open={relatedOpen} onOpenChange={setRelatedOpen}>
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>同一审批的关联配置</DialogTitle>
            <DialogDescription>
              不同数据表独立配置，包含已停用配置。
            </DialogDescription>
          </DialogHeader>
          {related.isPending ? (
            <p role="status">正在读取关联配置…</p>
          ) : related.isError ? (
            <div role="alert">
              <p>暂时无法读取关联配置</p>
              <Button variant="outline" onClick={() => void related.refetch()}>
                重新加载
              </Button>
            </div>
          ) : (
            <>
              <div className="max-h-80 overflow-auto">
                {related.data?.items.map((config) => (
                  <div
                    key={config.id}
                    className="flex flex-wrap items-center justify-between gap-3 border-b py-4"
                  >
                    <div className="min-w-0">
                      <p className="break-words font-medium">
                        {config.title}
                        {config.id === item.id && (
                          <span className="ml-2 text-xs text-muted-foreground">
                            当前配置
                          </span>
                        )}
                      </p>
                      <p className="mt-1 break-words text-xs text-muted-foreground">
                        {nameOf(config.baseName, "表格名称待补充")} ·{" "}
                        {nameOf(config.tableName, "数据表名称待补充")}
                      </p>
                    </div>
                    <div className="flex items-center gap-2">
                      <StatusBadge
                        tone={config.enabled ? "positive" : "neutral"}
                      >
                        {config.enabled ? "已启用" : "已停用"}
                      </StatusBadge>
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => {
                          setRelatedOpen(false);
                          onOpenConfig(config.id);
                        }}
                      >
                        查看配置
                      </Button>
                    </div>
                  </div>
                ))}
                {related.data?.total === 0 && <p>暂无关联配置</p>}
              </div>
              <ListPagination
                page={relatedPage}
                pageSize={relatedSize}
                total={related.data?.total ?? null}
                pending={related.isFetching}
                onPageChange={setRelatedPage}
                onPageSizeChange={(size) => {
                  setRelatedSize(size);
                  setRelatedPage(1);
                }}
              />
            </>
          )}
        </DialogContent>
      </Dialog>
    </>
  );
}

function ExternalLinkButton({
  href,
  children,
}: {
  href: string | undefined;
  children: React.ReactNode;
}) {
  if (!href)
    return (
      <p className="mt-3 text-xs text-muted-foreground">
        {children}（标识待核对）
      </p>
    );
  return (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      className="mt-3 inline-flex items-center gap-1 text-xs underline-offset-4 hover:underline"
    >
      {children}
      <ExternalLink className="size-3" aria-hidden="true" />
    </a>
  );
}
