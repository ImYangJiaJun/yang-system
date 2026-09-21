import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { TokenPrecheckNotice } from "@/features/feishu/components/TokenPrecheckNotice";
import {
  APPROVAL_OPTION_CODES,
  approvalCodeHint,
  approvalCodeVerdict,
} from "@/features/feishu/types";

/// 创建后的连通性预检回执：通了要给可复制的一段，失败了要说清「数据源已创建」。

/// 造一个失败回执：结论与指引都走真实词汇表（`approvalCodeVerdict` / `approvalCodeHint`），
/// 这样测的是「界面按码说话」，而不是「界面复述了测试里的字符串」。
function failure(code: number, message: string) {
  return {
    status: "failed" as const,
    code,
    message,
    verdict: approvalCodeVerdict(code),
    hint: approvalCodeHint(code),
  };
}

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
        result={{
          status: "ok",
          optionCount: 3,
          hasMore: false,
          encrypted: false,
        }}
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

  it("失败：说清「数据源已创建、但这条链路没验通」，并给出码与含义", () => {
    render(
      <TokenPrecheckNotice
        sourceKey="dept_sales"
        result={failure(40102, "token 校验失败")}
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
        result={failure(40102, "token 校验失败")}
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
        result={{
          status: "ok",
          optionCount: 0,
          hasMore: false,
          encrypted: false,
        }}
      />,
    );
    expect(screen.getByText(/Token 已更新/)).toBeInTheDocument();
    expect(screen.queryByText(/数据源已创建/)).toBeNull();
  });

  it("服务端说有下一页：只说「还有更多」，不报那个属于本页的条数", () => {
    // 单页上限 100：超过 100 条时本页恰好 100，把它说成总数就是一句精确的假话。
    render(
      <TokenPrecheckNotice
        sourceKey="dept_sales"
        result={{
          status: "ok",
          optionCount: 100,
          hasMore: true,
          encrypted: false,
        }}
      />,
    );

    expect(screen.getByText(/还有更多/)).toBeInTheDocument();
    expect(screen.getByText(/给不出总数/)).toBeInTheDocument();
    // 不出现「（一共）100 个选项」这种总数口吻
    expect(screen.queryByText(/拉到了 100 个选项。$/)).toBeNull();
  });

  it("失败按码说话：Token 其实是对的，就不说「Token 没通过验证」", () => {
    // 40301（数据源已停用）与 50002（服务端没配密钥）里服务端已经比对过 Token 了，
    // 把它们说成凭据没过，会把排查方向引到重填 Token 上。
    for (const [code, message, verdictPattern] of [
      [
        APPROVAL_OPTION_CODES.sourceDisabled,
        "数据源已停用",
        /Token 已经通过了比对/,
      ],
      [
        APPROVAL_OPTION_CODES.encryptionNotConfigured,
        "服务端未配置加密密钥",
        /Token 已经通过了比对/,
      ],
      [
        APPROVAL_OPTION_CODES.sourceNotFound,
        "数据源不存在",
        /Token 对不对还无从谈起/,
      ],
      [APPROVAL_OPTION_CODES.tokenMismatch, "token 校验失败", /Token 没对/],
    ] as const) {
      const { unmount } = render(
        <TokenPrecheckNotice
          sourceKey="dept_sales"
          result={failure(code, message)}
        />,
      );

      expect(screen.getByText(verdictPattern)).toBeInTheDocument();
      expect(screen.getByRole("alert")).toHaveTextContent(message);
      // 一句泛化的失败结论不许盖住上面那条按码的结论
      expect(screen.queryByText(/没有通过验证/)).toBeNull();

      unmount();
    }
  });

  it("「数据源查不到」时不再断言数据源已经就位，也不给「重新填写 Token」", () => {
    const { unmount } = render(
      <TokenPrecheckNotice
        sourceKey="dept_sales"
        result={failure(APPROVAL_OPTION_CODES.sourceNotFound, "数据源不存在")}
        onRotate={vi.fn()}
      />,
    );
    expect(screen.queryByText(/数据源本身已经就位/)).toBeNull();
    expect(screen.getByText(/请回列表页核对/)).toBeInTheDocument();
    // 换 Token 修不了「查不到这一行」，给了就是把人送进死路
    expect(screen.queryByRole("button", { name: "重新填写 Token" })).toBeNull();
    unmount();

    // 其余失败码都说明那一行是存在的，照旧可以这么说
    render(
      <TokenPrecheckNotice
        sourceKey="dept_sales"
        result={failure(APPROVAL_OPTION_CODES.sourceDisabled, "数据源已停用")}
      />,
    );
    expect(screen.getByText(/数据源本身已经就位/)).toBeInTheDocument();
  });
});
