import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { createSessionController } from "@/engine/session/session-controller";
import { SessionControllerContext } from "@/engine/session/use-session";
import { parseUiCatalog } from "@/engine/contracts/ui-catalog";
import { TableView } from "@/engine/renderers/table/TableView";

import listFixture from "@test/fixtures/demo-items-list.json";
import optionsFixture from "@test/fixtures/demo-category-options.json";
import catalogFixture from "@test/fixtures/ui-catalog.json";

/**
 * M2 集成测试（真实 TableView + 实录 Catalog fixture）：
 * bulk 删除链路、download/preview/redirect 响应处理、multipart 上传。
 */

const catalog = parseUiCatalog(catalogFixture);
const view = catalog.table_views.find(
  (candidate) => candidate.view_id === "demo.items.main",
)!;

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

type CapturedRequest = { url: string; method: string; body: unknown };

interface MockRoutes {
  onList?: () => Response;
  onBulkDelete?: (body: unknown) => Response;
  onDownload?: () => Response;
  onPreview?: () => Response;
  onRedirect?: () => Response;
  onUpload?: (body: unknown) => Response;
}

function installFetchMock(routes: MockRoutes = {}) {
  const captured: CapturedRequest[] = [];
  const fetchMock = vi.fn(
    async (input: RequestInfo | URL, init?: RequestInit) => {
      const url =
        typeof input === "string"
          ? input
          : input instanceof URL
            ? input.href
            : input.url;
      const method = init?.method ?? "GET";
      const body = init?.body;
      const parsedBody =
        body instanceof FormData
          ? body
          : body
            ? JSON.parse(String(body))
            : undefined;
      captured.push({ url, method, body: parsedBody });
      if (url.includes("/api/v1/demo/items/bulk-delete")) {
        return (
          routes.onBulkDelete?.(parsedBody) ??
          jsonResponse({
            code: 0,
            message: "已删除",
            data: { deleted: 2 },
          })
        );
      }
      if (url.includes("/api/v1/demo/items/query")) {
        return routes.onList?.() ?? jsonResponse(listFixture);
      }
      if (url.includes("/api/v1/demo/categories/options")) {
        return jsonResponse(optionsFixture);
      }
      if (url.includes("/api/v1/demo/download")) {
        return (
          routes.onDownload?.() ??
          new Response(new Blob(["验收"], { type: "text/plain" }), {
            status: 200,
            headers: {
              "content-type": "text/plain",
              "content-disposition":
                "attachment; filename*=UTF-8''%E9%AA%8C%E6%94%B6%E6%8A%A5%E5%91%8A.txt",
            },
          })
        );
      }
      if (url.includes("/api/v1/demo/preview")) {
        return (
          routes.onPreview?.() ??
          new Response(new Blob(["预览"], { type: "text/plain" }), {
            status: 200,
            headers: { "content-type": "text/plain" },
          })
        );
      }
      if (url.includes("/api/v1/demo/redirect")) {
        return (
          routes.onRedirect?.() ??
          new Response(null, {
            status: 302,
            headers: { location: "/demo/target" },
          })
        );
      }
      if (url.includes("/api/v1/demo/upload")) {
        return (
          routes.onUpload?.(parsedBody) ??
          jsonResponse({
            code: 0,
            message: "上传成功",
            data: { filename: "note.txt", size: 3 },
          })
        );
      }
      throw new Error(`测试未覆盖的请求：${method} ${url}`);
    },
  );
  vi.stubGlobal("fetch", fetchMock);
  return { fetchMock, captured };
}

function renderTableView(
  options: {
    actionEffects?: {
      handleAttachment?: (
        result: import("@/engine/http/types").InvocationResult,
      ) => void;
      redirect?: (location: string) => void;
    };
  } = {},
) {
  const controller = createSessionController();
  controller.beginSession({ accessToken: "test-access" });
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  render(
    <SessionControllerContext.Provider value={controller}>
      <QueryClientProvider client={queryClient}>
        <TableView
          view={view}
          actions={catalog.actions}
          actionEffects={options.actionEffects}
        />
      </QueryClientProvider>
    </SessionControllerContext.Provider>,
  );
  return { controller };
}

async function openToolbarOverflow(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "更多工具操作" }));
  return screen.findByRole("menu");
}

