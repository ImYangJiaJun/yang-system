/**
 * 公开门户查询页。路由 `/portal/:slug`，**不在 `RequireAuth` 门控内**——
 * 这是项目里第一个匿名可访问的业务页面，所以它不渲染 `AppLayout`（无侧边栏、无导航），
 * 只有一张居中的卡片。
 *
 * # 为什么是「反应式」而不是「先取元信息」
 *
 * 设计文档 §5.1 画了门户标题与「按查询键动态显示的 placeholder」，但那需要一个
 * **门户元信息端点**——而后端 `portal_query` 只回 `{found, columns, rows, message}`，
 * 没有任何地方能拿到标题 / `access_mode` / 查询键提示（全仓 `HttpMethod::Get` 的门户
 * 路由为零）。所以在拿到第一次响应之前，本页无从知道这些。
 *
 * 于是访问控制按**后端给的错误码**反应式展开：
 *   · `40301` + 「需要登录」→ 跳登录页（`access_mode = login`）；
 *   · `40301` + 其余口令文案 → 展开口令框（`access_mode = passphrase`）；
 *   · `40401` → 门户不存在（后端把「停用」也归到这一码，不泄露存在性）。
 *
 * 这样页面在**当前后端**上就是可用的。要补齐标题与动态 placeholder，得先加一个
 * 公开元信息端点（或把 `title` / 查询键标签并进查询响应），见交付说明。
 */

import { useMutation } from "@tanstack/react-query";
import { useState } from "react";
import { Navigate, useParams } from "react-router";

import { useSessionCredentials } from "@/engine";
import { Button } from "@/shared/ui/button";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";

import { portalFailureView, queryPortal } from "../portal-api";

const ERROR_BAR =
  "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive";

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

  // 需登录模式：后端在判权阶段回「需要登录后才能查询」，据此送去登录页。
  if (loginRequired) {
    return (
      <Navigate
        to={`/login?redirect=${encodeURIComponent(`/portal/${slug}`)}`}
        replace
      />
    );
  }

  const canSubmit =
    value.trim() !== "" &&
    !query.isPending &&
    (!passphraseRevealed || passphrase !== "");

  const failure =
    query.error === null || query.error === undefined
      ? null
      : portalFailureView(query.error);
  const result = query.data;
  // 命中且真有行才画表；列由后端给，前端不硬编码。
  const columns = result?.columns ?? [];
  const rows = result !== undefined && result.found ? result.rows : [];

  return (
    <div className="flex min-h-svh items-start justify-center bg-background p-4 sm:items-center">
      <div className="w-full max-w-xl space-y-6">
        <h1 className="text-2xl font-semibold">数据查询</h1>

        <form
          className="space-y-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (canSubmit) query.mutate();
          }}
        >
          {passphraseRevealed ? (
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
              placeholder="请输入查询值"
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

        {query.isSuccess && result?.message ? (
          <p aria-live="polite" className="text-sm text-muted-foreground">
            {result.message}
          </p>
        ) : null}
      </div>
    </div>
  );
}
