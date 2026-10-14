/**
 * 公开门户查询页的行为测试。
 *
 * 走**真实路由** `/portal/:slug`：本页是项目里第一个不在 `RequireAuth` 门控内的
 * 业务页面，所以第一条用例就是「未登录也能打开」——如果哪天有人把它挪进 `/` 那条
 * 受保护路由的 children，那条用例会立刻变红（页面会被门控顶成登录页/骨架）。
 *
 * 页面 mount 时先取**公开元信息**（标题 / 查询键提示 / 访问模式），再用它渲染；
 * 查询本身的失败码（`40301` / `40401`）仍是兜底分支，所以两路都要钉住。
 */

import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { formatCardNumber, PORTAL_ERROR_CODES } from "@/features/feishu/types";
import { renderTestApp } from "@test/helpers/render-app";

const QUERY_PATH = "/api/v1/feishu/portal/bank/query";
const METADATA_PATH = "/api/v1/feishu/portal/bank";
const SUGGEST_PATH = "/api/v1/feishu/portal/bank/suggest";

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

type PortalMetadataBody = {
  title: string;
  query_field_label: string;
  access_mode: "public" | "passphrase" | "login";
  empty_message: string;
  fuzzy_enabled: boolean;
  fuzzy_min_digits: number;
};

/// 后端元信息端点的默认回执（`GET /api/v1/feishu/portal/{slug}`）。
function metadataBody(overrides: Partial<PortalMetadataBody> = {}) {
  return {
    title: "信用卡查询",
    query_field_label: "完整卡号",
    access_mode: "public",
    empty_message: "未找到该卡号的记录",
    fuzzy_enabled: true,
    fuzzy_min_digits: 4,
    ...overrides,
  };
}

