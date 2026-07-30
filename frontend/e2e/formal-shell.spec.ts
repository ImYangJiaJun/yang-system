import { expect, test, type Page } from "@playwright/test";

async function serveBusinessCatalog(page: Page) {
  await page.route("**/.well-known/yang/ui-catalog", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          schema_version: "2.3",
          revision: "b".repeat(64),
          actions: [
            {
              operation_id: "demo.items.list",
              title: "查询项目",
              description: "分页查询项目",
              method: "POST",
              path: "/api/v1/demo/items/query",
              params: [],
              input_schema: { type: "object" },
              output_schema: { type: "object" },
              request_media_type: "json",
              response_kind: "json",
              requires_auth: false,
            },
            {
              operation_id: "demo.items.add",
              title: "新增项目",
              description: "创建项目",
              method: "POST",
              path: "/api/v1/demo/items",
              params: [],
              input_schema: { type: "object" },
              output_schema: { type: "object" },
              request_media_type: "json",
              response_kind: "json",
              requires_auth: false,
            },
          ],
          table_views: [
            {
              view_id: "demo.items.main",
              title: "项目目录",
              table: "demo_items",
              data_action: "demo.items.list",
              columns: [
                {
                  field: "name",
                  title: "名称",
                  description: "项目名称",
                  widget: "text",
                  required: true,
                  searchable: true,
                  filterable: true,
                  sortable: true,
                },
              ],
              form: { fields: [] },
              query: {
                search_fields: ["name"],
                filter_fields: ["name"],
                default_sort: [],
                default_page_size: 20,
                max_page_size: 100,
              },
              actions: ["demo.items.add"],
              action_presentations: [
                {
                  operation_id: "demo.items.add",
                  title: "新增项目",
                  placement: "toolbar",
                  interaction: "form",
                },
              ],
            },
          ],
          modules: [],
        },
      }),
    }),
  );
  await page.route("**/api/v1/demo/items/query", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: { items: [{ name: "示例项目" }], total: 1 },
      }),
    }),
  );
}

test("正式控制台模块只有一个导航入口", async ({ page }) => {
  await page.addInitScript(() => {
    sessionStorage.setItem("yang.token", "formal-shell-token");
    sessionStorage.setItem("yang.account-identity", "user");
  });
  await page.route("**/.well-known/yang/ui-catalog", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        code: 0,
        message: "成功",
        data: {
          schema_version: "2.3",
          revision: "f".repeat(64),
          actions: [
            {
              operation_id: "account.user.me",
              title: "当前用户",
              description: "查看当前登录账号",
              method: "GET",
              path: "/api/v1/account/user/me",
              params: [],
              input_schema: {},
              output_schema: {},
              request_media_type: "json",
              response_kind: "json",
              requires_auth: true,
            },
          ],
          table_views: [],
          modules: [
            {
              module_id: "account.user",
              identity: {
                id: "user",
                title: "个人账户",
                icon: "person",
                order: 10,
              },
              title: "用户中心",
              description: "",
              icon: "account",
              order: 10,
              primary_action: "account.user.me",
              actions: [],
              action_presentations: [],
              views: [],
            },
          ],
        },
      }),
    }),
  );
  await page.goto("/module/account.user");

  await expect(page.getByRole("tab")).toHaveCount(0);
  await expect(page.getByTestId("module-nav-account.user")).toBeVisible();
  await expect(page.getByTestId("module-navigation")).toHaveCount(1);
  await expect(page.getByText("account.user", { exact: true })).toHaveCount(0);
  await expect(
    page.locator(".formal-context").getByText("个人账户", { exact: true }),
  ).toBeVisible();
  await expect(page.getByText("切换角色", { exact: true })).toHaveCount(0);
  await expect(page.locator(".navigation-mode")).toHaveCount(0);
  await expect(page.getByRole("tab", { name: "开发工作台" })).toHaveCount(0);

  await page.goto("/workbench");
  await expect(page).toHaveURL("/workbench");
  await expect(
    page.getByText("YANG 接口工作台", { exact: true }),
  ).toBeVisible();
  await expect(page.locator(".navigation-mode")).toBeVisible();
});

test("正式业务入口使用目录投影的通用页面", async ({ page }) => {
  await page.addInitScript(() => {
    sessionStorage.setItem("yang.token", "formal-shell-token");
    sessionStorage.setItem("yang.account-identity", "user");
  });
  await serveBusinessCatalog(page);
  await page.goto("/business");

  await expect(page).toHaveURL("/business");
  await expect(page.getByRole("heading", { name: "项目目录" })).toBeVisible();
  await expect(
    page.locator(".formal-nav-list").getByText("项目目录", { exact: true }),
  ).toBeVisible();
  await expect(page.locator(".navigation-mode")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "新增项目" })).toBeVisible();
  await expect(
    page.locator(".table-view-heading").getByText("demo.items.main", {
      exact: true,
    }),
  ).toHaveCount(0);
  await expect(
    page.locator(".table-view-heading").getByText(/数据源/),
  ).toHaveCount(0);
});
