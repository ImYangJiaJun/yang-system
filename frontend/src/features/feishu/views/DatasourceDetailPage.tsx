/**
 * 一个数据源的详情页。路由 `/feishu/datasources/:id`，路由级 lazy → 必须 default 导出。
 *
 * 设计约束落在这里：
 *
 * 1. **身份是表级主键 `id`**。一条数据源 = 一张表 = 一条表级行 + N 条字段绑定；
 *    `source_key` 属于**绑定**（它要进飞书控件的 URL），不是一条数据源的身份。
 *    本页曾经按 `:sourceKey` 路由，于是它天生只能看一个字段，而且它**自己的查询**
 *    还得拿 `source_key` 去 `order_by` 并 `search` 一张没有这一列的**表级**表——
 *    两处都失败，「查到了哪条数据源」恒为 null，三块面板一起说「查不到这条数据源」。
 *    这类「表级化删掉的概念还有消费方在引用」的 bug 前后出现过四次，本页是第四次。
 *    所以 `DatasourceItem` 上干脆不再有 `sourceKey` 字段，由编译器替我们看着。
 * 2. **只读**：选项只能由多维表格自动化推送或服务端出站拉取写入，控制台不提供任何
 *    增删改入口（两个可写入口会让审计语义与数据来源分叉）。所以页面顶部那条说明
 *    不是装饰，它得同时说清「选项从哪来」与两级「停用」的区别：数据源级停用是整个
 *    控件取不到选项，选项级停用只影响那一条，且后端是禁用而非删除。
 * 3. **「失败」不等于「空」**。请求被拒（400/403）与服务端确认没有这一行（200 + 空
 *    结果集）是两件事，下一步动作完全不同（重试/看错误码 vs 回列表确认）。本页把
 *    它们分成四态（`types.ts` 的 `datasourceGap`），失败态必须把 `status` / `code` /
 *    `request_id` 原样显示出来——否则排障只能靠猜，而猜错的代价是一个来回：
 *    上一版把三块都渲染成「查不到这条数据源」，而那条源就在库里、9 条绑定、
 *    服务端一分钟前刚拉成功。
 * 4. **选项为空不能断言数据源是好的**：服务端的选项查询对不存在的 `source_key`
 *    也只回空结果集。所以三个区块统一按同一个 `gap` 渲染，而不是各自判
 *    `datasource === null`——那样连**加载中**都会被说成「查不到这条数据源」。
 * 5. **「最近写入」列默认倒序**（`updated_at`）。这一列的语义是「这行选项最后一次被
 *    写是什么时候」，**不是**「推送还活着吗」——出站拉取同样会写这些行，两条路径都会
 *    改 `updated_at`。判断某个数据源的同步是否还活着，看「同步」区里的
 *    `lastSuccessAt` 与 `consecutiveFailures`。
 *    去重键 `option_id` 恒作收尾，否则同一批写入落下的同一个 unix 秒会让翻页重复或漏行。
 */

import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { Link, useParams } from "react-router";
import { useQuery } from "@tanstack/react-query";
import {
  ArrowDown,
  ArrowLeft,
  ArrowUp,
  ArrowUpDown,
  RefreshCw,
} from "lucide-react";

import { useSessionCredentials, useUiCatalog } from "@/engine";

import { Button } from "@/shared/ui/button";
import { Skeleton } from "@/shared/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";

import {
  DATASOURCE_OPERATION_IDS,
  DEFAULT_OPTION_ORDER_BY,
  TABLE_OPERATION_IDS,
  XLSX_OPERATION_IDS,
  canWriteDatasources,
  checkDatasourceHealth,
  hasOperation,
  useCredentialClient,
  useDatasourceList,
  useFeishuActions,
  useOptionList,
  usePullSchedule,
  useXlsxImportClient,
} from "../api";
import type { XlsxImportReport } from "../api";
import { CredentialChecklist } from "../components/CredentialChecklist";
import { FieldBindingsTable } from "../components/FieldBindingsTable";
import { DatasourceHealthPanel } from "../components/DatasourceHealthPanel";
import { ListPagination } from "../components/ListPagination";
import { StatusBadge } from "../components/StatusBadge";
import { XlsxReimportDialog } from "../components/XlsxReimportDialog";
import { DEFAULT_PAGE_SIZE } from "../list-query";
import type {
  DatasourceGap,
  DatasourceItem,
  OptionItem,
  OptionListQuery,
  OrderByClause,
} from "../types";
import {
  asIngestMode,
  credentialItems,
  datasourceGap,
  describeNextPull,
  formatUnixSeconds,
  ingestModeLabel,
  localeLabel,
  orderBindingsForDisplay,
  pullLanded,
  syncHealth,
} from "../types";

const SKELETON_ROWS = 5;

/// 「立即拉取」的轮询节拍与总预算。
///
/// 触发是**异步受理**的（后端只回 202 语义的「已受理」），所以结果只能靠盯
/// `lastPullAt` 变化来收口。1 秒一拍、15 秒封顶：一轮小表通常 2~3 秒内落定，
/// 而大表可能要几十秒——超过预算就让用户自己回来看，不要一直转。
const PULL_POLL_MS = 1_000;
const PULL_WAIT_MS = 15_000;

/// 「立即拉取」触发后的状态机。
///
/// 每条分支对应一句**不同**的话——尤其 `failed` 与 `timeout` 不能合并：前者是
/// 「服务端明确拒了，照它说的改配置」，后者是「不知道，自己回来看」，用户的下一步动作不同。
type PullTrigger =
  | { kind: "idle" }
  /// `baseline` 是触发前那一轮的 `lastPullAt`——落定判据是「它变了没有」。
  | { kind: "pending"; baseline: number | null }
  | { kind: "landed" }
  | { kind: "timeout" }
  /// 后端在发信号**之前**就拒了（模式不对 / 已停用 / 坐标不全），这里是它的原话。
  | { kind: "failed"; message: string };