beforeEach(() => {
  Element.prototype.scrollIntoView ??= () => undefined;
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("bulk Action 链路", () => {
  it("选中多行 → 批量栏 → 确认 → 提交 selected 行数组", async () => {
    const { captured } = installFetchMock();
    renderTableView();

    // 等表格行渲染，然后选中两行。
    await waitFor(() =>
      expect(screen.getByText("平台能力")).toBeInTheDocument(),
    );
    const user = userEvent.setup();
    await user.click(screen.getByRole("checkbox", { name: "选择第 1 行" }));
    await user.click(screen.getByRole("checkbox", { name: "选择第 2 行" }));

    expect(screen.getByText("已选 2 项")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "批量删除项目" }));

    // 确认对话框
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("确认批量删除")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "确认" }));

    await waitFor(() => {
      const bulk = captured.find((request) =>
        request.url.includes("/api/v1/demo/items/bulk-delete"),
      );
      expect(bulk?.method).toBe("POST");
      // 与旧实现一致：selected 为完整选中行数组（树形构造后父行带 children）。
      const body = bulk?.body as { selected: Array<Record<string, unknown>> };
      expect(body.selected.map((row) => row.id)).toEqual([1, 2]);
      expect(body.selected[0]).toMatchObject({
        name: "平台能力",
        status: "active",
      });
      expect(body.selected[1]).toMatchObject({
        name: "通用渲染器",
        status: "draft",
      });
    });
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent("已删除"),
    );
  });
});

describe("附件与重定向响应", () => {
  it("download：生成 blob 下载并携带 content-disposition 文件名", async () => {
    const handleAttachment = vi.fn();
    installFetchMock();
    renderTableView({
      actionEffects: { handleAttachment },
    });
    await waitFor(() =>
      expect(screen.getByText("平台能力")).toBeInTheDocument(),
    );

    const user = userEvent.setup();
    const menu = await openToolbarOverflow(user);
    await user.click(within(menu).getByText("下载验收文件"));

    await waitFor(() => expect(handleAttachment).toHaveBeenCalledOnce());
    expect(handleAttachment.mock.calls[0]?.[0]).toMatchObject({
      kind: "download",
      status: 200,
      filename: "验收报告.txt",
    });
  });

  it("preview：新窗口打开 blob 预览", async () => {
    const handleAttachment = vi.fn();
    installFetchMock();
    renderTableView({
      actionEffects: { handleAttachment },
    });
    await waitFor(() =>
      expect(screen.getByText("平台能力")).toBeInTheDocument(),
    );

    const user = userEvent.setup();
    const menu = await openToolbarOverflow(user);
    await user.click(within(menu).getByText("预览验收文件"));

    await waitFor(() => expect(handleAttachment).toHaveBeenCalledOnce());
    expect(handleAttachment.mock.calls[0]?.[0]).toMatchObject({
      kind: "preview",
      status: 200,
    });
    expect(handleAttachment.mock.calls[0]?.[0]?.blobUrl).toMatch(/^blob:/);
  });

  it("redirect：跟随服务端声明的 location 跳转", async () => {
    const redirect = vi.fn();
    installFetchMock();
    renderTableView({ actionEffects: { redirect } });
    await waitFor(() =>
      expect(screen.getByText("平台能力")).toBeInTheDocument(),
    );

    const user = userEvent.setup();
    const menu = await openToolbarOverflow(user);
    await user.click(within(menu).getByText("重定向验收"));

    await waitFor(() => expect(redirect).toHaveBeenCalledWith("/demo/target"));
  });
});

describe("multipart 上传", () => {
  it("文件选择 + 文本字段 → FormData 提交", async () => {
    const { captured } = installFetchMock();
    renderTableView();
    await waitFor(() =>
      expect(screen.getByText("平台能力")).toBeInTheDocument(),
    );

    const user = userEvent.setup();
    const menu = await openToolbarOverflow(user);
    await user.click(within(menu).getByText("上传验收文件"));

    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText(/标题|title/i), "周报");
    const fileInput = within(dialog).getByLabelText(/file/i, {
      selector: "input[type=file]",
    });
    await user.upload(
      fileInput,
      new File(["hello"], "note.txt", { type: "text/plain" }),
    );
    await user.click(within(dialog).getByRole("button", { name: "提交" }));

    await waitFor(() => {
      const upload = captured.find(
        (request) =>
          request.url.includes("/api/v1/demo/upload") &&
          request.method === "POST",
      );
      expect(upload).toBeDefined();
      const form = upload?.body;
      expect(form).toBeInstanceOf(FormData);
      const data = form as unknown as FormData;
      expect(data.get("title")).toBe("周报");
      expect(data.get("file")).toBeInstanceOf(File);
      expect((data.get("file") as File).name).toBe("note.txt");
    });
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent("上传成功"),
    );
  });
});
