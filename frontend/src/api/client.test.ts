import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ApiError,
  fetchUiCatalog,
  invokeAction,
  StepUpRequiredError,
} from "./client";
import {
  activeAccessToken,
  clearStoredSession,
  persistTokenPair,
  SESSION_RELOGIN_REQUIRED_EVENT,
} from "./auth-session";
import type { ActionDemoSchema, UiCatalog } from "src/contracts/ui-catalog";

const action: ActionDemoSchema = {
  operation_id: "demo.update",
  title: "更新",
  description: "",
  method: "POST",
  path: "/api/items/{id}",
  params: [
    {
      name: "id",
      source: "path",
      required: true,
      title: "ID",
      description: "",
    },
    {
      name: "page",
      source: "query",
      required: false,
      title: "页码",
      description: "",
    },
    {
      name: "trace",
      source: "header",
      required: false,
      title: "跟踪",
      description: "",
    },
    {
      name: "name",
      source: "body",
      required: true,
      title: "名称",
      description: "",
    },
  ],
  input_schema: {},
  output_schema: {},
  request_media_type: "json",
  response_kind: "json",
  requires_auth: true,
};

afterEach(() => {
  clearStoredSession();
  sessionStorage.clear();
  vi.unstubAllGlobals();
});

describe("fetchUiCatalog", () => {
  it("发送 revision ETag，并在 304 时复用不可变目录", async () => {
    const cached: UiCatalog = {
      schema_version: "2.3",
      revision: "a".repeat(64),
      actions: [],
      table_views: [],
      modules: [],
    };
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_url: string, init: RequestInit) => {
        expect(new Headers(init.headers).get("if-none-match")).toBe(
          `"${cached.revision}"`,
        );
        return new Response(null, { status: 304 });
      }),
    );

    await expect(fetchUiCatalog({}, undefined, cached)).resolves.toBe(cached);
  });
});