const NEUTRAL_BAR =
  "rounded-md border border-border bg-muted/50 px-3 py-2 text-sm";

const ERROR_BAR =
  "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive";

function subscribeToSecond(onStoreChange: () => void): () => void {
  const timer = window.setInterval(onStoreChange, 1_000);
  return () => window.clearInterval(timer);
}

function nowSeconds(): number {
  return Math.floor(Date.now() / 1000);
}

/// 每秒推进一次的「现在」（unix 秒）。
///
/// 存在的理由有两条：渲染期不能直接读时钟（`react-hooks/purity` 会拦），而
/// 「下次自动拉取」要回答的是「还有多久」——拿查询的取回时刻当现在，文案会在
/// 两次取回之间冻住，越看越不准。`useSyncExternalStore` 是 React 给
/// 「订阅外部可变源」的正门，时钟正是这种源。
function useNowSeconds(): number {
  return useSyncExternalStore(subscribeToSecond, nowSeconds, nowSeconds);
}

/**
 * `i18n` 是 JSON 文本（可能是 `'{"zh_cn":"差旅费","en_us":"Travel"}'` 或 null），
 * **不能直接铺在单元格里**。解析失败就当没有多语言文案，原文只进 title 提示。
 */
function parseI18n(
  raw: string | null,
): Array<{ locale: string; text: string }> {
  if (!raw) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (
      parsed === null ||
      typeof parsed !== "object" ||
      Array.isArray(parsed)
    ) {
      return [];
    }
    return Object.entries(parsed as Record<string, unknown>)
      .filter(
        (entry): entry is [string, string] => typeof entry[1] === "string",
      )
      .map(([locale, text]) => ({ locale, text }));
  } catch {
    return [];
  }
}

function i18nText(item: OptionItem): string {
  return parseI18n(item.i18n)
    .map((pair) => `${localeLabel(pair.locale)}：${pair.text}`)
    .join(" · ");
}

function SortableHeader({
  orderBy,
  onSort,
}: {
  orderBy: OrderByClause[];
  onSort: (field: string) => void;
}) {
  const current = orderBy.find((clause) => clause.field === "updated_at");
  const Icon = current
    ? current.direction === "Asc"
      ? ArrowUp
      : ArrowDown
    : ArrowUpDown;

  return (
    <button
      type="button"
      className="inline-flex items-center gap-1 rounded-sm font-medium focus-visible:ring-ring/50 focus-visible:ring-[3px] focus-visible:outline-none"
      onClick={() => onSort("updated_at")}
      aria-label="按最近写入排序"
    >
      最近写入
      <Icon className="size-3.5 text-muted-foreground" aria-hidden="true" />
    </button>
  );
}

/// 路由参数 → 表级主键。非数字（旧地址、手输、被截断的链接）返回 `null`，
/// 由页面把「地址不对」和「这条不存在」分开说。
function parseDatasourceId(raw: string | undefined): number | null {
  if (raw === undefined || raw.trim() === "") return null;
  const value = Number(raw);
  return Number.isInteger(value) && value > 0 ? value : null;
}

/// 一次重新导入的回执，压成一行。
///
/// 逐项都留着，因为它们各自对应一种**要做不同处置**的情形：`rowsRead` 是文件里读到
/// 多少行（不是总行数）、`derived` 是派生出的选项数、`skippedReason` 非空表示这一轮
/// **跳过了**这条绑定（它一行都没动，这是最容易被读成「导入成功」的一种）、异常行数
/// 表示有几行没喂给飞书。少了任何一项，回执就只能说「跑完了」——而那句话回答不了
/// 「跑对了吗」。
function reimportReceipt(title: string, report: XlsxImportReport): string {
  const files = report.files
    .map((file) => `${file.name}：${file.rowsRead} 行`)
    .join("、");
  const bindings = report.bindings
    .map((binding) => {
      const parts = [
        `${binding.sourceKey}：读到 ${binding.fetched} 行 → ${binding.derived} 个选项`,
      ];
      if (binding.disabled > 0) parts.push(`停用 ${binding.disabled} 条`);
      if (binding.unchanged) parts.push("内容没变，本轮没写库");
      if (binding.skippedReason !== null) {
        parts.push(`这一轮跳过了它：${binding.skippedReason}`);
      }
      if (binding.anomalies.length > 0) {
        parts.push(
          `异常 ${binding.anomalies.length} 行${binding.truncatedDetails ? "（还有更多没列出来）" : ""}`,
        );
      }
      return parts.join("，");
    })
    .join("；");
  return `已重新导入「${title}」：${files}${bindings === "" ? "" : `。${bindings}`}`;
}