/// 元信息 / 查询 / 建议三个端点各自独立可编排：元信息默认成功，查询默认未命中，
/// 建议默认空列表。
function stubPortal(
  options: {
    metadata?: () => Response;
    query?: () => Response;
    suggest?: () => Response;
  } = {},
) {
  const metadata = options.metadata ?? (() => envelope(metadataBody()));
  const query =
    options.query ?? (() => envelope({ found: false, columns: [], rows: [] }));
  const suggest = options.suggest ?? (() => envelope({ suggestions: [] }));
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      // 先判建议：它的路径是元信息路径多一段 `/suggest?query=…`。
      if (url.includes(SUGGEST_PATH)) return suggest();
      // 再判查询：它的路径是元信息路径多一段 `/query`。
      if (url.endsWith(QUERY_PATH)) return query();
      if (url.endsWith(METADATA_PATH)) return metadata();
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
    stubPortal();
    renderPortal();

    expect(await screen.findByLabelText("查询")).toBeInTheDocument();
    // 受保护页在未登录时会被 RequireAuth 顶去 /login —— 这里必须没有。
    expect(screen.queryByText("正在恢复会话…")).toBeNull();
  });

  it("元信息给出标题与查询键提示：标题用门户的，placeholder 用查询键展示名", async () => {
    stubPortal();
    renderPortal();

    expect(
      await screen.findByRole("heading", { name: "信用卡查询" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("查询")).toHaveAttribute(
      "placeholder",
      "完整卡号",
    );
  });

  it("元信息说 passphrase：口令框预先展开，不必先吃一次 40301", async () => {
    stubPortal({
      metadata: () => envelope(metadataBody({ access_mode: "passphrase" })),
    });
    renderPortal();

    expect(await screen.findByLabelText("口令")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "查询" })).toBeDisabled();
  });

  it("元信息说 login 且当前无会话：直接跳登录页并带回跳地址", async () => {
    stubPortal({
      metadata: () => envelope(metadataBody({ access_mode: "login" })),
    });
    const { router } = renderPortal();

    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/login");
    });
    expect(router.state.location.search).toContain(
      `redirect=${encodeURIComponent("/portal/bank")}`,
    );
  });

  it("元信息 40401（HTTP 200）→ 门户不存在或已停用", async () => {
    stubPortal({
      metadata: () =>
        businessFailure(PORTAL_ERROR_CODES.notFound, "门户不存在"),
    });
    renderPortal();

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "门户不存在或已停用",
    );
    expect(screen.queryByLabelText("查询")).toBeNull();
  });

  it("查询命中：列由后端给，表格按它渲染", async () => {
    stubPortal({
      query: () =>
        envelope({
          found: true,
          columns: [
            { field_id: "fld_limit", label: "额度", field_type: "number" },
          ],
          rows: [{ values: { fld_limit: 50000 } }],
          message: "",
        }),
    });
    renderPortal();

    await submitQuery();

    expect(
      await screen.findByRole("columnheader", { name: "额度" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "50000" })).toBeInTheDocument();
  });

  it("未命中：显示后端给的 message", async () => {
    stubPortal({
      query: () =>
        envelope({
          found: false,
          columns: [],
          rows: [],
          message: "未查询到该卡号",
        }),
    });
    renderPortal();

    await submitQuery();

    expect(await screen.findByText("未查询到该卡号")).toBeInTheDocument();
  });

  it("未命中且查询响应没给文案时，退回元信息里的 empty_message", async () => {
    stubPortal({
      query: () =>
        envelope({ found: false, columns: [], rows: [], message: "" }),
    });
    renderPortal();

    await submitQuery();

    expect(await screen.findByText("未找到该卡号的记录")).toBeInTheDocument();
  });

  it("命中但被截断时，表格与提示**同时**出现（提示不能只在空结果时画）", async () => {
    stubPortal({
      query: () =>
        envelope({
          found: true,
          columns: [
            { field_id: "fld_limit", label: "额度", field_type: "number" },
          ],
          rows: [{ values: { fld_limit: 50000 } }],
          message: "匹配结果超过 100 条，仅显示前 100 条",
        }),
    });
    renderPortal();

    await submitQuery();

    expect(
      await screen.findByRole("columnheader", { name: "额度" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("匹配结果超过 100 条，仅显示前 100 条"),
    ).toBeInTheDocument();
  });

  it("40401（HTTP 200）→ 门户不存在或已停用", async () => {
    stubPortal({
      query: () => businessFailure(PORTAL_ERROR_CODES.notFound, "门户不存在"),
    });
    renderPortal();

    await submitQuery();

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "门户不存在或已停用",
    );
  });

  it("40301 口令不正确 → 展开口令框，且口令为空时按钮不可用", async () => {
    stubPortal({
      query: () => businessFailure(PORTAL_ERROR_CODES.forbidden, "口令不正确"),
    });
    renderPortal();

    expect(await screen.findByLabelText("查询")).toBeInTheDocument();
    expect(screen.queryByLabelText("口令")).toBeNull();
    await submitQuery();

    expect(await screen.findByLabelText("口令")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "查询" })).toBeDisabled();
    await userEvent.type(screen.getByLabelText("口令"), "secret-pass");
    expect(screen.getByRole("button", { name: "查询" })).toBeEnabled();
  });

  it("40301 需要登录 → 跳登录页并带回跳地址", async () => {
    stubPortal({
      query: () =>
        businessFailure(PORTAL_ERROR_CODES.forbidden, "需要登录后才能查询"),
    });
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
    stubPortal({
      query: () => jsonResponse({ code: 42901, message: "too many" }, 429),
    });
    renderPortal();

    await submitQuery();

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "查询过于频繁，请稍后再试",
    );
  });

  it("输入卡号片段：去抖后出下拉建议，选中填入完整卡号（带空格）", async () => {
    stubPortal({
      suggest: () =>
        envelope({
          suggestions: ["4514 6175 9317 4990", "4514 6175 6142 8030"],
        }),
    });
    renderPortal();

    const input = await screen.findByLabelText("查询");
    await userEvent.type(input, "45146175");

    // 输入过程中每 4 位自动分组：光标处的值应是 "4514 6175"。
    expect(input).toHaveValue("4514 6175");

    // 去抖 300ms 后出下拉（建议是库里的标准形态，带空格）。
    const first = await screen.findByText("4514 6175 9317 4990");
    await userEvent.click(first);

    expect(input).toHaveValue("4514 6175 9317 4990");
  });

  it("输入不足最少连续数字时不请求建议（门槛取门户配置）", async () => {
    const fetchSpy = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes(SUGGEST_PATH)) return envelope({ suggestions: [] });
      if (url.endsWith(METADATA_PATH)) return envelope(metadataBody());
      return jsonResponse({ code: 401, message: "no session" }, 401);
    });
    vi.stubGlobal("fetch", fetchSpy);
    renderPortal();

    const input = await screen.findByLabelText("查询");
    await userEvent.type(input, "123");
    // 等超过去抖窗口，确认 suggest 端点从未被调用。
    await new Promise((resolve) => setTimeout(resolve, 400));
    const suggestCalls = fetchSpy.mock.calls.filter(([url]) =>
      String(url).includes(SUGGEST_PATH),
    );
    expect(suggestCalls).toHaveLength(0);
  });
});

describe("formatCardNumber", () => {
  it("无空格卡号按 4 位分组补空格", () => {
    expect(formatCardNumber("4514617593174990")).toBe("4514 6175 9317 4990");
  });

  it("已是标准形态的输入保持不变（多了的空格被删）", () => {
    expect(formatCardNumber("4514 6175 9317 4990")).toBe("4514 6175 9317 4990");
    expect(formatCardNumber("4514  6175")).toBe("4514 6175");
  });

  it("不足一组或末尾不足一组时正确分组", () => {
    expect(formatCardNumber("451461")).toBe("4514 61");
    expect(formatCardNumber("4514")).toBe("4514");
    expect(formatCardNumber("")).toBe("");
  });

  it("含数字与空格之外字符的输入原样返回（门户查询键不一定是卡号）", () => {
    expect(formatCardNumber("abc1234")).toBe("abc1234");
    expect(formatCardNumber("4514-6175")).toBe("4514-6175");
    expect(formatCardNumber("卡号4514")).toBe("卡号4514");
  });
});