describe("invokeAction", () => {
  it("把合法 428 解析为不携带 details 的 Step-up challenge", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              code: 40110,
              message: "敏感操作需要重新认证",
              data: { challenge: "signed-challenge", expires_in: 120 },
            }),
            { status: 428, headers: { "content-type": "application/json" } },
          ),
      ),
    );

    const error = await invokeAction(action, { id: 1, name: "A" }, {}).catch(
      (cause: unknown) => cause,
    );
    expect(error).toBeInstanceOf(StepUpRequiredError);
    expect(error).toMatchObject({
      status: 428,
      challenge: "signed-challenge",
      expiresIn: 120,
      details: undefined,
    });
  });

  it("仅把调用方提供的临时 proof 注入当前请求头", async () => {
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      expect(new Headers(init.headers).get("x-step-up-proof")).toBe(
        "one-shot-proof",
      );
      return new Response(
        JSON.stringify({ code: 0, message: "成功", data: { ok: true } }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    await invokeAction(action, { id: 1, name: "A" }, {}, undefined, {
      stepUpProof: "one-shot-proof",
    });
    expect(JSON.stringify({ ...sessionStorage })).not.toContain(
      "one-shot-proof",
    );
    expect(JSON.stringify({ ...localStorage })).not.toContain("one-shot-proof");
  });

  it("拒绝缺字段或越界 TTL 的伪 428 契约", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              code: 40110,
              data: { challenge: "signed-challenge", expires_in: 301 },
            }),
            { status: 428, headers: { "content-type": "application/json" } },
          ),
      ),
    );

    const error = await invokeAction(action, { id: 1, name: "A" }, {}).catch(
      (cause: unknown) => cause,
    );
    expect(error).toBeInstanceOf(ApiError);
    expect(error).not.toBeInstanceOf(StepUpRequiredError);
  });

  it("凭据变更成功后清除 Access 状态并发出重新登录事件", async () => {
    persistTokenPair({ accessToken: "access-before-change" });
    const relogin = vi.fn();
    window.addEventListener(SESSION_RELOGIN_REQUIRED_EVENT, relogin);
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              code: 0,
              message: "密码已修改",
              data: { relogin_required: true },
            }),
            { status: 200, headers: { "content-type": "application/json" } },
          ),
      ),
    );

    await expect(
      invokeAction(
        {
          ...action,
          operation_id: "account.user.change_password",
          path: "/api/v1/users/change-password",
          params: [],
        },
        { old_password: "old", new_password: "new" },
        { token: "access-before-change" },
      ),
    ).resolves.toMatchObject({ data: { relogin_required: true } });

    expect(activeAccessToken()).toBeUndefined();
    expect(relogin).toHaveBeenCalledOnce();
    window.removeEventListener(SESSION_RELOGIN_REQUIRED_EVENT, relogin);
  });

  it("按来源构建 path/query/header/body 且注入会话上下文", async () => {
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      const headers = new Headers(init.headers);
      expect(headers.get("authorization")).toBe("Bearer secret");
      expect(headers.get("x-tenant-id")).toBe("7");
      expect(headers.get("trace")).toBe("abc");
      expect(init.body).toBe(JSON.stringify({ name: "A" }));
      return new Response(
        JSON.stringify({ code: 0, message: "成功", data: { ok: true } }),
        {
          status: 200,
          headers: {
            "content-type": "application/json",
            "x-request-id": "req-1",
          },
        },
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    const result = await invokeAction(
      action,
      { id: "a/b", page: 2, trace: "abc", name: "A" },
      { token: "secret", tenantId: "7" },
    );

    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/items/a%2Fb?page=2");
    expect(result).toMatchObject({
      kind: "json",
      status: 200,
      requestId: "req-1",
      data: { ok: true },
    });
  });

  it("业务 code 非零即使 HTTP 200 也作为失败", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(JSON.stringify({ code: 40001, message: "拒绝" }), {
            status: 200,
            headers: { "content-type": "application/json" },
          }),
      ),
    );
    const error = await invokeAction(action, { id: 1, name: "A" }, {}).catch(
      (cause: unknown) => cause,
    );
    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({ code: 40001, message: "拒绝" });
  });

  it("无参数映射的 JSON Action 把完整输入作为请求体", async () => {
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      expect(init.body).toBe(
        JSON.stringify({ page: 1, page_size: 20, search: "Alice" }),
      );
      return new Response(
        JSON.stringify({ code: 0, message: "成功", data: { items: [] } }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    await invokeAction(
      {
        ...action,
        operation_id: "demo.items.select",
        path: "/api/items/query",
        params: [],
      },
      { page: 1, page_size: 20, search: "Alice" },
      {},
    );
  });

  it("收到 401 后使用刷新结果重建并重试 Action 请求", async () => {
    sessionStorage.setItem("yang.token", "access-old");
    const authorizations: Array<string | null> = [];
    const fetchMock = vi.fn(async (url: string, init: RequestInit) => {
      if (url === "/api/v1/users/refresh") {
        return new Response(
          JSON.stringify({
            code: 0,
            message: "成功",
            data: {
              access_token: "access-new",
            },
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }
      authorizations.push(new Headers(init.headers).get("authorization"));
      if (authorizations.length === 1) {
        return new Response(
          JSON.stringify({ code: 40102, message: "Token 已过期" }),
          { status: 401, headers: { "content-type": "application/json" } },
        );
      }
      return new Response(
        JSON.stringify({ code: 0, message: "成功", data: { ok: true } }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      invokeAction(action, { id: 1, name: "A" }, { token: "access-old" }),
    ).resolves.toMatchObject({ data: { ok: true } });
    expect(authorizations).toEqual(["Bearer access-old", "Bearer access-new"]);
  });

  it("multipart Action 使用 FormData 并在发送前执行 MIME 与大小边界", async () => {
    const uploadAction: ActionDemoSchema = {
      ...action,
      path: "/api/upload",
      params: [],
      request_media_type: "multipart",
      multipart: {
        max_fields: 1,
        max_files: 1,
        max_file_bytes: 8,
        max_text_field_bytes: 16,
        max_total_bytes: 24,
        allowed_content_types: ["text/plain"],
        lifecycle: "request_scoped",
      },
    };
    const fetchMock = vi.fn(async (_url: string, init: RequestInit) => {
      if (!(init.body instanceof FormData))
        throw new Error("multipart 请求体必须是 FormData");
      const form = init.body;
      expect(form.get("title")).toBe("报告");
      expect(form.get("file")).toBeInstanceOf(File);
      return new Response(
        JSON.stringify({ code: 0, message: "成功", data: { size: 5 } }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    await invokeAction(
      uploadAction,
      {
        title: "报告",
        file: new File(["hello"], "report.txt", { type: "text/plain" }),
      },
      {},
    );
    await expect(
      invokeAction(
        uploadAction,
        {
          title: "报告",
          file: new File(["blocked"], "report.pdf", {
            type: "application/pdf",
          }),
        },
        {},
      ),
    ).rejects.toThrow("不允许的文件类型");
  });

  it("把浏览器 opaqueredirect 识别为已拦截重定向而不是 HTTP 0 错误", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          ({
            type: "opaqueredirect",
            status: 0,
            headers: new Headers(),
          }) as Response,
      ),
    );
    const result = await invokeAction(
      {
        ...action,
        method: "GET",
        path: "/api/redirect",
        params: [],
        response_kind: "redirect",
      },
      {},
      {},
    );
    expect(result).toMatchObject({
      kind: "redirect",
      status: 0,
      location: undefined,
    });
  });
});