/// 「这条数据源没渲染出来」的那一块。五种原因分开说（判定在 `types.ts` 的
/// `datasourceGap`，纯函数可测），这里只负责呈现。
///
/// **它存在的唯一理由**：曾经「请求失败」和「没有这一行」渲染成同一句话，于是
/// 详情页三块全说「查不到这条数据源」，而那条数据源就在库里、9 条绑定、服务端
/// 一分钟前刚拉成功。排障只能靠猜——猜错一次就是一个来回。所以失败必须带
/// `status` / `code` / `request_id`，让下一步动作不必靠推理。
///
/// **按钮只在真的能重试时给**：加载中给按钮是废话，而「这一行不存在」重试多少次
/// 也还是不存在——那两个分支的下一步动作是回列表，不是再点一次。
function DatasourceGapNote({
  gap,
  onRetry,
}: {
  gap: DatasourceGap;
  onRetry: () => void;
}) {
  if (gap.kind === "loading") {
    return <Skeleton className="h-24 w-full" />;
  }

  if (gap.kind === "forbidden") {
    // 与选项区的 403 说明同一口径：**这条数据源存不存在，这一页确认不了**，
    // 所以只说「你看不到」，不说「它不存在」。
    return (
      <div role="alert" className={ERROR_BAR + " space-y-2"}>
        <p className="font-medium">当前身份没有查看数据源的权限</p>
        <p>
          缺 <code className="font-mono">feishu.datasource.read</code>
          ——这一栏是空的，
          <span className="font-medium">不代表它真的不存在</span>
          。如果刚刚才开通权限，刷新一次界面目录即可。
        </p>
        <Button variant="outline" size="sm" onClick={onRetry}>
          <RefreshCw aria-hidden="true" />
          重新加载权限目录
        </Button>
      </div>
    );
  }

  if (gap.kind === "failed") {
    const facts = [
      gap.status === null ? null : `HTTP ${gap.status}`,
      gap.code === null ? null : `code ${gap.code}`,
      gap.requestId === null ? null : `request_id ${gap.requestId}`,
    ].filter((part): part is string => part !== null);
    return (
      <div role="alert" className={ERROR_BAR + " space-y-2"}>
        <p className="font-medium">
          读取这条数据源失败——是请求被拒，不是「没有数据」
        </p>
        {facts.length > 0 ? (
          <p className="font-mono text-xs break-all">{facts.join("  ")}</p>
        ) : null}
        <p>{gap.message}</p>
        <Button variant="outline" size="sm" onClick={onRetry}>
          <RefreshCw aria-hidden="true" />
          重试读取
        </Button>
      </div>
    );
  }

  // 剩下的两态（地址不合法 / 服务端确认没有这一行）都没有可重试的东西：
  // 下一步是回列表页，不是再点一次。所以这里**只给话，不给按钮**。
  return (
    <p aria-live="polite" className={NEUTRAL_BAR}>
      {gap.kind === "missing-id"
        ? "地址里没有有效的数据源主键（列表页里的数字 id）——请从数据源列表点进来。"
        : "查询成功但没有这一行——这条数据源可能已被删除，回列表页确认一下。"}
    </p>
  );
}

