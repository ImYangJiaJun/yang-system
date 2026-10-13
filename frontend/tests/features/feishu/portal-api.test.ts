/**
 * 门户 API 层的单测：请求体形状、公开端点解析、失败码分类、JSON 列的容错解析。
 *
 * 管理侧的 Action 调用**用一份手写目录**驱动（与真实目录同形）；公开侧直接桩 fetch。
 * 断言全部对着后端源码里的真实契约（`src/addon/feishu/portal/actions/*.rs`）：
 * 主键键名是 `portal_id`、`update_portal` 不收口令、业务失败走信封 code 而非 HTTP 状态。
 */

import { afterEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "@/engine";
import type { UiCatalog } from "@/engine";

import {
  createPortal,
  listPortals,
  portalFailureView,
  queryPortal,
  setPassphrase,
  togglePortal,
  updatePortal,
} from "@/features/feishu/portal-api";
import { PORTAL_ERROR_CODES } from "@/features/feishu/types";
import {
  parsePortalColumns,
  parsePortalLabels,
  portalPathSlug,
} from "@/features/feishu/types";

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function envelope(data: unknown): Response {
  return jsonResponse({ code: 0, message: "成功", data });
}

function action(operationId: string, method: string, path: string) {
  return {
    operation_id: operationId,
    title: operationId,
    description: "",
    method,
    path,
    params: [],
    input_schema: {},
    output_schema: {},
    request_media_type: "json",
    response_kind: "json",
    requires_auth: true,
  };
}

const CATALOG = {
  schemaVersion: "2.3",
  revision: "a".repeat(64),
  actions: [
    action(
      "feishu.portal.list_portals",
      "POST",
      "/api/v1/feishu/portals/query",
    ),
    action("feishu.portal.create_portal", "POST", "/api/v1/feishu/portals"),
    action("feishu.portal.update_portal", "PUT", "/api/v1/feishu/portals"),
    action("feishu.portal.delete_portal", "DELETE", "/api/v1/feishu/portals"),
    action(
      "feishu.portal.toggle_portal",
      "PUT",
      "/api/v1/feishu/portals/toggle",
    ),
    action(
      "feishu.portal.set_passphrase",
      "PUT",
      "/api/v1/feishu/portals/passphrase",
    ),
  ],
  tableViews: [],
  modules: [],
} as unknown as UiCatalog;

const DEPS = { catalog: CATALOG, session: { token: "t" } };

/// 记录每一次请求的 URL + 解析后的 body。
function recordFetch(reply: (url: string, body: unknown) => Response) {
  const calls: Array<{ url: string; body: Record<string, unknown> }> = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const body = init?.body
        ? (JSON.parse(String(init.body)) as Record<string, unknown>)
        : {};
      calls.push({ url, body });
      return reply(url, body);
    }),
  );
  return calls;
}

