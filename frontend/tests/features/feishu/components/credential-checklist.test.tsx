/**
 * 凭据拷贝清单与体检面板。
 *
 * 这一份用例的中心是**决策 D10**：复制只回显，轮换是独立按钮。
 * 「复制是纯读」不是一句界面文案——它意味着误点复制不能有任何后果
 * （每个字段一份凭据，而粘贴它们的是审批后台里已经配好的控件），
 * 所以这里用桩把「点复制时到底调了什么」逐条钉住：
 * 复制 URL 一个请求都不发；复制 Token 只发**回显**（读），绝不发轮换（写）。
 */

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { CredentialChecklist } from "@/features/feishu/components/CredentialChecklist";
import { DatasourceHealthPanel } from "@/features/feishu/components/DatasourceHealthPanel";
import type { CredentialClient } from "@/features/feishu/api";
import type { CredentialItem, HealthReport } from "@/features/feishu/types";

const ORIGIN = "https://ops.example.com";
const SOURCE_KEY = "fee_type";

const itemFixture: CredentialItem = {
  fieldId: "fldEblAr7X",
  fieldName: "费用类型/Fee Type*",
  sourceKey: SOURCE_KEY,
  tokenRotatedAt: 1758000000,
  enabled: true,
};

/// 体检报告夹具：一个字段被删（`fldGONE` / `old_rate`），表与视图都在。
const reportFixture: HealthReport = {
  ok: false,
  missingFields: [{ fieldId: "fldGONE", sourceKey: "old_rate" }],
  viewMissing: false,
  tableMissing: false,
  unchecked: [],
};

function stubClient(overrides: Partial<CredentialClient> = {}): {
  client: CredentialClient;
  reveal: ReturnType<typeof vi.fn>;
  rotate: ReturnType<typeof vi.fn>;
} {
  const reveal = vi.fn().mockResolvedValue("plaintext-token");
  const rotate = vi.fn().mockResolvedValue("rotated-token");
  const client: CredentialClient = { reveal, rotate, ...overrides };
  return { client, reveal, rotate };
}

function stubClipboard() {
  const writeText = vi.fn().mockResolvedValue(undefined);
  vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
  return writeText;
}

describe("凭据拷贝清单", () => {
  it("每行有两个可复制项：URL 与 Token", async () => {
    // 本次不做 Key 加密（设计决策 D11），所以是两项不是三项
    const { client } = stubClient();
    render(
      <CredentialChecklist
        items={[itemFixture]}
        origin={ORIGIN}
        client={client}
      />,
    );

    expect(screen.getAllByRole("button", { name: /复制/ })).toHaveLength(2);
    expect(
      screen.getByText(
        `${ORIGIN}/api/v1/feishu/approval/options/${SOURCE_KEY}`,
      ),
    ).toBeInTheDocument();
    // 字段名与控件标识只是标签，但它们必须看得见——不然不知道在配哪一列
    expect(screen.getByText("费用类型/Fee Type*")).toBeInTheDocument();
  });

  it("轮换按钮二次确认并说明后果", async () => {
    const user = userEvent.setup();
    const { client, rotate } = stubClient();
    render(
      <CredentialChecklist
        items={[itemFixture]}
        origin={ORIGIN}
        client={client}
      />,
    );

    // 点开确认框只是弹窗，**还没轮换**
    await user.click(screen.getByRole("button", { name: "轮换" }));
    expect(
      await screen.findByText(/已配置该字段的控件会立即失效/),
    ).toBeInTheDocument();
    expect(screen.getByText(/粘回审批后台/)).toBeInTheDocument();
    expect(rotate).not.toHaveBeenCalled();

    // 取消就什么都不发生
    await user.click(screen.getByRole("button", { name: "取消" }));
    expect(rotate).not.toHaveBeenCalled();
  });

  it("确认轮换才真的调轮换端点，并把新的 Token 摆出来给人粘", async () => {
    const user = userEvent.setup();
    const writeText = stubClipboard();
    const { client, rotate } = stubClient();
    const onRotated = vi.fn();
    render(
      <CredentialChecklist
        items={[itemFixture]}
        origin={ORIGIN}
        client={client}
        onRotated={onRotated}
      />,
    );

    await user.click(screen.getByRole("button", { name: "轮换" }));
    await user.click(await screen.findByRole("button", { name: "轮换 Token" }));

    expect(rotate).toHaveBeenCalledWith(SOURCE_KEY);
    expect(onRotated).toHaveBeenCalledWith(SOURCE_KEY, "rotated-token");
    // 轮换后的新值必须可复制——它就是要粘回审批后台的那一串
    await user.click(
      screen.getAllByRole("button", { name: /复制/ })[1] as HTMLElement,
    );
    expect(writeText).toHaveBeenCalledWith("rotated-token");
    vi.unstubAllGlobals();
  });

  it("复制不会触发任何写请求", async () => {
    // 「复制是纯读」是设计硬要求（决策 D10）：误点复制不能有任何后果。
    const user = userEvent.setup();
    const writeText = stubClipboard();
    const { client, reveal, rotate } = stubClient();
    render(
      <CredentialChecklist
        items={[itemFixture]}
        origin={ORIGIN}
        client={client}
      />,
    );

    const copyButtons = screen.getAllByRole("button", { name: /复制/ });

    // 复制 URL：一个客户端方法都不该被摸到（地址是本地就能拼出来的）
    await user.click(copyButtons[0] as HTMLElement);
    expect(writeText).toHaveBeenCalledWith(
      `${ORIGIN}/api/v1/feishu/approval/options/${SOURCE_KEY}`,
    );
    expect(reveal).not.toHaveBeenCalled();
    expect(rotate).not.toHaveBeenCalled();

    // 复制 Token：只回显（读），仍然绝不写
    await user.click(
      screen.getAllByRole("button", { name: /复制/ })[1] as HTMLElement,
    );
    expect(reveal).toHaveBeenCalledWith(SOURCE_KEY);
    expect(rotate).not.toHaveBeenCalled();
    expect(writeText).toHaveBeenLastCalledWith("plaintext-token");

    vi.unstubAllGlobals();
  });

  it("回显失败时说清原因，不假装复制成功", async () => {
    const user = userEvent.setup();
    const writeText = stubClipboard();
    const reveal = vi
      .fn()
      .mockRejectedValue(
        new Error("UI 目录里找不到 Action「feishu.datasource.reveal_token」"),
      );
    const { client } = stubClient({ reveal });
    render(
      <CredentialChecklist
        items={[itemFixture]}
        origin={ORIGIN}
        client={client}
      />,
    );

    await user.click(
      (
        await screen.findAllByRole("button", { name: /复制/ })
      )[1] as HTMLElement,
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(/找不到 Action/);
    expect(writeText).not.toHaveBeenCalled();
    vi.unstubAllGlobals();
  });

  it("没有回显权限的部署：轮换按钮给出可归因的说明，而不是静默失败", async () => {
    const user = userEvent.setup();
    render(<CredentialChecklist items={[itemFixture]} origin={ORIGIN} />);

    expect(screen.getByText(/没有回显与轮换凭据的权限/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "轮换" }));
    expect(
      await screen.findByRole("button", { name: "轮换 Token" }),
    ).toBeDisabled();
  });

  it("轮换时间拿不到时显示「—」，不谎称「从未轮换」", () => {
    // 列表端点的绑定投影里没有 `token_rotated_at`，所以「拿不到」是常态。
    // 把它画成「从未轮换」是一句可查证的假话。
    const { client } = stubClient();
    const { unmount } = render(
      <CredentialChecklist
        items={[
          {
            fieldId: "fldEblAr7X",
            fieldName: "费用类型/Fee Type*",
            sourceKey: SOURCE_KEY,
            enabled: true,
          },
        ]}
        origin={ORIGIN}
        client={client}
      />,
    );
    expect(screen.getByText("—")).toBeInTheDocument();
    expect(screen.queryByText("从未轮换")).toBeNull();
    unmount();

    // 明确知道没轮换过时才那么说
    render(
      <CredentialChecklist
        items={[{ ...itemFixture, tokenRotatedAt: null }]}
        origin={ORIGIN}
        client={client}
      />,
    );
    expect(screen.getByText("从未轮换")).toBeInTheDocument();
  });

  it("多个字段时每行各自一套复制项与轮换按钮", () => {
    const { client } = stubClient();
    render(
      <CredentialChecklist
        items={[
          itemFixture,
          { ...itemFixture, fieldId: "fldA", sourceKey: "currency" },
        ]}
        origin={ORIGIN}
        client={client}
      />,
    );
    expect(screen.getAllByRole("button", { name: /复制/ })).toHaveLength(4);
    expect(screen.getAllByRole("button", { name: "轮换" })).toHaveLength(2);
  });
});

