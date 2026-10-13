/**
 * 公开门户查询页的行为测试。
 *
 * 走**真实路由** `/portal/:slug`：本页是项目里第一个不在 `RequireAuth` 门控内的
 * 业务页面，所以第一条用例就是「未登录也能打开」——如果哪天有人把它挪进 `/` 那条
 * 受保护路由的 children，那条用例会立刻变红（页面会被门控顶成登录页/骨架）。
 *
 * 访问控制是**反应式**的（后端没有门户元信息端点），所以口令框与登录跳转都靠
 * 后端失败码驱动：`40301` + 文案。
 */

import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { PORTAL_ERROR_CODES } from "@/features/feishu/types";
import { renderTestApp } from "@test/helpers/render-app";

const QUERY_PATH = "/api/v1/feishu/portal/bank/query";

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function envelope(data: unknown): Response {
  return jsonResponse({ code: 0, message: "成功", data });
}

/// 业务失败：HTTP 200 + 非零信封 code（后端 `ApiResponse::fail` 的真实形态）。
function businessFailure(code: number, message: string): Response {
  return jsonResponse({ code, message, data: null });
}

function stubQuery(reply: () => Response) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith(QUERY_PATH)) return reply();
      // 会话恢复的 refresh 端点：一律 401（终端失败 → 匿名）。
      return jsonResponse({ code: 401, message: "no session" }, 401);
    }),
  );
}

function renderPortal() {
  return renderTestApp({ path: "/portal/bank", authenticated: false });
}

/// 填一个查询值并提交。
async function submitQuery(value = "6222") {
  await userEvent.type(await screen.findByLabelText("查询"), value);
  await userEvent.click(screen.getByRole("button", { name: "查询" }));
}

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("公开门户查询页", () => {
  it("未登录也能打开：不落登录页、不被会话骨架挡住", async () => {
    stubQuery(() => envelope({ found: false, columns: [], rows: [] }));
    renderPortal();

    expect(await screen.findByLabelText("查询")).toBeInTheDocument();
    // 受保护页在未登录时会被 RequireAuth 顶去 /login —— 这里必须没有。
    expect(screen.queryByText("正在恢复会话…")).toBeNull();
  });

  it("查询命中：列由后端给，表格按它渲染", async () => {
    stubQuery(() =>
      envelope({
        found: true,
        columns: [
          { field_id: "fld_limit", label: "额度", field_type: "number" },
        ],
        rows: [{ values: { fld_limit: 50000 } }],
        message: "",
      }),
    );
    renderPortal();

    await submitQuery();

    expect(
      await screen.findByRole("columnheader", { name: "额度" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "50000" })).toBeInTheDocument();
  });

  it("未命中：显示后端给的 message", async () => {
    stubQuery(() =>
      envelope({
        found: false,
        columns: [],
        rows: [],
        message: "未查询到该卡号",
      }),
    );
    renderPortal();

    await submitQuery();

    expect(await screen.findByText("未查询到该卡号")).toBeInTheDocument();
  });

  it("命中但被截断时，表格与提示**同时**出现（提示不能只在空结果时画）", async () => {
    stubQuery(() =>
      envelope({
        found: true,
        columns: [
          { field_id: "fld_limit", label: "额度", field_type: "number" },
        ],
        rows: [{ values: { fld_limit: 50000 } }],
        message: "匹配结果超过 100 条，仅显示前 100 条",
      }),
    );
    renderPortal();

    await submitQuery();

    expect(
      await screen.findByRole("columnheader", { name: "额度" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("匹配结果超过 100 条，仅显示前 100 条"),
    ).toBeInTheDocument();
  });

  it("40401（HTTP 200）→ 门户不存在", async () => {
    stubQuery(() => businessFailure(PORTAL_ERROR_CODES.notFound, "门户不存在"));
    renderPortal();

    await submitQuery();

    expect(await screen.findByRole("alert")).toHaveTextContent("门户不存在");
  });

  it("40301 口令不正确 → 展开口令框，且口令为空时按钮不可用", async () => {
    stubQuery(() =>
      businessFailure(PORTAL_ERROR_CODES.forbidden, "口令不正确"),
    );
    renderPortal();

    expect(screen.queryByLabelText("口令")).toBeNull();
    await submitQuery();

    expect(await screen.findByLabelText("口令")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "查询" })).toBeDisabled();
    await userEvent.type(screen.getByLabelText("口令"), "secret-pass");
    expect(screen.getByRole("button", { name: "查询" })).toBeEnabled();
  });

  it("40301 需要登录 → 跳登录页并带回跳地址", async () => {
    stubQuery(() =>
      businessFailure(PORTAL_ERROR_CODES.forbidden, "需要登录后才能查询"),
    );
    const { router } = renderPortal();

    await submitQuery();

    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/login");
    });
    expect(router.state.location.search).toContain(
      `redirect=${encodeURIComponent("/portal/bank")}`,
    );
  });

  it("429 → 查询过于频繁", async () => {
    stubQuery(() => jsonResponse({ code: 42901, message: "too many" }, 429));
    renderPortal();

    await submitQuery();

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "查询过于频繁，请稍后再试",
    );
  });
});