function portalWire(overrides: Record<string, unknown> = {}) {
  return {
    id: 1,
    datasource_id: 2,
    title: "卡查询",
    slug: "uuid-1",
    custom_slug: null,
    query_field_id: "fld_card",
    query_field_source_key: "card_no",
    query_field_ids: null,
    access_mode: "public",
    exposed_columns: '["fld_limit"]',
    result_labels: null,
    empty_message: null,
    rate_limit_per_min: 30,
    enabled: true,
    created_at: 100,
    updated_at: 100,
    ...overrides,
  };
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("门户 JSON 列的容错解析", () => {
  it("数组列：认不出形状时回空数组，而不是把 JSON 原文当字段名", () => {
    expect(parsePortalColumns('["a","b"]')).toEqual(["a", "b"]);
    expect(parsePortalColumns(null)).toEqual([]);
    expect(parsePortalColumns("")).toEqual([]);
    expect(parsePortalColumns("{坏掉的")).toEqual([]);
    expect(parsePortalColumns('{"a":1}')).toEqual([]);
  });

  it("展示名映射：丢掉非字符串值", () => {
    expect(parsePortalLabels('{"a":"甲","b":2}')).toEqual({ a: "甲" });
    expect(parsePortalLabels("坏掉的")).toEqual({});
    expect(parsePortalLabels(null)).toEqual({});
  });
});

describe("portalPathSlug", () => {
  const base = portalWire();

  it("优先自定义短链，空/空白/缺失回落 UUID", () => {
    expect(portalPathSlug({ ...base, custom_slug: "bank" } as never)).toBe(
      "bank",
    );
    expect(portalPathSlug({ ...base, custom_slug: "" } as never)).toBe(
      "uuid-1",
    );
    expect(portalPathSlug({ ...base, custom_slug: "   " } as never)).toBe(
      "uuid-1",
    );
    expect(portalPathSlug({ ...base, custom_slug: null } as never)).toBe(
      "uuid-1",
    );
  });
});

describe("listPortals 请求体", () => {
  it("按 datasource_id 等值过滤，且排序恒带唯一键收尾", async () => {
    const calls = recordFetch(() =>
      envelope({ items: [portalWire()], page: 1, page_size: 100, total: 1 }),
    );

    const page = await listPortals(2, DEPS);

    expect(calls[0]?.url).toBe("/api/v1/feishu/portals/query");
    expect(calls[0]?.body.where).toEqual({
      type: "eq",
      field: "datasource_id",
      value: 2,
    });
    expect(calls[0]?.body.order_by).toEqual([
      { field: "created_at", direction: "Desc" },
      { field: "id", direction: "Asc" },
    ]);
    expect(page.items[0]?.title).toBe("卡查询");
  });

  it("缺 id/slug 的行被丢掉，而不是降级成一条没有主键的门户", async () => {
    recordFetch(() =>
      envelope({
        items: [
          portalWire(),
          portalWire({ id: null }),
          portalWire({ slug: "" }),
        ],
        page: 1,
        page_size: 100,
        total: 3,
      }),
    );

    const page = await listPortals(2, DEPS);
    expect(page.items).toHaveLength(1);
  });
});

describe("管理侧写操作的键名", () => {
  it("create：空短链**省略该键**（后端对空串判参数错），回执只取 portal_id/slug", async () => {
    const calls = recordFetch(() => envelope({ portal_id: 7, slug: "uuid-7" }));

    const created = await createPortal(
      {
        datasource_id: 2,
        title: "卡查询",
        query_field_id: "fld_card",
        exposed_columns: ["fld_limit"],
        access_mode: "public",
        custom_slug: "",
      },
      DEPS,
    );

    expect(calls[0]?.body.custom_slug).toBeUndefined();
    expect(calls[0]?.body.datasource_id).toBe(2);
    expect(created).toEqual({ portalId: 7, slug: "uuid-7" });
  });

  it("update：主键键名是 portal_id，且**不发 passphrase**（后端不收）", async () => {
    const calls = recordFetch(() => envelope({ portal_id: 7 }));

    await updatePortal(
      { portal_id: 7, title: "改个名", custom_slug: "" },
      DEPS,
    );

    expect(calls[0]?.url).toBe("/api/v1/feishu/portals");
    expect(calls[0]?.body.portal_id).toBe(7);
    expect(calls[0]?.body).not.toHaveProperty("passphrase");
    // 显式空串 = 清空短链，这个键要照发。
    expect(calls[0]?.body.custom_slug).toBe("");
  });

  it("toggle / set_passphrase 都用 portal_id", async () => {
    const calls = recordFetch((url) =>
      url.endsWith("/toggle")
        ? envelope({ portal_id: 7, enabled: false })
        : envelope({ portal_id: 7, expires_at: null }),
    );

    const toggled = await togglePortal(7, false, DEPS);
    await setPassphrase(7, "secret-pass", DEPS);

    expect(calls[0]?.body).toEqual({ portal_id: 7, enabled: false });
    expect(calls[1]?.body).toEqual({ portal_id: 7, passphrase: "secret-pass" });
    expect(toggled).toEqual({ portalId: 7, enabled: false });
  });
});

describe("公开查询端点", () => {
  it("POST 到 /query，口令为空时不发 passphrase 键", async () => {
    const calls = recordFetch(() =>
      envelope({
        found: true,
        columns: [
          { field_id: "fld_limit", label: "额度", field_type: "number" },
        ],
        rows: [{ values: { fld_limit: 50000 } }],
        message: "",
      }),
    );

    const result = await queryPortal("bank", "6222", undefined);

    expect(calls[0]?.url).toBe("/api/v1/feishu/portal/bank/query");
    expect(calls[0]?.body).toEqual({ value: "6222" });
    expect(result.found).toBe(true);
    expect(result.columns[0]?.label).toBe("额度");
    expect(result.rows[0]?.values.fld_limit).toBe(50000);
  });
});

describe("公开端点的失败分类（按信封 code，不按 HTTP 状态）", () => {
  it("40401 → 门户不存在（HTTP 仍是 200）", () => {
    const error = new ApiError("门户不存在", {
      status: 200,
      code: PORTAL_ERROR_CODES.notFound,
    });
    expect(portalFailureView(error)).toEqual({
      message: "门户不存在",
      revealPassphrase: false,
      loginRequired: false,
    });
  });

  it("40301 + 口令文案 → 展开口令框", () => {
    const error = new ApiError("口令不正确", {
      status: 200,
      code: PORTAL_ERROR_CODES.forbidden,
    });
    expect(portalFailureView(error)).toEqual({
      message: "口令不正确",
      revealPassphrase: true,
      loginRequired: false,
    });
  });

  it("40301 + 登录文案 → 送登录页", () => {
    const error = new ApiError("需要登录后才能查询", {
      status: 200,
      code: PORTAL_ERROR_CODES.forbidden,
    });
    expect(portalFailureView(error)).toEqual({
      message: "需要登录后才能查询",
      revealPassphrase: false,
      loginRequired: true,
    });
  });

  it("429（唯一的真 Err）→ 限速话术", () => {
    const error = new ApiError("too many", { status: 429 });
    expect(portalFailureView(error).message).toBe("查询过于频繁，请稍后再试");
  });
});
