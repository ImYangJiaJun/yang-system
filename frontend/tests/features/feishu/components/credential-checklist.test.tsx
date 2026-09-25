/**
 * 凭据拷贝清单与体检面板。
 *
 * 这一份用例的中心是**决策 D10**：复制只回显，轮换是独立按钮。
 * 「复制是纯读」不是一句界面文案——它意味着误点复制不能有任何后果
 * （每个字段一份凭据，而粘贴它们的是审批后台里已经配好的控件），
 * 所以这里用桩把「点复制时到底调了什么」逐条钉住：
 * 复制 URL 与复制联动 key 一个请求都不发；复制 Token 只发**回显**（读），
 * 绝不发轮换（写）。
 *
 * 另一条中心是**三项只在有父字段时才是三项**：联动 key 那一项的有无、
 * 以及它的值取自父绑定，都在这里逐条钉住——填错一个字符的后果是静默的。
 */

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CredentialChecklist } from "@/features/feishu/components/CredentialChecklist";
import { DatasourceHealthPanel } from "@/features/feishu/components/DatasourceHealthPanel";
import type { CredentialClient } from "@/features/feishu/api";
import type { CredentialItem, HealthReport } from "@/features/feishu/types";
import {
  restoreClipboard,
  stubExecCommand,
  stubInsecureClipboard,
} from "@test/helpers/clipboard";

afterEach(restoreClipboard);

const ORIGIN = "https://ops.example.com";
const SOURCE_KEY = "fee_type";
const FIELD_NAME = "费用类型/Fee Type*";

const URL_OF_ITEM = `${ORIGIN}/api/v1/feishu/approval/options/${SOURCE_KEY}`;

const itemFixture: CredentialItem = {
  fieldId: "fldEblAr7X",
  fieldName: FIELD_NAME,
  sourceKey: SOURCE_KEY,
  tokenRotatedAt: 1758000000,
  enabled: true,
  parent: null,
};

/// 可访问名的三处拼装。抽出来是为了让「按钮叫什么」这件事在用例里只有一处定义
/// ——一页多行、每行最多三个复制点，名字里必须点名是哪一个。
const copyName = (what: string, name = FIELD_NAME) => `复制${what}：${name}`;
const rotateName = (name = FIELD_NAME) => `轮换 Token：${name}`;

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
  it("没有父字段的块只有两项可复制：接口地址与 Token", () => {
    // 本次不做 Key 加密（设计决策 D11），控件里那一格留空——它与联动 key 不是一回事
    const { client } = stubClient();
    render(
      <CredentialChecklist
        items={[itemFixture]}
        origin={ORIGIN}
        client={client}
      />,
    );

    expect(screen.getAllByRole("button", { name: /^复制/ })).toHaveLength(2);
    expect(screen.queryByText("联动 key")).toBeNull();
    expect(screen.getByText(URL_OF_ITEM)).toBeInTheDocument();
    // 字段名与控件标识只是标签，但它们必须看得见——不然不知道在配哪一列
    expect(screen.getByText(FIELD_NAME)).toBeInTheDocument();
  });

  it("接口地址整串渲染出来，不截断（它是这一页唯一的产出物）", () => {
    // 上一版是表格列 + `truncate`，896px 的内容列里被截成
    // `…/options/expense_categ…`，全文只能靠悬停 title 看。
    const { client } = stubClient();
    render(
      <CredentialChecklist
        items={[itemFixture]}
        origin={ORIGIN}
        client={client}
      />,
    );
    const url = screen.getByText(URL_OF_ITEM);
    expect(url).toBeInTheDocument();
    expect(url.className).toContain("break-all");
    expect(url.className).not.toContain("truncate");
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
    await user.click(screen.getByRole("button", { name: rotateName() }));
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

    await user.click(screen.getByRole("button", { name: rotateName() }));
    await user.click(await screen.findByRole("button", { name: "轮换 Token" }));

    expect(rotate).toHaveBeenCalledWith(SOURCE_KEY);
    expect(onRotated).toHaveBeenCalledWith(SOURCE_KEY, "rotated-token");
    // 轮换后的新值必须可复制——它就是要粘回审批后台的那一串
    await user.click(screen.getByRole("button", { name: copyName(" Token") }));
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

    // 复制 URL：一个客户端方法都不该被摸到（地址是本地就能拼出来的）
    await user.click(
      screen.getByRole("button", { name: copyName("接口地址") }),
    );
    expect(writeText).toHaveBeenCalledWith(URL_OF_ITEM);
    expect(reveal).not.toHaveBeenCalled();
    expect(rotate).not.toHaveBeenCalled();

    // 复制 Token：只回显（读），仍然绝不写
    await user.click(screen.getByRole("button", { name: copyName(" Token") }));
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

    await user.click(screen.getByRole("button", { name: copyName(" Token") }));

    expect(await screen.findByRole("alert")).toHaveTextContent(/找不到 Action/);
    expect(writeText).not.toHaveBeenCalled();
    vi.unstubAllGlobals();
  });

  it("没有回显权限的部署：轮换按钮给出可归因的说明，而不是静默失败", async () => {
    const user = userEvent.setup();
    render(<CredentialChecklist items={[itemFixture]} origin={ORIGIN} />);

    expect(screen.getByText(/没有回显与轮换凭据的权限/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: rotateName() }));
    expect(
      await screen.findByRole("button", { name: "轮换 Token" }),
    ).toBeDisabled();
  });

  it("轮换时间拿不到时不说「从未轮换」——那是两句不同的事实", () => {
    // 列表端点的绑定投影里可能没有 `token_rotated_at`，所以「拿不到」是常态。
    // 把它画成「从未轮换」是一句可查证的假话。
    const { client } = stubClient();
    const { unmount } = render(
      <CredentialChecklist
        items={[
          {
            fieldId: "fldEblAr7X",
            fieldName: FIELD_NAME,
            sourceKey: SOURCE_KEY,
            enabled: true,
            parent: null,
          },
        ]}
        origin={ORIGIN}
        client={client}
      />,
    );
    expect(screen.getByText("轮换时间未知")).toBeInTheDocument();
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

  it("多个字段时每块各自一套复制项与轮换按钮", () => {
    const { client } = stubClient();
    render(
      <CredentialChecklist
        items={[
          itemFixture,
          {
            ...itemFixture,
            fieldId: "fldA",
            fieldName: "币种",
            sourceKey: "currency",
            parent: null,
          },
        ]}
        origin={ORIGIN}
        client={client}
      />,
    );
    expect(screen.getAllByRole("button", { name: /^复制/ })).toHaveLength(4);
    expect(
      screen.getAllByRole("button", { name: /^轮换 Token：/ }),
    ).toHaveLength(2);
  });
});

