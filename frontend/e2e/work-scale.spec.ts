import { expect, test, type Page, type Route } from "@playwright/test";

const treeItems = Array.from({ length: 100 }, (_, index) => ({
  id: index + 1,
  parent_task: index === 0 ? null : index,
  title: `树任务-${String(index).padStart(3, "0")}`,
  status: "todo",
}));

function action(operationId: string, title: string, path: string) {
  return {
    operation_id: operationId,
    title,
    description: title,
    method: "POST",
    path,
    params: [],
    input_schema: { type: "object", properties: {} },
    output_schema: { type: "object" },
    request_media_type: "json",
    response_kind: "json",
    requires_auth: true,
  };
}

function workView(viewId: string, title: string, tree = false) {
  return {
    view_id: viewId,
    title,
    table: "work_task",
    data_action: "work.task.select",
    columns: [
      {
        field: "title",
        title: "任务标题",
        description: "任务标题",
        widget: "text",
        required: true,
        searchable: true,
        filterable: true,
        sortable: true,
        filter: {
          operators: ["contains"],
          default_operator: "contains",
        },
      },
      {
        field: "status",
        title: "任务状态",
        description: "任务状态",
        widget: "radio",
        required: true,
        searchable: false,
        filterable: true,
        sortable: true,
        display: {
          kind: "status",
          options: [
            { value: "todo", label: "待处理", tone: "neutral" },
            { value: "done", label: "已完成", tone: "positive" },
          ],
        },
        filter: { operators: ["eq"], default_operator: "eq" },
      },
    ],
    form: { fields: [] },
    ...(tree
      ? {
          tree: {
            id_field: "id",
            parent_field: "parent_task",
            label_field: "title",
            max_nodes: 100,
          },
        }
      : {}),
    query: {
      search_fields: ["title"],
      filter_fields: ["title", "status"],
      default_sort: [{ field: "title", direction: "asc" }],
      default_page_size: 20,
      max_page_size: 100,
    },
    actions: ["work.task.complete"],
    action_presentations: [
      {
        operation_id: "work.task.complete",
        title: "批量完成",
        placement: "bulk",
        interaction: "invoke",
        confirmation: {
          title: "批量完成任务",
          message: "只会更新当前工作区内已选择的任务",
        },
      },
    ],
  };
}

async function restoreSession(page: Page) {
  await page.addInitScript(() => {
    sessionStorage.setItem("yang.account-identity", "user");
  });
  await page.route("**/api/v1/users/refresh", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { access_token: "work-scale-token" },
      }),
      headers: {
        "Set-Cookie":
          "yang_refresh=work-scale-refresh; Path=/api/v1/users; HttpOnly; SameSite=Strict",
      },
    }),
  );
}

async function fulfillJson(
  route: Route,
  data: Record<string, unknown>,
  delayMs = 0,
) {
  if (delayMs) await new Promise((resolve) => setTimeout(resolve, delayMs));
  try {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ code: 0, message: "成功", data }),
    });
  } catch {
    // 旧请求会被 AbortController 主动断开；服务端迟到响应无需再写入浏览器。
  }
}

test("真实任务 View 在弱网乱序下保留新结果并安全批量 100 条", async ({
  page,
}) => {
  await restoreSession(page);
  await page.route("**/.well-known/yang/ui-catalog", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          schema_version: "2.3",
          revision: "9".repeat(64),
          actions: [
            action("work.task.select", "查询任务", "/api/v1/work/tasks/query"),
            action(
              "work.task.complete",
              "批量完成",
              "/api/v1/work/tasks/complete",
            ),
          ],
          table_views: [
            workView("work.task.outline", "任务树", true),
            workView("work.task.backlog", "任务清单"),
          ],
          modules: [
            {
              module_id: "work.task",
              identity: {
                id: "user",
                title: "个人账户",
                icon: "person",
                order: 10,
              },
              title: "任务规划",
              description: "在树形大纲与分页清单中维护个人任务",
              icon: "account_tree",
              order: 50,
              actions: [],
              action_presentations: [],
              views: ["work.task.outline", "work.task.backlog"],
            },
          ],
        },
      }),
    }),
  );

  const queryBodies: Array<Record<string, unknown>> = [];
  await page.route("**/api/v1/work/tasks/query", async (route) => {
    const body = route.request().postDataJSON() as Record<string, unknown>;
    queryBodies.push(body);
    if (body.search === "旧请求") {
      await fulfillJson(
        route,
        {
          items: [{ id: 501, title: "旧结果", status: "todo" }],
          page: 1,
          page_size: 20,
          total: 1,
        },
        700,
      );
      return;
    }
    if (body.search === "新请求") {
      await fulfillJson(
        route,
        {
          items: [{ id: 502, title: "新结果", status: "todo" }],
          page: 1,
          page_size: 20,
          total: 1,
        },
        30,
      );
      return;
    }
    await fulfillJson(route, {
      items: treeItems,
      page: 1,
      page_size: 100,
      total: 100,
    });
  });

  const completedBodies: unknown[] = [];
  await page.route("**/api/v1/work/tasks/complete", async (route) => {
    completedBodies.push(route.request().postDataJSON());
    await fulfillJson(route, { requested: 100, affected: 100 });
  });

  await page.goto("/module/work.task");
  await expect(page.getByRole("heading", { name: "任务树" })).toBeVisible();
  await expect(page.locator("tbody tr")).toHaveCount(100);
  await expect(page.getByText("树任务-099", { exact: true })).toBeVisible();
  expect(
    queryBodies.some(
      (body) =>
        body.page_size === 100 &&
        Array.isArray(body.order_by) &&
        (body.order_by as Array<{ direction?: string }>)[0]?.direction ===
          "Asc",
    ),
  ).toBe(true);

  const search = page.getByPlaceholder("搜索 title");
  const oldRequest = page.waitForRequest(
    (request) =>
      request.url().includes("/api/v1/work/tasks/query") &&
      request.postDataJSON()?.search === "旧请求",
  );
  await search.fill("旧请求");
  await search.press("Enter");
  await oldRequest;

  const newRequest = page.waitForRequest(
    (request) =>
      request.url().includes("/api/v1/work/tasks/query") &&
      request.postDataJSON()?.search === "新请求",
  );
  await search.fill("新请求");
  await search.press("Enter");
  await newRequest;
  await expect(page.getByText("新结果", { exact: true })).toBeVisible();
  await page.waitForTimeout(800);
  await expect(page.getByText("旧结果", { exact: true })).toHaveCount(0);
  await expect(page.getByText("新结果", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "清除全部" }).click();
  await expect(page.locator("tbody tr")).toHaveCount(100);
  await page.locator("thead .q-checkbox").click();
  await expect(page.getByText("已选 100 项", { exact: true })).toBeVisible();
  const bulkButton = page.getByRole("button", { name: "批量完成" });
  await bulkButton.click();
  const confirmation = page.getByRole("dialog", { name: "批量完成任务" });
  const confirmButton = confirmation.getByRole("button", { name: "确认" });
  await expect(confirmButton).toBeFocused();
  await confirmButton.click();
  await expect.poll(() => completedBodies.length).toBe(1);
  expect(completedBodies[0]).toMatchObject({
    selected: expect.arrayContaining([
      expect.objectContaining({ id: 1 }),
      expect.objectContaining({ id: 100 }),
    ]),
  });
  expect((completedBodies[0] as { selected: unknown[] }).selected).toHaveLength(
    100,
  );
});
