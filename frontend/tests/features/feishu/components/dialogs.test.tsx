import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { ConfirmDialog } from "@/features/feishu/components/ConfirmDialog";
import {
  DatasourceFormDialog,
  type DatasourceFormSubmission,
} from "@/features/feishu/components/DatasourceFormDialog";

/// 两个对话框：确认语气（删除 destructive / 停用中性）与手写表单的提交物形状。

const DELETE_MESSAGE = "删除后其下全部选项会被同时停用，且不可恢复。确认删除？";

describe("ConfirmDialog", () => {
  it("删除：标题与正文逐字用后端原文，确认按钮是破坏性语气", () => {
    render(
      <ConfirmDialog
        open
        kind="delete"
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByText("删除数据源")).toBeInTheDocument();
    expect(screen.getByText(DELETE_MESSAGE)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "删除" })).toHaveClass(
      "bg-destructive",
    );
    expect(screen.getByRole("button", { name: "取消" })).toBeInTheDocument();
  });

  it("停用：说清后果、说明可逆，且确认按钮是中性语气", () => {
    render(
      <ConfirmDialog
        open
        kind="disable"
        sourceKey="expense_category"
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByText("停用数据源")).toBeInTheDocument();
    const message = screen.getByText(/飞书审批控件会立即取不到选项/);
    expect(message.textContent).toContain("随时可以再启用");
    expect(message.textContent).not.toContain("不可恢复");
    expect(screen.getByRole("button", { name: "停用" })).not.toHaveClass(
      "bg-destructive",
    );
  });

  it("删除：正文之外必须显示 source_key——用户得看见自己删的是哪一条", () => {
    render(
      <ConfirmDialog
        open
        kind="delete"
        sourceKey="expense_category"
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    // 后端原文一字不动，标识另起一行给
    expect(screen.getByText(DELETE_MESSAGE)).toBeInTheDocument();
    expect(screen.getByText("expense_category")).toBeInTheDocument();
  });

  it("两种语气说得出区别：删除不可逆、停用可逆", () => {
    const { unmount } = render(
      <ConfirmDialog
        open
        kind="delete"
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    const deleteText = screen.getByText(DELETE_MESSAGE).textContent ?? "";
    unmount();

    render(
      <ConfirmDialog
        open
        kind="disable"
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    const disableText =
      screen.getByText(/飞书审批控件会立即取不到选项/).textContent ?? "";

    expect(disableText).not.toBe(deleteText);
    expect(disableText).toContain("不会被删除");
    expect(deleteText).toContain("不可恢复");
  });

  it("提交中时两个按钮都禁用，文案是「提交中…」", () => {
    render(
      <ConfirmDialog
        open
        kind="delete"
        pending
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByRole("button", { name: "提交中…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "取消" })).toBeDisabled();
  });

  it("关闭时不渲染内容", () => {
    render(
      <ConfirmDialog
        open={false}
        kind="delete"
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.queryByText("删除数据源")).toBeNull();
  });
});

describe("DatasourceFormDialog · 新建", () => {
  it("空表单提交时不回调，并逐字段给出界面话的错误", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn<(submission: DatasourceFormSubmission) => void>();
    render(
      <DatasourceFormDialog
        open
        mode="create"
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: "创建数据源" }));

    expect(onSubmit).not.toHaveBeenCalled();
    expect(screen.getByText(/标识必须是 1\.\.=64 字节/)).toBeInTheDocument();
    expect(screen.getByText("名称必须在 1..=100 字符")).toBeInTheDocument();
    expect(screen.getByText("接口 Token 不能为空")).toBeInTheDocument();
  });

  it("数据源标识拒绝大写与连字符", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn<(submission: DatasourceFormSubmission) => void>();
    render(
      <DatasourceFormDialog
        open
        mode="create"
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );

    await user.type(screen.getByLabelText("数据源标识"), "Dept-Sales");
    await user.type(screen.getByLabelText("名称"), "部门");
    await user.type(screen.getByLabelText("接口 Token"), "t-1");
    await user.click(screen.getByRole("button", { name: "创建数据源" }));

    expect(onSubmit).not.toHaveBeenCalled();
    expect(screen.getByText(/标识必须是 1\.\.=64 字节/)).toBeInTheDocument();
  });

  it("合法输入提交出 create 形状，默认语言默认 zh_cn", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn<(submission: DatasourceFormSubmission) => void>();
    render(
      <DatasourceFormDialog
        open
        mode="create"
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );

    await user.type(screen.getByLabelText("数据源标识"), "dept_sales");
    await user.type(screen.getByLabelText("名称"), "  部门  ");
    await user.type(screen.getByLabelText("接口 Token"), "t-1");
    await user.click(screen.getByRole("checkbox", { name: /加密返回/ }));
    await user.click(screen.getByRole("button", { name: "创建数据源" }));

    expect(onSubmit).toHaveBeenCalledWith({
      mode: "create",
      sourceKey: "dept_sales",
      title: "部门",
      token: "t-1",
      encryptEnabled: true,
      defaultLocale: "zh_cn",
    });
  });

  it("常驻写明「加密返回」的真实代价（50002）", () => {
    render(
      <DatasourceFormDialog
        open
        mode="create"
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByText(/50002/)).toBeInTheDocument();
    expect(screen.queryByText(/加密存储/)).toBeNull();
  });

  it("服务端错误直接回显后端原文", () => {
    render(
      <DatasourceFormDialog
        open
        mode="create"
        serverError="数据源不存在"
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent("数据源不存在");
  });
});