/**
 * 联动 key：只有带父字段的绑定才有这一项，值是**父绑定**的 field_id。
 *
 * 它填进审批后台那个联动控件的「参数代码」，是级联精确匹配的唯一依据；
 * 服务端从不比较审批表单里 widget 形态的控件代码。判据写错一处（取成子自己的
 * field_id、或取成父的 source_key）都不会报错——只会让运维把一个没用的值
 * 粘进审批后台，然后级联静默退化。
 */
describe("凭据拷贝清单：联动 key", () => {
  const childItem: CredentialItem = {
    ...itemFixture,
    fieldId: "fldChild",
    fieldName: "明细科目",
    sourceKey: "expense_subject",
    parent: {
      linkageKey: "fldParent",
      label: "费用类别",
      enabled: true,
    },
  };

  it("有父字段的块多一项「联动 key」，值是父的字段 id", () => {
    const { client } = stubClient();
    render(
      <CredentialChecklist
        items={[childItem]}
        origin={ORIGIN}
        client={client}
      />,
    );

    expect(screen.getByText("联动 key")).toBeInTheDocument();
    expect(screen.getByText("fldParent")).toBeInTheDocument();
    // 三项都可复制
    expect(screen.getAllByRole("button", { name: /^复制/ })).toHaveLength(3);
    // 说清它是给哪个控件用的
    expect(screen.getByText(/父字段「费用类别」/)).toBeInTheDocument();
  });

  it("复制联动 key 是纯本地操作：不发任何请求", async () => {
    // 与接口地址同理——值本来就渲染在块里，复制不该摸到回显或轮换。
    const user = userEvent.setup();
    const writeText = stubClipboard();
    const { client, reveal, rotate } = stubClient();
    render(
      <CredentialChecklist
        items={[childItem]}
        origin={ORIGIN}
        client={client}
      />,
    );

    await user.click(
      screen.getByRole("button", { name: copyName("联动 key", "明细科目") }),
    );
    expect(writeText).toHaveBeenCalledWith("fldParent");
    expect(reveal).not.toHaveBeenCalled();
    expect(rotate).not.toHaveBeenCalled();
    vi.unstubAllGlobals();
  });

  it("复制联动 key 失败时只指路，不把本来就显示着的值再摆一遍", async () => {
    const user = userEvent.setup();
    stubInsecureClipboard();
    stubExecCommand(false);
    const { client } = stubClient();
    render(
      <CredentialChecklist
        items={[childItem]}
        origin={ORIGIN}
        client={client}
      />,
    );

    await user.click(
      screen.getByRole("button", { name: copyName("联动 key", "明细科目") }),
    );

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/「联动 key」就在下面那一块里/);
    expect(alert).toHaveTextContent("expense_subject");
  });

  it("父字段已停用时点明这条级联现在取不到选项", () => {
    // 服务端 `load_parent_source_key` 要求父启用中；父停了就按无父处理。
    // 不说这一句，那串 key 会看起来是「填上就能用」的。
    const { client } = stubClient();
    render(
      <CredentialChecklist
        items={[
          { ...childItem, parent: { ...childItem.parent!, enabled: false } },
        ]}
        origin={ORIGIN}
        client={client}
      />,
    );
    expect(
      screen.getByText(/父字段「费用类别」已停用，这条级联现在取不到选项/),
    ).toBeInTheDocument();
  });

  it("父被停用时联动 key 照旧可复制——它仍是这一行唯一能指认父的东西", () => {
    const { client } = stubClient();
    render(
      <CredentialChecklist
        items={[
          { ...childItem, parent: { ...childItem.parent!, enabled: false } },
        ]}
        origin={ORIGIN}
        client={client}
      />,
    );
    expect(screen.getByText("fldParent")).toBeInTheDocument();
  });
});

