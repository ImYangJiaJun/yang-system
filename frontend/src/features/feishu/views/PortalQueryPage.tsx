/**
 * 公开门户查询页。路由 `/portal/:slug`，**不在 `RequireAuth` 门控内**——
 * 这是项目里第一个匿名可访问的业务页面，所以它不渲染 `AppLayout`（无侧边栏、无导航），
 * 只有一张居中的卡片。
 *
 * # 元信息先取
 *
 * mount 时先调公开元信息端点 `GET /api/v1/feishu/portal/{slug}`，拿到标题、查询键
 * 展示名、访问模式与空结果提示——这些是**配置**，页面在第一次查询之前就要用。于是
 * 口令框（`access_mode = passphrase`）与登录跳转（`access_mode = login`）都是预先
 * 展开的，不必等用户先查一次吃个 `40301` 才知道该填什么。
 *
 * # 查询失败码仍是兜底
 *
 * 元信息说 login 但 Bearer 已失效（令牌过期）时，后端仍会回 `40301`+「需要登录」，
 * 所以那一条反应式分支留着：
 *   · `40301` + 「需要登录」→ 跳登录页（`access_mode = login`）；
 *   · `40301` + 其余口令文案 → 展开口令框；
 *   · `40401` → 门户不存在或已停用（后端把「停用」也归到这一码，不泄露存在性）。
 */

import { useMutation } from "@tanstack/react-query";
import { useState } from "react";
import { Navigate, useParams } from "react-router";

import { useSessionCredentials } from "@/engine";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";
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
  portalFailureView,
  queryPortal,
  usePortalMetadata,
} from "../portal-api";

const ERROR_BAR =
  "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive";

/// 元信息没给标题 / 查询键展示名时的中性兜底（配置残缺时页面至少不空）。
const FALLBACK_TITLE = "数据查询";
const FALLBACK_PLACEHOLDER = "请输入查询值";

/// 结果单元格的展示文本。值是后端原样回的 JSON（数字 / 字符串 / 布尔 / null）。
function cellText(value: unknown): string {
  if (value === null || value === undefined) return "—";
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean")
    return String(value);
  return JSON.stringify(value);
}

export default function PortalQueryPage() {
  const { slug = "" } = useParams<{ slug: string }>();
  // `access_mode = login` 的门户要靠这个 Bearer 才能通过后端判权；匿名时它是空的。
  const session = useSessionCredentials();
  const metadata = usePortalMetadata(slug);
  const [value, setValue] = useState("");
  const [passphrase, setPassphrase] = useState("");
  const [passphraseRevealed, setPassphraseRevealed] = useState(false);
  const [loginRequired, setLoginRequired] = useState(false);

  const query = useMutation({
    mutationFn: () =>
      queryPortal(slug, value.trim(), passphrase || undefined, session),
    onError: (cause) => {
      const failure = portalFailureView(cause);
      if (failure.revealPassphrase) setPassphraseRevealed(true);
      if (failure.loginRequired) setLoginRequired(true);
    },
  });

  const loginRedirect = `/login?redirect=${encodeURIComponent(`/portal/${slug}`)}`;

  if (metadata.isPending) {
    return (
      <div className="flex min-h-svh items-center justify-center bg-background p-4">
        <div className="w-full max-w-xl space-y-4" aria-label="门户加载中">
          <Skeleton className="h-8 w-48" />
          <Skeleton className="h-10 w-full" />
          <Skeleton className="h-10 w-24" />
        </div>
      </div>
    );
  }

  if (metadata.isError) {
    return (
      <div className="flex min-h-svh items-center justify-center bg-background p-4">
        <p role="alert" className={`w-full max-w-xl ${ERROR_BAR}`}>
          {portalFailureView(metadata.error).message}
        </p>
      </div>
    );
  }

  const meta = metadata.data;

  // login 模式且当前无会话：直接送登录页，别让访客先填一次再吃 40301。
  // 登录页会认 `?redirect=`，cookie 会话有效时再把人送回这里。
  if ((meta.access_mode === "login" && !session.token) || loginRequired) {
    return <Navigate to={loginRedirect} replace />;
  }

  const showPassphrase =
    passphraseRevealed || meta.access_mode === "passphrase";
  const canSubmit =
    value.trim() !== "" &&
    !query.isPending &&
    (!showPassphrase || passphrase !== "");

  const failure =
    query.error === null || query.error === undefined
      ? null
      : portalFailureView(query.error);
  const result = query.data;
  // 命中且真有行才画表；列由后端给，前端不硬编码。
  const columns = result?.columns ?? [];
  const rows = result !== undefined && result.found ? result.rows : [];
  // 查无结果时优先用查询响应里的文案（后端已折过门户的 empty_message），
  // 元信息那一份是查询响应没给文案时的兜底。
  const notice =
    result?.message ||
    (result?.found === false ? meta.empty_message : undefined);

  return (
    <div className="flex min-h-svh items-start justify-center bg-background p-4 sm:items-center">
      <div className="w-full max-w-xl space-y-6">
        <h1 className="text-2xl font-semibold">
          {meta.title || FALLBACK_TITLE}
        </h1>

        <form
          className="space-y-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (canSubmit) query.mutate();
          }}
        >
          {showPassphrase ? (
            <div className="space-y-2">
              <Label htmlFor="portal-passphrase">口令</Label>
              <Input
                id="portal-passphrase"
                type="password"
                autoComplete="off"
                value={passphrase}
                onChange={(event) => setPassphrase(event.target.value)}
              />
            </div>
          ) : null}

          <div className="space-y-2">
            <Label htmlFor="portal-value">查询</Label>
            <Input
              id="portal-value"
              value={value}
              placeholder={meta.query_field_label || FALLBACK_PLACEHOLDER}
              onChange={(event) => setValue(event.target.value)}
            />
          </div>

          <Button type="submit" disabled={!canSubmit}>
            {query.isPending ? "查询中…" : "查询"}
          </Button>
        </form>

        {failure !== null ? (
          <p role="alert" className={ERROR_BAR}>
            {failure.message}
          </p>
        ) : null}

        {rows.length > 0 ? (
          <div className="overflow-x-auto rounded-lg border border-border">
            <Table>
              <TableHeader>
                <TableRow>
                  {columns.map((column) => (
                    <TableHead key={column.field_id}>{column.label}</TableHead>
                  ))}
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map((row, index) => (
                  <TableRow key={index}>
                    {columns.map((column) => (
                      <TableCell key={column.field_id}>
                        {cellText(row.values[column.field_id])}
                      </TableCell>
                    ))}
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        ) : null}

        {query.isSuccess && notice ? (
          <p aria-live="polite" className="text-sm text-muted-foreground">
            {notice}
          </p>
        ) : null}
      </div>
    </div>
  );
}
