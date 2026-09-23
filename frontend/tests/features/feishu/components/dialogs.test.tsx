import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ConfirmDialog } from "@/features/feishu/components/ConfirmDialog";

/// 删除确认框的语气与「看得见删的是哪一条」。
///
/// 创建/编辑那个字段级表单对话框跟着字段级可写入口一起退役了（服务端只剩表级
/// 端点），所以这一份只留 `ConfirmDialog`。

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

  it("停用：说清后果、说明可逆，且确认按钮是中性语气（历史上用过，组件保留这个语气）", () => {
    render(
      <ConfirmDialog
        open
        kind="disable"
        target="expense_category"
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
        target="expense_category"
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
