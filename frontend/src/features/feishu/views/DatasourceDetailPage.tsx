/**
 * 某个数据源下的选项（只读）。路由 `/feishu/datasources/:sourceKey`，lazy → default 导出。
 *
 * 三条设计约束落在这里：
 *
 * 1. **只读**：选项只能由飞书多维表格的自动化推送，控制台不提供任何增删改入口
 *    （两个可写入口会让审计语义与数据来源分叉）。所以页面顶部那条说明不是装饰，
 *    它得同时说清「选项从哪来」与两级「停用」的区别——后者最容易在排查时被混淆：
 *    数据源级停用是整个控件取不到选项，选项级停用只影响那一条，且后端是禁用而非删除。
 * 2. **「最近写入」列默认倒序**（`updated_at`）。这一列的语义是「这行选项最后一次被
 *    写是什么时候」，**不是**「推送还活着吗」——出站拉取同样会写这些行，两条路径都会
 *    改 `updated_at`。判断某个数据源的同步是否还活着，看上面「同步」区里的
 *    `lastSuccessAt` 与 `consecutiveFailures`。
 *    去重键 `option_id` 恒作收尾，否则同一批写入落下的同一个 unix 秒会让翻页重复或漏行。
 * 3. **缺 `feishu.option.read` 是 403 态而不是空列表**：三个权限位彼此独立，存在
 *    「能看数据源、看不到选项」这种身份，也存在**两粒都没有**的身份——
 *    后者连「可以看数据源本身」都不成立，所以 403 文案按实际拿到的权限分两句说。
 *    那时选项区是空的，但**不代表真的没有选项**，绝不能渲染成「0 选项」。
 * 4. **选项为空不能断言数据源是好的**：服务端的选项查询对不存在的 `source_key`
 *    也只回空结果集。本页额外拉一次数据源列表并按 `sourceKey` 取精确匹配，因此
 *    「取到了这条数据源」与「没取到」是可分辨的——后者会让同步区明说「查不到这条数据源」，
 *    而不是与「还没有选项」混为一谈。
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
  canWriteDatasources,
  checkDatasourceHealth,
  hasOperation,
  useCredentialClient,
  useDatasourceList,
  useFeishuActions,
  useOptionList,
  usePullSchedule,
} from "../api";
import { CopyField } from "../components/CopyField";
import { CredentialChecklist } from "../components/CredentialChecklist";
import { DatasourceHealthPanel } from "../components/DatasourceHealthPanel";
import { ListPagination } from "../components/ListPagination";
import { StatusBadge } from "../components/StatusBadge";
import { DEFAULT_PAGE_SIZE } from "../list-query";
import type {
  DatasourceItem,
  OptionItem,
  OptionListQuery,
  OrderByClause,
} from "../types";
import {
  approvalOptionsUrl,
  credentialItems,
  describeNextPull,
  formatUnixSeconds,
  hasCompleteCoordinates,
  ingestModeLabel,
  localeLabel,
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

export default function DatasourceDetailPage() {
  const { sourceKey = "" } = useParams<{ sourceKey: string }>();
  const actions = useFeishuActions();
  // 403 态的「重试」重拉的是**界面目录**：权限刚开通时目录还是上一份缓存。
  const catalog = useUiCatalog();
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(DEFAULT_PAGE_SIZE);
  const [orderBy, setOrderBy] = useState<OrderByClause[]>(
    DEFAULT_OPTION_ORDER_BY,
  );

  const query = useMemo<OptionListQuery>(
    () => ({ sourceKey, page, pageSize, orderBy }),
    [sourceKey, page, pageSize, orderBy],
  );
  const optionsQuery = useOptionList(query);
  const items = optionsQuery.data?.items ?? [];
  const total = optionsQuery.data?.total ?? null;

  // 这条数据源本身。取数方式与同步状态都挂在它上面，而本页只从路由拿到 sourceKey
  // ——没有「取单条数据源」的 Action，所以借列表端点并**在本地取精确匹配**：
  // 服务端的 search 是模糊的（LIKE 命中 source_key 或 title），可能带回别的行。
  const datasourceListQuery = useMemo(
    () => ({
      page: 1,
      pageSize: 50,
      search: sourceKey,
      status: "all" as const,
      orderBy: [{ field: "source_key", direction: "Asc" as const }],
    }),
    [sourceKey],
  );
  // 「立即拉取」的触发状态。轮询只在 pending 期间打开——常态下这个列表没有轮询的必要。
  const [pull, setPull] = useState<PullTrigger>({ kind: "idle" });
  const datasourceQuery = useDatasourceList(datasourceListQuery, {
    refetchInterval: pull.kind === "pending" ? PULL_POLL_MS : false,
  });
  // 表级化之后表级行上没有 `source_key`（它在每条字段绑定上），所以按两处定位：
  // 表级行的 `source_key`（旧形状）**或**任一绑定的 `source_key`（新形状）。
  // 后者正是路由参数在表级世界里的含义——一个字段的取数标识。
  const datasource: DatasourceItem | null =
    datasourceQuery.data?.items.find(
      (item) =>
        item.sourceKey === sourceKey ||
        item.fields.some((binding) => binding.sourceKey === sourceKey),
    ) ?? null;

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

  async function handlePullNow() {
    if (datasource === null) return;
    setPull({ kind: "pending", baseline: datasource.lastPullAt });
    try {
      await actions.pullNow(datasource.sourceKey);
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
          <h1 className="font-mono text-xl font-semibold">
            {sourceKey === "" ? "数据源" : sourceKey}
          </h1>
          <p className="text-sm text-muted-foreground">
            这个数据源下已推送的选项（只读）
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

      <section className="space-y-3 rounded-xl border border-border bg-card p-5">
        <h2 className="text-base font-medium">同步</h2>
        {sourceKey === "" ? (
          <p className={NEUTRAL_BAR}>
            地址里没有数据源标识，无法确定要看哪一个数据源。
          </p>
        ) : datasourceQuery.isPending ? (
          <Skeleton className="h-24 w-full" />
        ) : datasource === null ? (
          <p aria-live="polite" className={NEUTRAL_BAR}>
            查不到这条数据源——它可能已被删除，或者当前身份读不到它。
          </p>
        ) : (
          <SyncPanel
            item={datasource}
            pull={pull}
            onPullNow={() => void handlePullNow()}
          />
        )}
      </section>

      <section className="space-y-3 rounded-xl border border-border bg-card p-5">
        <h2 className="text-base font-medium">体检</h2>
        {datasource === null ? (
          <p aria-live="polite" className={NEUTRAL_BAR}>
            查不到这条数据源，体检无从谈起。
          </p>
        ) : (
          <HealthSection datasource={datasource} />
        )}
      </section>

      <section className="space-y-3 rounded-xl border border-border bg-card p-5">
        <h2 className="text-base font-medium">
          凭据清单（每字段一组 URL + Token）
        </h2>
        {datasource === null ? (
          <p aria-live="polite" className={NEUTRAL_BAR}>
            查不到这条数据源，取不到它的字段绑定。
          </p>
        ) : (
          <CredentialSection datasource={datasource} />
        )}
      </section>

      <section className="space-y-3 rounded-xl border border-border bg-card p-5">
        <div className="flex flex-wrap items-baseline justify-between gap-2">
          <h2 className="text-base font-medium">选项</h2>
          {total !== null && optionsQuery.isSuccess ? (
            <span className="text-xs text-muted-foreground tabular-nums">
              共 {total} 条
            </span>
          ) : null}
        </div>

        {sourceKey === "" ? (
          <p aria-live="polite" className={NEUTRAL_BAR}>
            地址里没有数据源标识，无法确定要看哪一个数据源。请从列表页点进来。
          </p>
        ) : !actions.canReadOptions ? (
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
        ) : optionsQuery.isPending ? (
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
          <OptionEmptyState sourceKey={sourceKey} />
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
  onPullNow,
}: {
  item: DatasourceItem;
  pull: PullTrigger;
  onPullNow: () => void;
}) {
  const catalog = useUiCatalog();
  const schedule = usePullSchedule();
  const health = syncHealth(item);
  // 排程是**全局**的，所以这个值在每条数据源的详情页都一样。
  // `now` 是**秒**（`nextRunAt` 也是秒）——传毫秒不会报错，只会让比较恒真。
  const now = useNowSeconds();
  const nextPull = describeNextPull(schedule.data ?? null, now);

  // 按钮只在**真的能触发**时才渲染：目录里有 `pull_now` 才说明服务端起了 worker。
  // 没有它却渲染一个按钮，点下去只会得到「UI 目录里找不到 Action」——那是把
  // 「这个部署没开导出站拉取」错报成一次功能故障。
  const canTrigger =
    hasOperation(catalog.data, DATASOURCE_OPERATION_IDS.pullNow) &&
    canWriteDatasources(catalog.data);

  const rows: Array<[string, string]> = [
    ["取数方式", ingestModeLabel(item.ingestMode)],
    ["Base Token", item.bitableBaseToken ?? "—"],
    ["数据表 ID", item.bitableTableId ?? "—"],
    ["视图 ID", item.bitableViewId ?? "—"],
    ["取数列", item.bitableFieldName ?? "—"],
    ["最近成功同步", formatUnixSeconds(item.lastSuccessAt ?? 0)],
    ["最近尝试拉取", formatUnixSeconds(item.lastPullAt ?? 0)],
    ["下次自动拉取", nextPull.label],
  ];
  if (item.bitableFieldName !== null && !hasCompleteCoordinates(item)) {
    rows.push(["提示", "坐标不全，服务端每轮都会跳过这个数据源"]);
  }

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

      <p className="text-xs text-muted-foreground">{nextPull.detail}</p>

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

      <CopyField
        value={approvalOptionsUrl(window.location.origin, item.sourceKey)}
        label="取选项接口地址（粘到飞书审批后台的外部选项配置里）："
        hint="按当前站点的地址拼出来的。若飞书要访问的是另一个域名（例如加了 TLS 边缘之后），以那个域名为准。"
      />

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