describe("体检面板", () => {
  it("把缺失的字段按 id 与 source_key 一起列出", () => {
    // 只报 field_id 运维看不懂；只报 source_key 又定位不到列。两个都要给。
    render(<DatasourceHealthPanel report={reportFixture} />);
    expect(screen.getByText("fldGONE")).toBeInTheDocument();
    expect(screen.getByText("old_rate")).toBeInTheDocument();
    // 说明这是「被删除」而不是「被改名」（改名能自愈，不该出现在这里），
    // 并给出下一步动作
    expect(
      screen.getByText(/已被删除：在向导里取消勾选它/),
    ).toBeInTheDocument();
  });

  it("全绿时明说通过", () => {
    render(
      <DatasourceHealthPanel
        report={{
          ok: true,
          missingFields: [],
          viewMissing: false,
          tableMissing: false,
          unchecked: [],
        }}
      />,
    );
    expect(screen.getByText(/体检通过/)).toBeInTheDocument();
  });

  it("查不成的项不许被读成「一切正常」", () => {
    // 网络失败 ≠ 没问题。它压住 ok，界面就不能只显示一句「通过」。
    render(
      <DatasourceHealthPanel
        report={{
          ok: false,
          missingFields: [],
          viewMissing: false,
          tableMissing: false,
          unchecked: ["列出字段：定时拉取未启用"],
        }}
      />,
    );
    expect(screen.queryByText(/体检通过/)).toBeNull();
    expect(screen.getByText(/定时拉取未启用/)).toBeInTheDocument();
    expect(screen.getByText(/这次没查成/)).toBeInTheDocument();
  });

  it("表与视图没了也要各说一句，不和缺字段混为一谈", () => {
    render(
      <DatasourceHealthPanel
        report={{
          ok: false,
          missingFields: [],
          viewMissing: true,
          tableMissing: true,
          unchecked: [],
        }}
      />,
    );
    expect(screen.getByText(/数据表已不存在/)).toBeInTheDocument();
    expect(screen.getByText(/视图已不存在/)).toBeInTheDocument();
  });

  it("还没体检过时不渲染一个空结论", () => {
    const { container } = render(<DatasourceHealthPanel report={null} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("体检失败时回显原因并给重试入口", async () => {
    const user = userEvent.setup();
    const onRecheck = vi.fn();
    render(
      <DatasourceHealthPanel
        report={null}
        error="定时拉取未启用，体检需要出站查一次字段"
        onRecheck={onRecheck}
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent(/定时拉取未启用/);
    await user.click(screen.getByRole("button", { name: "重新体检" }));
    expect(onRecheck).toHaveBeenCalled();
  });
});
