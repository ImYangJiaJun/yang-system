import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { DatasourceCardGrid } from "@/features/feishu/components/DatasourceCardGrid";
import { DatasourceLedger } from "@/features/feishu/components/DatasourceLedger";
import { ListPagination } from "@/features/feishu/components/ListPagination";
import { ListToolbar } from "@/features/feishu/components/ListToolbar";
import type { DatasourceItem } from "@/features/feishu/types";

/// 列表的两种渲染 + 工具栏 + 分页：权限门控、可排序列、空/加载分支。

function item(overrides: Partial<DatasourceItem> = {}): DatasourceItem {
  return {
    id: 7,
    fields: [],
    sourceKey: "dept_sales",
    title: "部门",
    encryptEnabled: false,
    defaultLocale: "zh_cn",
    status: "active",
    updatedAt: 1758000000,
    ingestMode: "push",
    bitableBaseToken: null,
    bitableTableId: null,
    bitableViewId: null,
    bitableFieldName: null,
    linkageMapping: null,
    lastPullAt: null,
    lastSuccessAt: null,
    consecutiveFailures: 0,
    lastError: null,
    snapshotDigest: null,
    ...overrides,
  };
}

const TWO_ITEMS = [
  item({ sourceKey: "dept_sales", title: "部门" }),
  item({
    sourceKey: "expense_category",
    title: "费用类型",
    status: "disabled",
    encryptEnabled: true,
    defaultLocale: "en_us",
  }),
];

function gridHandlers() {
  return {
    onOpen: vi.fn(),
    onRename: vi.fn(),
    onToggleStatus: vi.fn(),
    onDelete: vi.fn(),
  };
}

function ledgerHandlers() {
  return { ...gridHandlers(), onSort: vi.fn() };
}