describe("DatasourceFormDialog · 重命名", () => {
  it("标识不可改，只以文本展示", () => {
    render(
      <DatasourceFormDialog
        open
        mode="rename"
        initialSourceKey="dept_sales"
        initialTitle="部门"
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.queryByLabelText("数据源标识")).toBeNull();
    expect(screen.getByText("dept_sales")).toBeInTheDocument();
    expect(screen.getByLabelText("名称")).toHaveValue("部门");
  });

  it("Token 留空时提交物里**没有** token 这个键（省略 = 不轮换）", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn<(submission: DatasourceFormSubmission) => void>();
    render(
      <DatasourceFormDialog
        open
        mode="rename"
        initialSourceKey="dept_sales"
        initialTitle="部门"
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );

    await user.clear(screen.getByLabelText("轮换 Token（可留空）"));
    await user.click(screen.getByRole("button", { name: "保存" }));

    expect(onSubmit).toHaveBeenCalledTimes(1);
    const submission = onSubmit.mock.calls[0]?.[0];
    expect(submission).toEqual({
      mode: "rename",
      sourceKey: "dept_sales",
      title: "部门",
    });
    expect(submission && "token" in submission).toBe(false);
  });

  it("填了 Token 时带上 token（轮换）", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn<(submission: DatasourceFormSubmission) => void>();
    render(
      <DatasourceFormDialog
        open
        mode="rename"
        initialSourceKey="dept_sales"
        initialTitle="部门"
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );

    await user.type(screen.getByLabelText("轮换 Token（可留空）"), "t-2");
    await user.click(screen.getByRole("button", { name: "保存" }));

    expect(onSubmit).toHaveBeenCalledWith({
      mode: "rename",
      sourceKey: "dept_sales",
      title: "部门",
      token: "t-2",
    });
  });

  it("提交中时按钮禁用并显示「提交中…」", () => {
    render(
      <DatasourceFormDialog
        open
        mode="rename"
        pending
        initialSourceKey="dept_sales"
        initialTitle="部门"
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByRole("button", { name: "提交中…" })).toBeDisabled();
  });
});

describe("DatasourceFormDialog · 编辑态也能改「加密返回 / 默认语言」", () => {
  /// 默认语言选错会让该数据源在飞书侧所有语言下都取不到文案，控制台必须留一条修复路径。
  const editProps = {
    open: true,
    mode: "rename" as const,
    initialSourceKey: "dept_sales",
    initialTitle: "部门",
    initialEncryptEnabled: true,
    initialDefaultLocale: "zh_cn",
  };

  it("给了现值就渲染这两项：勾选框反映现值，默认语言是当前值", () => {
    render(
      <DatasourceFormDialog
        {...editProps}
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByRole("checkbox", { name: /加密返回/ })).toBeChecked();
    expect(
      screen.getByRole("combobox", { name: "默认语言" }),
    ).toHaveTextContent("简体中文");
  });

  it("改默认语言并把「加密返回」关掉，提交物带上这两项", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn<(submission: DatasourceFormSubmission) => void>();
    render(
      <DatasourceFormDialog
        {...editProps}
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("combobox", { name: "默认语言" }));
    await user.click(await screen.findByRole("option", { name: "English" }));
    await user.click(screen.getByRole("checkbox", { name: /加密返回/ }));
    await user.click(screen.getByRole("button", { name: "保存" }));

    expect(onSubmit).toHaveBeenCalledWith({
      mode: "rename",
      sourceKey: "dept_sales",
      title: "部门",
      encryptEnabled: false,
      defaultLocale: "en_us",
    });
  });

  it("现值在取值域外（zh-CN）：留空并说明，改动只能由用户显式选一项产生", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn<(submission: DatasourceFormSubmission) => void>();
    render(
      <DatasourceFormDialog
        {...editProps}
        initialDefaultLocale="zh-CN"
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );

    // 不把它偷偷当成 zh_cn，也不拿它当选中项
    const locale = screen.getByRole("combobox", { name: "默认语言" });
    expect(locale).toHaveTextContent("未选择（不修改）");
    expect(screen.getByText(/「zh-CN」/)).toBeInTheDocument();
    expect(screen.getByText(/不在取值域内/)).toBeInTheDocument();
    // 「加密返回」与它互不牵连：那一项知道现值，照样能改
    expect(screen.getByRole("checkbox", { name: /加密返回/ })).toBeChecked();

    await user.click(screen.getByRole("button", { name: "保存" }));
    const submission = onSubmit.mock.calls[0]?.[0];
    expect(submission).toEqual({
      mode: "rename",
      sourceKey: "dept_sales",
      title: "部门",
      encryptEnabled: true,
    });
  });

  it("拿不到现值就不渲染这两项，提交物里也不带（省略 = 保持原值）", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn<(submission: DatasourceFormSubmission) => void>();
    render(
      <DatasourceFormDialog
        open
        mode="rename"
        initialSourceKey="dept_sales"
        initialTitle="部门"
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.queryByRole("checkbox", { name: /加密返回/ })).toBeNull();
    expect(screen.queryByRole("combobox", { name: "默认语言" })).toBeNull();
    // 说清为什么这次改不了，以及去哪改
    expect(screen.getByText(/读不到这个数据源当前的/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "保存" }));
    const submission = onSubmit.mock.calls[0]?.[0];
    expect(submission).toEqual({
      mode: "rename",
      sourceKey: "dept_sales",
      title: "部门",
    });
  });
});
