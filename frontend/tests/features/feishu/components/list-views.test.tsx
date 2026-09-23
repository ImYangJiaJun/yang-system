import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { DatasourceCardGrid } from "@/features/feishu/components/DatasourceCardGrid";
import { DatasourceLedger } from "@/features/feishu/components/DatasourceLedger";
import { ListPagination } from "@/features/feishu/components/ListPagination";
import { ListToolbar } from "@/features/feishu/components/ListToolbar";
import type {
  DatasourceFieldBinding,
  DatasourceItem,
} from "@/features/feishu/types";

/// 列表的两种渲染 + 工具栏 + 分页：权限门控、可排序列、空/加载分支。

function item(overrides: Partial<DatasourceItem> = {}): DatasourceItem {
  return {
    id: 7,
    fields: [],
    title: "部门",
    status: "active",
    updatedAt: 1758000000,
    ingestMode: "push",
    bitableBaseToken: null,
    bitableTableId: null,
    bitableViewId: null,
    lastPullAt: null,
    lastSuccessAt: null,
    consecutiveFailures: 0,
    lastError: null,
    ...overrides,
  };
}

/// 一条字段绑定。表级化之后**行上的标识来自首个绑定**（`identityLabel`），
/// 所以卡片/台账上那个等宽标识要靠它才立得住——不给绑定，行上就只剩 `#id`。
function binding(
  sourceKey: string,
  overrides: Partial<DatasourceFieldBinding> = {},
): DatasourceFieldBinding {
  return {
    fieldId: `fld_${sourceKey}`,
    fieldName: null,
    sourceKey,
    parentFieldId: null,
    enabled: true,
    encryptEnabled: false,
    defaultLocale: "zh_cn",
    tokenRotatedAt: null,
    ...overrides,
  };
}

const TWO_ITEMS = [
  // 区分两条行靠主键、标题与各自的首个绑定——`source_key` 不是**表级行**的身份
  // （一条行有 N 个，全在 `fields` 里）。
  item({ id: 7, title: "部门", fields: [binding("dept_sales")] }),
  item({
    id: 8,
    title: "费用类型",
    status: "disabled",
    fields: [binding("expense_category", { enabled: false })],
  }),
];

function gridHandlers() {
  return {
    onOpen: vi.fn(),
    onEdit: vi.fn(),
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
    // 「加密返回」与「默认语言」**不在这里**：它们是绑定级属性，卡片拿到的表级行上
    // 没有这两个键。逐字段的取值在详情页的 `FieldBindingsTable`。
    expect(screen.queryByText("加密返回")).toBeNull();
    expect(screen.queryByText("English")).toBeNull();
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
    // 「默认语言」列已删（绑定级属性，表级行上没有单一值可显示）。
    expect(screen.queryByText("简体中文")).toBeNull();
  });

  it("只有名称一列可排序：「标识」在表级行上没有单一值，排序会打到不存在的列", () => {
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
      ["按名称排序"],
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
  it("搜索框只承诺名称——后端只对表级行的 title 做检索，标识在字段绑定上", () => {
    // 回归：文案曾经是「搜索名称或标识」。而关键词最终进 `list_datasources` 打在
    // **表级行**上的 `.search()`，那一行唯一 `searchable` 的列是 `title`（`source_key`
    // 属于字段绑定，表级行上没有这一列）。于是粘一个真实存在的标识恒得 0 行，
    // 页面随即渲染「没有匹配的数据源」——对一条确实存在的数据源说了一句假话。
    // 支持按标识搜要在服务端跨表查；在那之前，文案不能先答应。
    render(
      <ListToolbar
        view="ledger"
        search=""
        status="all"
        onViewChange={vi.fn()}
        onSearchChange={vi.fn()}
        onStatusChange={vi.fn()}
      />,
    );
    expect(screen.getByLabelText("搜索数据源")).toHaveAttribute(
      "placeholder",
      "搜索名称",
    );
  });

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
