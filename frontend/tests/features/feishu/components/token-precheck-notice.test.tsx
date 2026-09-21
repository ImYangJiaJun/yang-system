import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { TokenPrecheckNotice } from "@/features/feishu/components/TokenPrecheckNotice";

/// 创建后的连通性预检回执：通了要给可复制的一段，失败了要说清「数据源已创建」。

describe("TokenPrecheckNotice", () => {
  it("预检中给出中性说明（不是错误）", () => {
    render(
      <TokenPrecheckNotice sourceKey="dept_sales" result={null} pending />,
    );
    expect(
      screen.getByText(/正在用刚填的 Token 试拉一次选项/),
    ).toBeInTheDocument();
  });

  it("没有结果且不在预检时不渲染任何东西", () => {
    const { container } = render(
      <TokenPrecheckNotice sourceKey="dept_sales" result={null} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("通了：给出带真实 source_key 的「粘回飞书审批后台」与复制按钮", async () => {
    const user = userEvent.setup();
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", {
      ...navigator,
      clipboard: { writeText },
    });

    render(
      <TokenPrecheckNotice
        sourceKey="dept_sales"
        result={{ status: "ok", optionCount: 3, encrypted: false }}
      />,
    );

    expect(screen.getByText(/服务端这次拉到了 3 个选项/)).toBeInTheDocument();
    expect(screen.getByText(/粘回飞书审批后台/)).toBeInTheDocument();
    expect(screen.getByText("dept_sales")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /复制/ }));
    expect(writeText).toHaveBeenCalledWith("dept_sales");
    expect(screen.getByText("已复制")).toBeInTheDocument();

    vi.unstubAllGlobals();
  });

  it("加密返回时说明「内容读不了但取数成功」", () => {
    render(
      <TokenPrecheckNotice
        sourceKey="dept_sales"
        result={{ status: "ok", optionCount: null, encrypted: true }}
      />,
    );
    expect(screen.getByText(/加密内容/)).toBeInTheDocument();
  });

  it("失败：说清「数据源已创建、但 Token 没验证过」，并给出码与含义", () => {
    render(
      <TokenPrecheckNotice
        sourceKey="dept_sales"
        result={{
          status: "failed",
          code: 40102,
          message: "token 校验失败",
          hint: "Token 与这个数据源里存的摘要不一致。",
        }}
      />,
    );

    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("数据源已创建");
    expect(alert).toHaveTextContent("token 校验失败");
    expect(alert).toHaveTextContent("40102");
    expect(screen.getByText(/摘要不一致/)).toBeInTheDocument();
    expect(screen.getByText(/不影响上面这次操作的结果/)).toBeInTheDocument();
  });

  it("失败时给「重新填写 Token」入口（不必删掉重建）", async () => {
    const user = userEvent.setup();
    const onRotate = vi.fn();
    render(
      <TokenPrecheckNotice
        sourceKey="dept_sales"
        result={{
          status: "failed",
          code: 40102,
          message: "token 校验失败",
          hint: "重填一次 Token 即可。",
        }}
        onRotate={onRotate}
      />,
    );

    await user.click(screen.getByRole("button", { name: "重新填写 Token" }));
    expect(onRotate).toHaveBeenCalled();
  });

  it("轮换场景的措辞是「Token 已更新」", () => {
    render(
      <TokenPrecheckNotice
        sourceKey="dept_sales"
        mode="rotate"
        result={{ status: "ok", optionCount: 0, encrypted: false }}
      />,
    );
    expect(screen.getByText(/Token 已更新/)).toBeInTheDocument();
    expect(screen.queryByText(/数据源已创建/)).toBeNull();
  });
});
