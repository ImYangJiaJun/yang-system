import { test, expect, type Page } from "@playwright/test";
import { mockSessionRestore } from "./support";

async function mockApprovalConfigs(page: Page) {
  await mockSessionRestore(page);
  const actions = [
    ["list_configs", "/configs/query"],
    ["list_requests", "/requests/query"],
    ["list_tasks", "/tasks/query"],
    ["create_config", "/configs/create"],
    ["update_config", "/configs/update"],
    ["delete_config", "/configs/delete"],
    ["list_widgets", "/definitions/widgets"],
    ["refresh_names", "/configs/refresh-names"],
  ].map(([name, path]) => ({
    operation_id: `feishu.approval.${name}`,
    title: name,
    description: "",
    method: "POST",
    path: `/api/v1/feishu/approval${path}`,
    params: [],
    input_schema: {},
    output_schema: {},
    request_media_type: "json",
    response_kind: "json",
    requires_auth: true,
  }));
  const rows = Array.from({ length: 24 }, (_, index) => ({
    id: index + 1,
    title: `差旅派发配置 ${index + 1}`,
    base_token: "appFinance",
    table_id: `tbl${index + 1}`,
    approval_code: "CODE-TRAVEL",
    base_name: "财务协作",
    table_name: `差旅申请表 ${index + 1}`,
    approval_name: "员工报销审批",
    applicant_field_name: "报销人",
    backfill_field_name: "审批单编号",
    applicant_field: "fldPerson",
    backfill_field: "fldNumber",
    base_timezone: "Asia/Shanghai",
    enabled: index % 2 === 0,
    updated_at: 1791780000,
    form_snapshot_at: null,
    maps: [
      {
        widget_id: "wDate",
        widget_name: "出发日期",
        widget_type: "date",
        bitable_field: "fldDate",
        bitable_field_name: "出差日期",
        required: true,
        converter: "date",
      },
    ],
  }));
  await page.route("**/.well-known/yang/ui-catalog", (route) =>
    route.fulfill({
      json: {
        code: 0,
        message: "成功",
        data: {
          schema_version: "2.3",
          revision: "a".repeat(64),
          actions,
          table_views: [],
          modules: [],
        },
      },
    }),
  );
  await page.route("**/api/v1/users/me", (route) =>
    route.fulfill({
      json: {
        code: 0,
        message: "成功",
        data: {
          id: 7,
          username: "alice",
          email: "alice@example.com",
          email_verified_at: 1000,
          status: "active",
          created_at: 500,
          updated_at: 600,
        },
      },
    }),
  );
  await page.route("**/api/v1/feishu/approval/configs/query", (route) => {
    const body = route.request().postDataJSON();
    type Filter = {
      type: string;
      field?: string;
      value?: unknown;
      conditions?: Filter[];
    };
    function matches(
      row: (typeof rows)[number],
      where: Filter | undefined,
    ): boolean {
      if (!where) return true;
      if (where.type === "and")
        return (where.conditions ?? []).every((condition) =>
          matches(row, condition),
        );
      return row[where.field as keyof typeof row] === where.value;
    }
    const filtered = rows.filter(
      (row) =>
        matches(row, body.where) &&
        (!body.search ||
          `${row.title} ${row.base_name} ${row.table_name} ${row.approval_name}`.includes(
            body.search,
          )),
    );
    return route.fulfill({
      json: {
        code: 0,
        message: "成功",
        data: {
          items: filtered.slice(
            (body.page - 1) * body.page_size,
            body.page * body.page_size,
          ),
          total: filtered.length,
          page: body.page,
          page_size: body.page_size,
        },
      },
    });
  });
}

test("业务列表、独立详情、关联分页与浏览器返回", async ({ page }) => {
  await mockApprovalConfigs(page);
  await page.goto("/feishu/approval?tab=configs&page=2&search=差旅");
  await expect(
    page.getByRole("heading", { name: "审批配置", exact: true }),
  ).toBeVisible();
  await expect(page.getByText("共 24 个 · 第 2 / 3 页")).toBeVisible();
  const main = page.getByRole("main");
  await main.evaluate((element) => {
    element.scrollTop = 180;
  });
  const scroll = await main.evaluate((element) => element.scrollTop);
  await page
    .getByRole("button", { name: "差旅派发配置 11", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "差旅派发配置 11", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("按表格时区转换日期", { exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "查看关联配置" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByText("共 24 个 · 第 1 / 3 页")).toBeVisible();
  await dialog.getByRole("button", { name: "下一页" }).click();
  await expect(dialog.getByText("共 24 个 · 第 2 / 3 页")).toBeVisible();
  await page.keyboard.press("Escape");
  await page.goBack();
  await expect(
    page.getByRole("heading", { name: "审批配置", exact: true }),
  ).toBeVisible();
  await expect(page.getByRole("textbox", { name: "搜索配置" })).toHaveValue(
    "差旅",
  );
  await expect
    .poll(() => main.evaluate((element) => element.scrollTop))
    .toBeGreaterThanOrEqual(scroll - 3);
  await page.screenshot({
    path: "output/playwright/approval-business-list.png",
    fullPage: true,
  });
});

test("窄屏无整页横向溢出，字段和排查标签可用键盘切换", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await mockApprovalConfigs(page);
  await page.goto("/feishu/approval?tab=configs");
  await expect(
    page.getByRole("heading", { name: "审批配置", exact: true }),
  ).toBeVisible();
  const mainBounds = await page.locator("main").boundingBox();
  expect(mainBounds?.width).toBeGreaterThanOrEqual(380);
  await page.getByRole("button", { name: "打开导航菜单" }).click();
  const navigation = page.getByRole("dialog", { name: "导航菜单" });
  await expect(navigation).toBeVisible();
  await expect(
    navigation.getByRole("button", { name: "账号菜单" }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(navigation).not.toBeVisible();
  await expect(
    page.getByRole("button", { name: "打开导航菜单" }),
  ).toBeFocused();
  await expect
    .poll(() =>
      page.evaluate(
        () => document.documentElement.scrollWidth - window.innerWidth,
      ),
    )
    .toBeLessThanOrEqual(1);
  await page
    .getByRole("button", { name: "差旅派发配置 1", exact: true })
    .click();
  await expect(
    page.getByRole("tabpanel", { name: "字段对应（1）", exact: true }),
  ).toContainText("出差日期");
  const maps = page.getByRole("tab", { name: "字段对应（1）" });
  await maps.focus();
  await page.keyboard.press("End");
  await expect(page.getByRole("tab", { name: "排查信息" })).toBeFocused();
  await expect(
    page.getByRole("tabpanel", { name: "排查信息", exact: true }),
  ).toContainText("CODE-TRAVEL");
  await page.keyboard.press("Home");
  await expect(maps).toBeFocused();
  await expect
    .poll(() =>
      page.evaluate(
        () => document.documentElement.scrollWidth - window.innerWidth,
      ),
    )
    .toBeLessThanOrEqual(1);
  await page.screenshot({
    path: "output/playwright/approval-business-mobile.png",
    fullPage: true,
  });
  await page.getByRole("button", { name: "返回配置列表" }).click();
  await expect(
    page.getByRole("heading", { name: "审批配置", exact: true }),
  ).toBeVisible();
});