describe("DatasourceCardGrid", () => {
  it("渲染标题、等宽标识与徽标行（语言显示为中文名）", () => {
    render(
      <DatasourceCardGrid
        items={TWO_ITEMS}
        canWrite={false}
        {...gridHandlers()}
      />,
    );
    expect(screen.getByText("费用类型")).toBeInTheDocument();
    expect(screen.getByText("expense_category")).toBeInTheDocument();
    expect(screen.getByText("已停用")).toBeInTheDocument();
    expect(screen.getByText("English")).toBeInTheDocument();
    // 只有开了「加密返回」的那条才有这个标记
    expect(screen.getAllByText("加密返回")).toHaveLength(1);
  });

  it("无写权限时「⋯」操作菜单不渲染（不是禁用）", () => {
    render(
      <DatasourceCardGrid
        items={TWO_ITEMS}
        canWrite={false}
        {...gridHandlers()}
      />,
    );
    expect(screen.queryByRole("button", { name: /的操作/ })).toBeNull();
  });

  it("有写权限时每张卡片都有「⋯」菜单", () => {
    render(
      <DatasourceCardGrid items={TWO_ITEMS} canWrite {...gridHandlers()} />,
    );
    expect(screen.getAllByRole("button", { name: /的操作/ })).toHaveLength(2);
  });

  it("点卡片进详情（点标题或点卡片本体都算）", async () => {
    const user = userEvent.setup();
    const handlers = gridHandlers();
    render(
      <DatasourceCardGrid items={TWO_ITEMS} canWrite={false} {...handlers} />,
    );

    await user.click(screen.getByRole("button", { name: "费用类型" }));
    expect(handlers.onOpen).toHaveBeenCalledWith(TWO_ITEMS[1]);

    await user.click(screen.getByText("dept_sales"));
    expect(handlers.onOpen).toHaveBeenCalledWith(TWO_ITEMS[0]);
  });

  it("加载中出卡片形状的骨架屏", () => {
    const { container } = render(
      <DatasourceCardGrid items={[]} canWrite pending {...gridHandlers()} />,
    );
    expect(
      container.querySelectorAll('[data-slot="skeleton"]').length,
    ).toBeGreaterThan(0);
  });

  it("没有数据源时不渲染空栅格", () => {
    const { container } = render(
      <DatasourceCardGrid items={[]} canWrite {...gridHandlers()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });
});

describe("DatasourceLedger", () => {
  it("每个数据源一行，状态与语言按展示名渲染", () => {
    render(
      <DatasourceLedger
        items={TWO_ITEMS}
        orderBy={[{ field: "source_key", direction: "Asc" }]}
        canWrite={false}
        {...ledgerHandlers()}
      />,
    );
    expect(screen.getAllByRole("row")).toHaveLength(3); // 表头 + 两行
    expect(screen.getByText("已停用")).toBeInTheDocument();
    expect(screen.getByText("简体中文")).toBeInTheDocument();
  });

  it("只有名称与标识两列可排序（其余列后端没声明 sortable）", () => {
    render(
      <DatasourceLedger
        items={TWO_ITEMS}
        orderBy={[{ field: "source_key", direction: "Asc" }]}
        canWrite={false}
        {...ledgerHandlers()}
      />,
    );
    const sortable = screen.getAllByRole("button", { name: /^按.+排序$/ });
    expect(sortable.map((button) => button.getAttribute("aria-label"))).toEqual(
      ["按名称排序", "按标识排序"],
    );
  });

  it("点列头把字段名交给上层", async () => {
    const user = userEvent.setup();
    const handlers = ledgerHandlers();
    render(
      <DatasourceLedger
        items={TWO_ITEMS}
        orderBy={[{ field: "source_key", direction: "Asc" }]}
        canWrite={false}
        {...handlers}
      />,
    );
    await user.click(screen.getByRole("button", { name: "按名称排序" }));
    expect(handlers.onSort).toHaveBeenCalledWith("title");
  });

  it("无写权限时行末的「⋯」菜单不渲染", () => {
    render(
      <DatasourceLedger
        items={TWO_ITEMS}
        orderBy={[{ field: "source_key", direction: "Asc" }]}
        canWrite={false}
        {...ledgerHandlers()}
      />,
    );
    expect(screen.queryByRole("button", { name: /的操作/ })).toBeNull();
  });

  it("有写权限时有「⋯」菜单", () => {
    render(
      <DatasourceLedger
        items={TWO_ITEMS}
        orderBy={[{ field: "source_key", direction: "Asc" }]}
        canWrite
        {...ledgerHandlers()}
      />,
    );
    expect(screen.getAllByRole("button", { name: /的操作/ })).toHaveLength(2);
  });

  it("点行进详情", async () => {
    const user = userEvent.setup();
    const handlers = ledgerHandlers();
    render(
      <DatasourceLedger
        items={TWO_ITEMS}
        orderBy={[{ field: "source_key", direction: "Asc" }]}
        canWrite={false}
        {...handlers}
      />,
    );
    await user.click(screen.getByRole("button", { name: "部门" }));
    expect(handlers.onOpen).toHaveBeenCalledWith(TWO_ITEMS[0]);
  });

  it("加载中出若干行骨架，不是转圈", () => {
    const { container } = render(
      <DatasourceLedger
        items={[]}
        orderBy={[{ field: "source_key", direction: "Asc" }]}
        canWrite
        pending
        {...ledgerHandlers()}
      />,
    );
    expect(
      container.querySelectorAll('[data-slot="skeleton"]').length,
    ).toBeGreaterThan(0);
  });

  it("没有数据源时不渲染空表", () => {
    const { container } = render(
      <DatasourceLedger
        items={[]}
        orderBy={[{ field: "source_key", direction: "Asc" }]}
        canWrite
        {...ledgerHandlers()}
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });
});

describe("ListToolbar", () => {
  it("视图切换、搜索、状态筛选各自回调", async () => {
    const user = userEvent.setup();
    const onViewChange = vi.fn();
    const onSearchChange = vi.fn();
    const onStatusChange = vi.fn();
    render(
      <ListToolbar
        view="ledger"
        onViewChange={onViewChange}
        search=""
        onSearchChange={onSearchChange}
        status="all"
        onStatusChange={onStatusChange}
      />,
    );

    await user.click(screen.getByRole("button", { name: "卡片" }));
    expect(onViewChange).toHaveBeenCalledWith("cards");

    await user.type(screen.getByLabelText("搜索数据源"), "北京");
    expect(onSearchChange).toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "已停用" }));
    expect(onStatusChange).toHaveBeenCalledWith("disabled");

    // 当前选项要能被读出状态（分段控件用 aria-pressed）
    expect(screen.getByRole("button", { name: "台账" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  it("无写权限时不渲染「添加数据源」", () => {
    render(
      <ListToolbar
        view="ledger"
        onViewChange={vi.fn()}
        search=""
        onSearchChange={vi.fn()}
        status="all"
        onStatusChange={vi.fn()}
        canWrite={false}
        onAdd={vi.fn()}
      />,
    );
    expect(screen.queryByRole("button", { name: "添加数据源" })).toBeNull();
  });

  it("有写权限时渲染「添加数据源」并可点", async () => {
    const user = userEvent.setup();
    const onAdd = vi.fn();
    render(
      <ListToolbar
        view="ledger"
        onViewChange={vi.fn()}
        search=""
        onSearchChange={vi.fn()}
        status="all"
        onStatusChange={vi.fn()}
        canWrite
        onAdd={onAdd}
      />,
    );
    await user.click(screen.getByRole("button", { name: "添加数据源" }));
    expect(onAdd).toHaveBeenCalled();
  });
});

describe("ListPagination", () => {
  it("有总数时显示「共 N 个 · 第 x / y 页」", () => {
    render(
      <ListPagination
        page={1}
        pageSize={10}
        total={25}
        onPageChange={vi.fn()}
        onPageSizeChange={vi.fn()}
      />,
    );
    expect(screen.getByText("共 25 个 · 第 1 / 3 页")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "上一页" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "下一页" })).toBeEnabled();
  });

  it("翻到最后一页时下一页禁用", () => {
    render(
      <ListPagination
        page={3}
        pageSize={10}
        total={25}
        onPageChange={vi.fn()}
        onPageSizeChange={vi.fn()}
      />,
    );
    expect(screen.getByRole("button", { name: "下一页" })).toBeDisabled();
  });

  it("点下一页把目标页码交回上层", async () => {
    const user = userEvent.setup();
    const onPageChange = vi.fn();
    render(
      <ListPagination
        page={1}
        pageSize={10}
        total={25}
        onPageChange={onPageChange}
        onPageSizeChange={vi.fn()}
      />,
    );
    await user.click(screen.getByRole("button", { name: "下一页" }));
    expect(onPageChange).toHaveBeenCalledWith(2);
  });

  it("没请求总数时不编造「共 N 个」", () => {
    render(
      <ListPagination
        page={2}
        pageSize={10}
        total={null}
        onPageChange={vi.fn()}
        onPageSizeChange={vi.fn()}
      />,
    );
    expect(screen.getByText("第 2 页")).toBeInTheDocument();
    expect(screen.queryByText(/共 /)).toBeNull();
  });
});