export default function DatasourceDetailPage() {
  // 详情页按**表级主键**路由：一条数据源的身份是 `id`。
  //
  // 它曾经按 `:sourceKey` 路由 —— 那是**一条字段绑定**的标识，一条表级行有 N 个。
  // 于是那一版页面只能看一个字段，而且还得反过来用 `source_key` 去查一张根本没有
  // 这一列的表（`order_by: source_key` → 400，`search: sourceKey` → 检索只覆盖
  // `title`，命中零行）。两处都失败，`datasource` 恒为 null，整页三块全说
  // 「查不到这条数据源」——而它就在库里。
  const { id: rawId } = useParams<{ id: string }>();
  const datasourceId = parseDatasourceId(rawId);
  const actions = useFeishuActions();
  const xlsxClient = useXlsxImportClient();
  // 403 态的「重试」重拉的是**界面目录**：权限刚开通时目录还是上一份缓存。
  const catalog = useUiCatalog();
  const [reimportOpen, setReimportOpen] = useState(false);
  /// 重新导入的回执。它是**唯一**记录「这一轮导了什么、写了多少」的地方——
  /// 服务端只回这一次，库里不留（导入不写表级时间戳）。
  const [reimportNotice, setReimportNotice] = useState<string | null>(null);
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(DEFAULT_PAGE_SIZE);
  const [orderBy, setOrderBy] = useState<OrderByClause[]>(
    DEFAULT_OPTION_ORDER_BY,
  );

  // 这条数据源本身。没有「取单条数据源」的 Action，所以借列表端点 + **一个 `id`
  // 等值条件**（`id` 已声明 `filterable`）。不再用 `search`：检索只覆盖 `searchable`
  // 的列，而表级行上只有 `title` 可搜。
  const datasourceListQuery = useMemo(
    () => ({
      page: 1,
      pageSize: 1,
      search: "",
      status: "all" as const,
      orderBy: [{ field: "id", direction: "Asc" as const }],
      ...(datasourceId === null ? {} : { id: datasourceId }),
    }),
    [datasourceId],
  );
  // 「立即拉取」的触发状态。轮询只在 pending 期间打开——常态下这个列表没有轮询的必要。
  const [pull, setPull] = useState<PullTrigger>({ kind: "idle" });
  const datasourceQuery = useDatasourceList(datasourceListQuery, {
    // 地址里没有合法主键时**不要发**这一发：发出去的就是「随便给我一条」。
    enabled: datasourceId !== null,
    refetchInterval: pull.kind === "pending" ? PULL_POLL_MS : false,
  });
  const datasource: DatasourceItem | null =
    datasourceQuery.data?.items[0] ?? null;

  // 取不到就是取不到——**不许把「查询失败」和「没有这一行」折进同一个 null**。
  // 这两件事的下一步动作完全不同（重试/看错误码 vs 回列表），而折平之后页面
  // 只能替用户断言一个原因，那个断言是错的。
  const gap = datasourceGap({
    id: datasourceId,
    canRead: actions.canRead,
    isPending: datasourceQuery.isPending,
    isError: datasourceQuery.isError,
    error: datasourceQuery.error,
    present: datasource !== null,
  });

  // 选项是按 `source_key` 索引的，而 `source_key` 属于**一条绑定**：一条数据源有 N 个。
  // 默认看第一条**启用中**的绑定，点上面那张字段表可以把下面的选项切到别的字段。
  //
  // 选择态存 `sourceKey`（不是下标）：回读之后绑定可能被停用/删掉，按下标会切到
  // 另一条绑定上，而按标识比对的最坏结果是「回到默认那一条」，不会指错字段。
  const [selectedSourceKey, setSelectedSourceKey] = useState<string | null>(
    null,
  );
  const bindings = datasource?.fields ?? [];
  // 默认按**展示序**取第一条启用中的绑定，而不是数组里的第一条：上面的字段表就是
  // 按展示序画的，取别的话高亮行会停在表中间某一行的位置，看着像「它替你选了一个
  // 八竿子打不着的字段」。
  const orderedBindings = orderBindingsForDisplay(bindings).map(
    (entry) => entry.binding,
  );
  const defaultSourceKey =
    orderedBindings.find((binding) => binding.enabled)?.sourceKey ??
    orderedBindings[0]?.sourceKey ??
    "";
  // 选中的那条已经不在集合里时（刚被停用/删掉）回落到默认，而不是让下面显示一个
  // 已经不存在的字段的选项——那种「空」会被读成「这个字段没有选项」。
  const optionSourceKey =
    selectedSourceKey !== null &&
    bindings.some((binding) => binding.sourceKey === selectedSourceKey)
      ? selectedSourceKey
      : defaultSourceKey;

  const query = useMemo<OptionListQuery>(
    () => ({ sourceKey: optionSourceKey, page, pageSize, orderBy }),
    [optionSourceKey, page, pageSize, orderBy],
  );
  const optionsQuery = useOptionList(query, {
    enabled: optionSourceKey !== "",
  });
  const items = optionsQuery.data?.items ?? [];
  const total = optionsQuery.data?.total ?? null;

  /// xlsx 源的「最近一次**写库**的导入」。
  ///
  /// 措辞是收窄过的，别写成「最近一次导入」：服务端**确实**记着导入时刻——绑定行的
  /// `last_push_at`，`import_xlsx::persist_binding_status` 在**每一轮**都写它（内容没变
  /// 而跳库的那一轮也写，与选项行同事务）。只是那一列没进 `list_datasources` 的
  /// `BINDING_ITEM_COLUMNS` / `FieldBindingItem`，前端读不到（要投影它得同时动后端列清单、
  /// `feishu-projections.json`、`FIELD_BINDING_KEYS` 并重跑 `gen:contracts`——跨后端的
  /// 契约变更，另开一条）。所以这一格只能退到**选项行自己的写入时间**，而它只在真正写库
  /// 的那一轮才有新值。
  ///
  /// 于是「刚点完重新导入、回执说内容没变本轮没写库，这一格还是几天前」不再自相矛盾——
  /// 它答的就是「上一次真的写进库是什么时候」。
  ///
  /// 它**必须另取一发**（`page_size: 1` + 按时间倒序），不能拿上面那张表来推：那张表由
  /// 用户任意排序，点一次列头（升序）之后它第一页装的就是**最旧**的十条，把这一页的最大值
  /// 当成最近一次写库，说的会是一句假话（显示成几小时前，而实际是刚刚）。只查一条，代价
  /// 是一发几十字节的请求，换的是这一格**无论表格怎么排都是同一个答案**。
  ///
  /// 取不到（这个字段还没有选项）时给 `null`：那一格显示「—」，不编一个时间。
  const isXlsxSource =
    asIngestMode(datasource?.ingestMode ?? "") === "xlsx_import";
  const latestImportedQuery = useOptionList(
    {
      sourceKey: optionSourceKey,
      page: 1,
      pageSize: 1,
      orderBy: DEFAULT_OPTION_ORDER_BY,
    },
    { enabled: isXlsxSource && optionSourceKey !== "" },
  );
  const latestImportedAt =
    latestImportedQuery.data?.items[0]?.updatedAt ?? null;

  /// 重新导入成功：关对话框、给回执、**回读选项**（导入写的就是选项行，不回读的话
  /// 屏幕上还是导入前那份数据，「最近导入」也还停在上一轮）。
  function handleReimported(report: XlsxImportReport) {
    setReimportOpen(false);
    setReimportNotice(
      reimportReceipt(
        datasource?.title ?? `#${String(report.datasourceId)}`,
        report,
      ),
    );
    void optionsQuery.refetch();
    void latestImportedQuery.refetch();
  }

  // 落定：`lastPullAt` 变了就说明这一轮跑过了。**成功失败都算**（见 `pullLanded`）——
  // 失败的一轮同样写了 `last_pull_at`，拿它当判据是为了不让按钮在失败时一直转。
  useEffect(() => {
    if (pull.kind !== "pending") return;
    if (pullLanded(pull.baseline, datasource)) {
      setPull({ kind: "landed" });
    }
  }, [pull, datasource]);

  // 超时兜底：轮询不能无限转下去。
  useEffect(() => {
    if (pull.kind !== "pending") return;
    const timer = window.setTimeout(
      () => setPull({ kind: "timeout" }),
      PULL_WAIT_MS,
    );
    return () => window.clearTimeout(timer);
  }, [pull]);

  /// 重试的动作按缺口种类分：**权限不足要重拉界面目录**（权限刚开通时目录还是
  /// 上一份缓存，而那一发查询因 `enabled: false` 根本没发过），其余都是重取这条数据源。
  function retryDatasource() {
    if (gap?.kind === "forbidden") {
      void catalog.refetch();
      return;
    }
    void datasourceQuery.refetch();
  }

  async function handlePullNow() {
    // 拉取的单位是**表**，所以定位用的是表级主键 `id`——不是路由里那个
    // `source_key`（那是某一条字段绑定的标识，一条表级行有 N 个）。
    if (datasource === null || datasource.id === null) return;
    setPull({ kind: "pending", baseline: datasource.lastPullAt });
    try {
      await actions.pullNow(datasource.id);
    } catch (error) {
      // 后端的预检在**发信号之前**就会拒掉拉不动的源，并把原因带回来。
      setPull({
        kind: "failed",
        message: error instanceof Error ? error.message : String(error),
      });
    }
  }

  function toggleSort(field: string) {
    setOrderBy((previous) => {
      const current = previous.find((clause) => clause.field === field);
      const direction: OrderByClause["direction"] =
        current?.direction === "Asc" ? "Desc" : "Asc";
      // `option_id` 是唯一键，恒作收尾：一次推送会把整批选项写成同一个 unix 秒。
      return [
        { field, direction },
        { field: "option_id", direction: "Asc" },
      ];
    });
  }

  return (
    <main className="mx-auto w-full max-w-4xl space-y-6 p-6">
      <div className="space-y-2">
        {/* 仓库没有 Breadcrumb：返回入口放在页头正上方，是全页唯一一处能回列表的地方。 */}
        <Link
          to="/feishu/datasources"
          className="inline-flex items-center gap-1 rounded-sm text-sm text-muted-foreground hover:text-foreground focus-visible:ring-ring/50 focus-visible:ring-[3px] focus-visible:outline-none"
        >
          <ArrowLeft className="size-3.5" aria-hidden="true" />
          返回数据源列表
        </Link>
        <div className="space-y-1">
          <h1 className="text-xl font-semibold">
            {datasource?.title ??
              (datasourceId === null ? "数据源" : `数据源 #${datasourceId}`)}
          </h1>
          <p className="text-sm text-muted-foreground">
            一条数据源对应一张表（`id` = {datasourceId ?? "—"}
            ），选项按字段分别索引。
          </p>
        </div>
      </div>

      <p aria-live="polite" className={NEUTRAL_BAR}>
        选项由多维表格自动推送，控制台只读——这里能看到什么，取决于多维表格那边推了什么。
        两个层级的「停用」不一样：
        <span className="font-medium">数据源级停用</span>
        会让飞书审批里用它的控件整个取不到选项；
        <span className="font-medium">选项级停用</span>
        （下面状态列里的「已停用」）只影响那一条，而且后端是把它禁用而不是删除，改回来就恢复。
      </p>

      {/* 这条数据源取不到时，**只在一处**说明原因。三个区块共用同一个依赖
          （那一条表级行），在三个标题下重复同一句话、还各带一个「重试」，
          是把一个事实说三遍——而它们说的还可能都是错的（见 `DatasourceGapNote`）。 */}
      {gap !== null ? (
        <DatasourceGapNote gap={gap} onRetry={retryDatasource} />
      ) : null}

      {reimportNotice !== null ? (
        <p aria-live="polite" className={NEUTRAL_BAR}>
          {reimportNotice}
        </p>
      ) : null}

      {gap === null && datasource !== null ? (
        <>
          <section className="space-y-3 rounded-xl border border-border bg-card p-5">
            <h2 className="text-base font-medium">同步</h2>
            <SyncPanel
              item={datasource}
              pull={pull}
              latestImportedAt={latestImportedAt}
              onPullNow={() => void handlePullNow()}
              onReimport={() => setReimportOpen(true)}
            />
          </section>

          {/*
            xlsx 源**不走体检**（设计 §5.11）：它按 §4.1 不带多维表格坐标，而后端的
            `health_check` 对「没有坐标且不是定时拉取」的源一律回 40905。那粒 Action 的
            权限位就是 `feishu.datasource.write`（凡能建数据源的身份都有它），所以只要渲染
            这个区块，每打开一次详情页就会自动发出一次必然失败的请求，再把那个失败画成
            「体检坏了」——而这一类源本来就没有体检这回事。整块不渲染，而不是渲染一个
            「不做体检」：后者仍在暗示这里本该有一次。
          */}
          {isXlsxSource ? null : (
            <section className="space-y-3 rounded-xl border border-border bg-card p-5">
              <h2 className="text-base font-medium">体检</h2>
              <HealthSection datasource={datasource} />
            </section>
          )}

          <section className="space-y-3 rounded-xl border border-border bg-card p-5">
            <h2 className="text-base font-medium">
              凭据清单（每字段一组 URL + Token）
            </h2>
            <CredentialSection datasource={datasource} />
          </section>
        </>
      ) : null}

      <section className="space-y-3 rounded-xl border border-border bg-card p-5">
        <div className="flex flex-wrap items-baseline justify-between gap-2">
          <h2 className="text-base font-medium">选项</h2>
          {total !== null &&
          optionsQuery.isSuccess &&
          !optionsQuery.isPlaceholderData ? (
            <span className="text-xs text-muted-foreground tabular-nums">
              共 {total} 条
            </span>
          ) : null}
        </div>

        {gap === null && datasource !== null && bindings.length > 0 ? (
          <div className="space-y-3">
            <FieldBindingsTable
              bindings={bindings}
              selectedSourceKey={optionSourceKey}
              onSelect={(sourceKey) => {
                setSelectedSourceKey(sourceKey);
                // 切字段是一次**结果集变更**，必须回到第 1 页（仓库既有规则见
                // `list-query.ts` 顶部）。不回去的后果不是「看到第 2 页」：新字段的
                // 第 2 页可能不存在，服务端回空 items 而 `count_total` 仍给真值，
                // 于是「共 N 条」与空态同时出现，而空态分支**不渲染分页控件**——
                // 人被卡在那一页，点别的字段也还是同一页码，只能整页重载。
                setPage(1);
              }}
            />
            <p className="text-xs text-muted-foreground">
              下面这张表是
              <span className="font-medium">
                「
                {bindings.find(
                  (binding) => binding.sourceKey === optionSourceKey,
                )?.fieldName ?? optionSourceKey}
                」
              </span>
              的选项。点上面任意一行可以切到那个字段——级联的父列紧挨在它的子列上方。
            </p>
          </div>
        ) : null}

        {/* 权限不足**最先判**：那时数据源那一发请求根本没发，
            `gap` 会是 `forbidden`/`loading`，而这两态都答不出「有没有选项」。 */}
        {!actions.canReadOptions ? (
          <div className="space-y-3">
            <div
              role="alert"
              className="space-y-1 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            >
              <p className="font-medium">你没有查看选项的权限</p>
              <p>
                {/* 三个权限位彼此独立：`datasource.read` 没拿到时，连「能看数据源本身」
                    这句话都不成立，所以这里按实际拿到的权限分两句说。 */}
                {actions.canRead
                  ? "当前身份可以看数据源本身，但看不到它下面的选项——这一栏是空的，"
                  : "当前身份既看不到数据源本身，也看不到它下面的选项——这一栏是空的，"}
                <span className="font-medium">不代表它真的没有选项</span>
                {actions.canRead
                  ? "。如果刚刚才开通权限，重试一次刷新界面目录即可。"
                  : "，这个数据源是否存在这一页同样确认不了。如果刚刚才开通权限，重试一次刷新界面目录即可。"}
              </p>
            </div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void catalog.refetch()}
            >
              <RefreshCw aria-hidden="true" />
              重试
            </Button>
          </div>
        ) : gap !== null ? (
          <p aria-live="polite" className={NEUTRAL_BAR}>
            这条数据源还没取到，所以不知道选项该按哪个字段取——先看上面那条说明。
          </p>
        ) : optionSourceKey === "" ? (
          <p aria-live="polite" className={NEUTRAL_BAR}>
            这条数据源还没有字段绑定，所以没有可看的选项。用配置向导给它勾几列。
          </p>
        ) : optionsQuery.isError ? (
          <div
            role="alert"
            className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
          >
            <span>
              {optionsQuery.error instanceof Error
                ? optionsQuery.error.message
                : "选项列表没有拉到数据"}
            </span>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void optionsQuery.refetch()}
            >
              <RefreshCw aria-hidden="true" />
              重试
            </Button>
          </div>
        ) : optionsQuery.isPending || optionsQuery.isPlaceholderData ? (
          // `isPlaceholderData` **必须**并进来：`keepPreviousData` 让切字段那一帧先拿
          // **上一个字段**的 items/total 顶上，而那一刻 `isPending` / `isError` 都是
          // false、`isSuccess` 还是 true。不排除它，画出来的就是「新字段的名字 + 旧字段
          // 的行」，旧字段恰好为空时还会对新字段说「还没有选项推过来」——一句当场可证伪
          // 的假话。列表页早已这么判（`DatasourceListPage` 的 `settled`），沿用同一条纪律。
          <Table>
            <TableBody>
              {Array.from({ length: SKELETON_ROWS }, (_, index) => (
                <TableRow key={index}>
                  <TableCell colSpan={7}>
                    <Skeleton className="h-4 w-full" />
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        ) : items.length === 0 ? (
          <OptionEmptyState sourceKey={optionSourceKey} />
        ) : (
          <>
            <OptionTable items={items} orderBy={orderBy} onSort={toggleSort} />
            <ListPagination
              page={page}
              pageSize={pageSize}
              total={total}
              pending={optionsQuery.isFetching}
              onPageChange={setPage}
              onPageSizeChange={(next) => {
                setPageSize(next);
                setPage(1);
              }}
            />
          </>
        )}
      </section>

      {datasource !== null && datasourceId !== null ? (
        <XlsxReimportDialog
          open={reimportOpen}
          datasourceId={datasourceId}
          client={xlsxClient}
          onImported={handleReimported}
          onCancel={() => setReimportOpen(false)}
        />
      ) : null}
    </main>
  );
}

/// 体检：把该表启用中的字段拿去和表实际字段比对（T11）。
///
/// 目录里没有这粒权限位时**不渲染体检结论**——服务端没部署、或这个身份没有权限，
/// 画一个「还不知道」的面板只会把「没有」错报成「坏了」。
function HealthSection({ datasource }: { datasource: DatasourceItem }) {
  const catalog = useUiCatalog();
  const session = useSessionCredentials();
  const catalogData = catalog.data;
  const datasourceId = datasource.id;
  const permitted = hasOperation(catalogData, TABLE_OPERATION_IDS.healthCheck);

  const query = useQuery({
    enabled: permitted && datasourceId !== null,
    queryKey: ["feishu", "health", datasourceId ?? 0],
    queryFn: ({ signal }) =>
      checkDatasourceHealth(
        datasourceId ?? 0,
        { catalog: catalogData, session },
        signal,
      ),
    // 体检会出站打飞书：页面内不自动重取，换结论靠「重新体检」。
    staleTime: Infinity,
    refetchOnWindowFocus: false,
  });

  if (!permitted) {
    return (
      <p aria-live="polite" className={NEUTRAL_BAR}>
        当前身份没有体检权限（或这个部署没有体检端点）。
      </p>
    );
  }
  if (datasourceId === null) {
    return (
      <p aria-live="polite" className={NEUTRAL_BAR}>
        这条数据源没有可定位的主键（id）。体检按表定位，缺它做不了。
      </p>
    );
  }

  return (
    <DatasourceHealthPanel
      report={query.data ?? null}
      pending={query.isPending}
      error={query.error === null ? null : String(query.error)}
      onRecheck={() => void query.refetch()}
    />
  );
}

/// 凭据清单：一行一个字段绑定（URL + Token + 轮换）。
///
/// 回显与轮换**是两粒独立的权限位**，所以这里按各自的开关拼一个 client：
/// 只给回显的部署，轮换按钮会在确认框里说「没有轮换权限」，而不是发一个必然 403 的请求。
function CredentialSection({ datasource }: { datasource: DatasourceItem }) {
  const catalog = useUiCatalog();
  const credentialClient = useCredentialClient();
  const canReveal = hasOperation(catalog.data, TABLE_OPERATION_IDS.reveal);
  const canRotate = hasOperation(catalog.data, TABLE_OPERATION_IDS.rotate);

  const items = useMemo(() => credentialItems(datasource), [datasource]);
  const disabledCount = datasource.fields.length - items.length;

  const client = useMemo(() => {
    if (!canReveal && !canRotate) return undefined;
    return {
      reveal: canReveal
        ? credentialClient.reveal
        : () => Promise.reject(new Error("当前身份没有回显凭据的权限")),
      rotate: canRotate
        ? credentialClient.rotate
        : () => Promise.reject(new Error("当前身份没有轮换凭据的权限")),
    };
  }, [canReveal, canRotate, credentialClient]);

  if (items.length === 0) {
    return (
      <p aria-live="polite" className={NEUTRAL_BAR}>
        这条数据源还没有启用中的字段绑定——先用配置向导勾几列，凭据是逐字段生成的。
      </p>
    );
  }

  return (
    <div className="space-y-2">
      <CredentialChecklist items={items} client={client} />
      {disabledCount > 0 ? (
        <p className="text-xs text-muted-foreground">
          还有 {disabledCount} 条已停用的绑定没列在这里：停用的字段出站时会被拒
          （`SOURCE_DISABLED`），把它们配进控件只会得到一个永远取不到选项的下拉。
        </p>
      ) : null}
    </div>
  );
}

/**
 * 落点 3：0 选项不是「暂无数据」，而是「链路还没跑过第一步」。
 *
 * 但**不能顺手替服务端背书**说「这个数据源本身是好的」：本页只从路由拿到了
 * `sourceKey`，没有取单条数据源的 Action（列表那个 Action 打的是集合、用的是另一粒
 * 权限位），而服务端的选项查询对不存在的 `source_key` 也只是回一个空结果集——
 * 「数据源不存在」与「有数据源但没推过选项」在这一页长得一模一样。
 * 所以这里只说能证明的那半句：**这一页没有选项**，剩下两种可能要用户自己去列表页分。
 */
function OptionEmptyState({ sourceKey }: { sourceKey: string }) {
  return (
    <div className="space-y-3 rounded-xl border border-border bg-card p-5">
      <div className="space-y-1">
        <h2 className="text-base font-medium">还没有选项推过来</h2>
        <p className="text-sm text-muted-foreground">
          这一页没有拉到任何选项。可能是多维表格那边的自动化还没往这里推过，
          也可能是这个数据源已经不在了（例如刚被删除）——这一页确认不了它是否还存在，
          回列表页看一眼就知道。
        </p>
        <p className="text-sm text-muted-foreground">
          如果它确实还在，那选项只能从多维表格那边来，控制台不能手工添加。
          觉得应该已经有了的话：先确认那条自动化的目标地址就是这个数据源标识
          <span className="mx-1 font-mono">{sourceKey}</span>
          ，再确认它至少成功跑过一次；推送成功后回到这一页就能看到。
        </p>
      </div>
    </div>
  );
}

function OptionTable({
  items,
  orderBy,
  onSort,
}: {
  items: OptionItem[];
  orderBy: OrderByClause[];
  onSort: (field: string) => void;
}) {
  const sorted = orderBy.find((clause) => clause.field === "updated_at");

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>选项名称</TableHead>
          <TableHead>标识</TableHead>
          <TableHead className="text-right">排序</TableHead>
          <TableHead>默认</TableHead>
          <TableHead>状态</TableHead>
          <TableHead>多语言</TableHead>
          <TableHead
            aria-sort={
              sorted
                ? sorted.direction === "Asc"
                  ? "ascending"
                  : "descending"
                : "none"
            }
          >
            <SortableHeader orderBy={orderBy} onSort={onSort} />
          </TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {items.map((item) => {
          const translations = i18nText(item);
          return (
            <TableRow key={item.optionId} data-slot="option-row">
              <TableCell className="font-medium">{item.label}</TableCell>
              <TableCell className="font-mono text-xs">
                {item.optionId}
              </TableCell>
              <TableCell className="text-right tabular-nums">
                {item.sortOrder}
              </TableCell>
              <TableCell>
                {item.isDefault ? (
                  <StatusBadge tone="neutral">默认</StatusBadge>
                ) : (
                  <span className="text-muted-foreground">—</span>
                )}
              </TableCell>
              <TableCell>
                <StatusBadge tone={item.enabled ? "positive" : "warning"}>
                  {item.enabled ? "启用" : "已停用"}
                </StatusBadge>
              </TableCell>
              <TableCell
                className="text-xs text-muted-foreground"
                title={
                  translations === "" ? (item.i18n ?? undefined) : undefined
                }
              >
                {translations === "" ? "—" : translations}
              </TableCell>
              <TableCell className="text-xs text-muted-foreground tabular-nums">
                {formatUnixSeconds(item.updatedAt)}
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}

/// 同步状态面板。
///
/// 判定逻辑在 `types.ts` 的 `syncHealth` / `describeNextPull` 里（纯函数、可单测）——
/// 这里只负责呈现。分工的理由：那几个分支各对应一种**运维要采取不同动作**的情形，
/// 埋在 JSX 里就只能靠肉眼看，而它们恰恰是最容易在改动中被无声破坏的。
function SyncPanel({
  item,
  pull,
  latestImportedAt,
  onPullNow,
  onReimport,
}: {
  item: DatasourceItem;
  pull: PullTrigger;
  /// xlsx 源的「最近导入」：本页能拿到的、最近的选项行写入时刻（见页面里的推导）。
  latestImportedAt: number | null;
  onPullNow: () => void;
  onReimport: () => void;
}) {
  const catalog = useUiCatalog();
  const schedule = usePullSchedule();
  const health = syncHealth(item);
  // 排程是**全局**的，所以这个值在每条数据源的详情页都一样。
  // `now` 是**秒**（`nextRunAt` 也是秒）——传毫秒不会报错，只会让比较恒真。
  const now = useNowSeconds();
  const nextPull = describeNextPull(schedule.data ?? null, now);

  const mode = asIngestMode(item.ingestMode);
  /// 「拉取」这条线上的东西（按钮与「下次自动拉取」）只对**定时拉取**源成立。
  /// xlsx 与推送两类源服务端都不拉它们：给前者渲染一个「立即拉取」，点下去必然是
  /// 40903（取数方式不是定时拉取）；渲染一个具体的「下次自动拉取」时刻，等于说它会被
  /// 自动同步——那是假的。
  const pulled = mode === "pull";
  const isXlsx = mode === "xlsx_import";

  // 按钮只在**真的能触发**时才渲染：目录里有 `pull_now` 才说明服务端起了 worker。
  // 没有它却渲染一个按钮，点下去只会得到「UI 目录里找不到 Action」——那是把
  // 「这个部署没开导出站拉取」错报成一次功能故障。
  //
  // `id !== null` 也要一起要求：拉取按表级主键定位，缺它这个按钮点下去
  // 什么也发不出去（旧形状的行没有 `id`）。
  const canTrigger =
    pulled &&
    item.id !== null &&
    hasOperation(catalog.data, DATASOURCE_OPERATION_IDS.pullNow) &&
    canWriteDatasources(catalog.data);

  // 「重新导入」与「立即拉取」同一粒写权限的要求，但走的是**另一个端点**
  // （`feishu.datasource.import_xlsx`）：目录里没有它就不渲染，与拉取那条同一取舍。
  const canReimport =
    isXlsx &&
    item.id !== null &&
    hasOperation(catalog.data, XLSX_OPERATION_IDS.importFiles) &&
    canWriteDatasources(catalog.data);

  const rows: Array<[string, string]> = [
    ["取数方式", ingestModeLabel(item.ingestMode)],
    ["Base Token", item.bitableBaseToken ?? "—"],
    ["数据表 ID", item.bitableTableId ?? "—"],
    // 视图可空 = **整表拉取**，不是「缺一个坐标」。写「—」会被读成后者。
    // xlsx 源根本没有视图这一层（坐标三项对它都不适用），所以那一格明说原因——
    // 光写「—」会被读成「这个坐标没配」。
    [
      "视图 ID",
      item.bitableViewId ?? (pulled ? "整表" : isXlsx ? "—（文件导入）" : "—"),
    ],
    // 「取数列」这一行删掉了：一条表级行有 N 条绑定，每条各自取自己那一列，
    // 单一取数列在表级模型里**不存在**（它曾经恒显示「—」）。改报绑定数，
    // 那才是这张表真实的形状；每个字段各自的标识与凭据在下面的「凭据清单」里。
    [
      "取数字段",
      `${item.fields.filter((binding) => binding.enabled).length} 个启用中（共 ${item.fields.length} 条绑定）`,
    ],
    // 导入类源的「同步」其实只有一件事：上一次真的写进库是什么时候。**措辞收窄到
    // 「写库」**——绑定行的 `last_push_at`（每一轮导入都写，含跳库那一轮）没有投影到
    // 前端，这里读的是选项行的写入时间，见页面里 `latestImportedAt` 的推导。
    ...(isXlsx
      ? ([
          ["最近写库的导入", formatUnixSeconds(latestImportedAt ?? 0)],
        ] as Array<[string, string]>)
      : []),
    ["最近成功同步", formatUnixSeconds(item.lastSuccessAt ?? 0)],
    ["最近尝试拉取", formatUnixSeconds(item.lastPullAt ?? 0)],
    ...(pulled
      ? ([["下次自动拉取", nextPull.label]] as Array<[string, string]>)
      : []),
  ];
  // 「坐标不全」不再在这里补一行：顶部的 `syncHealth` 徽章已经会说这件事，
  // 而它的判据（Base Token + 数据表 ID）现在与表级模型一致。

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <StatusBadge tone={health.tone}>{health.title}</StatusBadge>
        <span className="text-xs text-muted-foreground">{health.detail}</span>
        {canTrigger ? (
          <Button
            variant="outline"
            size="sm"
            className="ml-auto"
            disabled={pull.kind === "pending"}
            onClick={onPullNow}
          >
            <RefreshCw aria-hidden="true" />
            {pull.kind === "pending" ? "正在拉取…" : "立即拉取"}
          </Button>
        ) : null}
        {canReimport ? (
          <Button
            variant="outline"
            size="sm"
            className={canTrigger ? undefined : "ml-auto"}
            onClick={onReimport}
          >
            <RefreshCw aria-hidden="true" />
            重新导入
          </Button>
        ) : null}
      </div>

      <dl className="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-2">
        {rows.map(([label, value]) => (
          <div key={label} className="flex min-w-0 justify-between gap-3">
            <dt className="shrink-0 text-muted-foreground">{label}</dt>
            <dd className="truncate font-mono text-xs" title={value}>
              {value}
            </dd>
          </div>
        ))}
      </dl>

      {pulled ? (
        <p className="text-xs text-muted-foreground">{nextPull.detail}</p>
      ) : null}

      {pull.kind === "landed" ? (
        <p aria-live="polite" className={NEUTRAL_BAR}>
          这一轮已经跑过了，上面的时间已更新。成功与否看下面的「最近一次错误」。
        </p>
      ) : null}
      {pull.kind === "timeout" ? (
        <p aria-live="polite" className={NEUTRAL_BAR}>
          15
          秒内没等到状态变化——这一轮可能还在跑，也可能服务端跳过了这个数据源。
          稍后刷新本页看「最近尝试拉取」。
        </p>
      ) : null}
      {pull.kind === "failed" ? (
        <p role="alert" className={ERROR_BAR}>
          {pull.message}
        </p>
      ) : null}

      {/*
        这里曾经有一块「取选项接口地址」：一条数据源一个地址。表级化之后**一条数据源
        有 N 个地址**（每个字段一个 `source_key`），所以那一块已经没有单一值可填——
        它读的还是表级行上那个已被删除的 `source_key`。地址改到「凭据清单」里
        一行一个，那里才是它的归属。
      */}

      {item.lastError !== null ? (
        <div className="space-y-1">
          <h3 className="text-xs font-medium text-muted-foreground">
            最近一次错误
          </h3>
          <pre className="max-h-40 overflow-auto rounded-md border border-border bg-muted/50 p-2 text-xs whitespace-pre-wrap">
            {item.lastError}
          </pre>
        </div>
      ) : null}
    </div>
  );
}