/**
 * 明文 HTTP 部署下的复制（2026-09-24 事故）。
 *
 * 现场是 `http://<公网IP>:18654`：`navigator.clipboard` 整块缺席，所有复制按钮
 * 全部「点了没反应」。这一页比别处多一层风险——**Token 是唯一一处「复制不成，
 * 就彻底拿不到」的值**：它只进本地状态、块里从不渲染，而「轮换」却会成功并让
 * 已配好的飞书控件立刻失效。所以修好之后必须满足：能复制就真复制，不能就摆出来。
 */
describe("凭据清单：明文 HTTP 部署下的复制", () => {
  it("非安全上下文：降级路径仍把 Token 复制成功，不误报失败", async () => {
    const user = userEvent.setup();
    stubInsecureClipboard();
    const execCommand = stubExecCommand(true);
    const { client, reveal } = stubClient();
    render(
      <CredentialChecklist
        items={[itemFixture]}
        origin={ORIGIN}
        client={client}
      />,
    );

    await user.click(screen.getByRole("button", { name: copyName(" Token") }));

    // 回显照旧只读一次，复制走降级路径，且不该出现任何错误提示
    expect(reveal).toHaveBeenCalledWith(SOURCE_KEY);
    expect(execCommand).toHaveBeenCalledWith("copy");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("两条路都不可用时把 Token 明文摆出来，而不是「点了没反应」", async () => {
    const user = userEvent.setup();
    stubInsecureClipboard();
    stubExecCommand(false);
    const { client } = stubClient();
    render(
      <CredentialChecklist
        items={[itemFixture]}
        origin={ORIGIN}
        client={client}
      />,
    );

    await user.click(screen.getByRole("button", { name: copyName(" Token") }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/只能手动复制/);
    // 明文必须真的摆出来，且能一键全选——不给就等于把人送进死角
    expect(alert).toHaveTextContent("plaintext-token");
    expect(screen.getByText("plaintext-token")).toHaveClass("select-all");
  });

  it("复制接口地址失败时也给退路，但不重复那串本来就显示着的地址", async () => {
    const user = userEvent.setup();
    stubInsecureClipboard();
    stubExecCommand(false);
    const { client, reveal } = stubClient();
    render(
      <CredentialChecklist
        items={[itemFixture]}
        origin={ORIGIN}
        client={client}
      />,
    );

    await user.click(
      screen.getByRole("button", { name: copyName("接口地址") }),
    );

    const alert = await screen.findByRole("alert");
    // 提示挂在清单**上方**，所以指路必须往下指；并且要点名是哪一项——
    // 多块多行时「上面那一行」既指错方向、也对不上号。
    expect(alert).toHaveTextContent(/「接口地址」就在下面那一块里/);
    expect(alert).toHaveTextContent(SOURCE_KEY);
    // 地址本来就渲染在块里；再往提示里塞一遍只是噪音
    expect(alert).not.toHaveTextContent(ORIGIN);
    expect(reveal).not.toHaveBeenCalled();
  });

  it("轮换之后不再摆着上一次复制失败留下的旧明文——它那一刻就作废了", async () => {
    const user = userEvent.setup();
    stubInsecureClipboard();
    stubExecCommand(false);
    const { client } = stubClient();
    render(
      <CredentialChecklist
        items={[itemFixture]}
        origin={ORIGIN}
        client={client}
      />,
    );

    // 复制失败 → 旧明文被摆出来（这是它唯一出现的场合）
    await user.click(screen.getByRole("button", { name: copyName(" Token") }));
    expect(await screen.findByText("plaintext-token")).toBeInTheDocument();

    // 轮换把旧值作废了：屏幕上要是还留着那串，用户就会照着它往审批后台粘
    await user.click(screen.getByRole("button", { name: rotateName() }));
    await user.click(await screen.findByRole("button", { name: "轮换 Token" }));

    expect(screen.queryByText("plaintext-token")).toBeNull();
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
