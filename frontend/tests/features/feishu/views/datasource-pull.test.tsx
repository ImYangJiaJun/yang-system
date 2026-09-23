/**
 * 详情页的三处新增：手动触发拉取、下次自动拉取时间、取选项接口地址。
 *
 * 这三条都有一条「看起来对、实际上静默失效」的失败形态，所以每条都要钉住：
 * 触发后**永远收不了口**（落定判据用错字段）、排程答不出来时**编一个时间**、
 * 地址**硬编码主机名**（换个部署就粘错）。
 */

import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { renderTestApp } from "@test/helpers/render-app";

import {
  bodiesOf,
  datasourceWire,
  jsonResponse,
  listPage,
  stubFeishuApi,
} from "./harness";

const SOURCE_KEY = "expense_category";
const PULL_NOW_PATH = "/api/v1/feishu/datasources/pull-now";

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

function renderDetail() {
  return renderTestApp({
    path: `/feishu/datasources/${DATASOURCE_ID}`,
    authenticated: true,
  });
}

/// 这条数据源的表级主键。后端 `pull_now` 只认它——`PullNowInput` 是
/// `deny_unknown_fields` + 必填 `datasource_id`，发 `source_key` 100% 被拒。
const DATASOURCE_ID = 7;

/// 一条坐标齐备、能自动拉取的数据源。
///
/// **刻意不带** `source_key` / `bitable_field_name`：这两个键在表级行上早已不存在
/// （绑定标识与取数列都在 `fields[]` 里）。fixture 比真实投影「宽」正是这类 bug
/// 长期全绿的原因——详情页曾经就是拿表级 `source_key` 去查一张没有这一列的表。
function pullSource(overrides: Record<string, unknown> = {}) {
  return datasourceWire({
    id: DATASOURCE_ID,
    title: "费用分类",
    ingest_mode: "pull",
    bitable_base_token: "ZoCWb82JQaCCiAspCqbcUvlsnwg",
    bitable_table_id: "tblauuOafa4acvT3",
    bitable_view_id: null,
    last_pull_at: 1758000000,
    last_success_at: 1758000000,
    consecutive_failures: 0,
    fields: [
      {
        field_id: "fldEblAr7X",
        field_name: "费用类型/Fee Type*",
        source_key: SOURCE_KEY,
        parent_field_id: null,
        enabled: true,
        token_rotated_at: null,
      },
    ],
    ...overrides,
  });
}

describe("详情页 · 下次自动拉取", () => {
  it("显示服务端报的下次时间与配置的间隔", async () => {
    const nextRunAt = Math.floor(Date.now() / 1000) + 720;
    stubFeishuApi({
      datasourceList: () => listPage([pullSource()]),
      optionList: () => listPage([]),
      pullSchedule: () => ({ interval_seconds: 900, next_run_at: nextRunAt }),
    });
    renderDetail();

    // 行标签是静态的，值要等排程查询落定——先 await 值，别 await 标签。
    expect(await screen.findByText(/约 12 分钟后/)).toBeInTheDocument();
    expect(screen.getByText("下次自动拉取")).toBeInTheDocument();
    expect(screen.getByText(/间隔 15 分钟/)).toBeInTheDocument();
  });

  it("排程答不出来时说「正在拉取」，不编一个时间", async () => {
    stubFeishuApi({
      datasourceList: () => listPage([pullSource()]),
      optionList: () => listPage([]),
      pullSchedule: () => ({ interval_seconds: 900, next_run_at: null }),
    });
    renderDetail();

    expect(await screen.findByText("正在拉取")).toBeInTheDocument();
  });
});

describe("详情页 · 立即拉取", () => {
  it("点击会触发后端，并在状态更新后收口", async () => {
    const user = userEvent.setup();
    // 模拟后台跑完一轮：受理之后，下一次列表查询就能看到新的 last_pull_at。
    let lastPullAt = 1758000000;
    const calls = stubFeishuApi({
      datasourceList: () =>
        listPage([pullSource({ last_pull_at: lastPullAt })]),
      optionList: () => listPage([]),
      pullSchedule: () => ({ interval_seconds: 900, next_run_at: null }),
      pullNow: () => {
        lastPullAt = 1758000900;
        return { accepted: true };
      },
    });
    renderDetail();

    await user.click(await screen.findByRole("button", { name: /立即拉取/ }));

    await waitFor(
      () => expect(screen.getByText(/这一轮已经跑过了/)).toBeInTheDocument(),
      { timeout: 5_000 },
    );
    // 后端 `pull_now.rs` 的 `PullNowInput` 是 `#[serde(deny_unknown_fields)]`
    // 加一个必填的 `datasource_id`：带 `source_key` 的请求**必然被拒**
    // （多一个未知键）。所以这里钉的是「网线上到底是什么形状」，
    // 而不是「发出去过一次请求」。
    const body = bodiesOf(calls, PULL_NOW_PATH)[0];
    expect(body).not.toHaveProperty("source_key");
    expect(body).toEqual({ datasource_id: DATASOURCE_ID });
  });

  it("后端在触发前就拒绝时，把原因原样显示出来", async () => {
    // 这是后端那道预检的价值所在：模式不对的源会被 worker 静默跳过，
    // 不在这里挡掉，用户只能等到轮询超时。
    const user = userEvent.setup();
    stubFeishuApi({
      datasourceList: () => listPage([pullSource()]),
      optionList: () => listPage([]),
      pullSchedule: () => ({ interval_seconds: 900, next_run_at: null }),
      pullNow: () =>
        jsonResponse({
          code: 40903,
          message: "取数方式是「手工推送」——服务端不主动出网",
          data: null,
        }),
    });
    renderDetail();

    await user.click(await screen.findByRole("button", { name: /立即拉取/ }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "取数方式是「手工推送」",
    );
  });

  it("目录里没有这个 Action 时不渲染按钮", async () => {
    // 服务端只在 can_pull() 时才注册它。渲染一个点了必然报错的按钮，
    // 等于把「这个部署没开导出站拉取」错报成一次功能故障。
    stubFeishuApi({
      datasourceList: () => listPage([pullSource()]),
      optionList: () => listPage([]),
      pullSchedule: () => ({ interval_seconds: 900, next_run_at: null }),
      datasourceWrite: false,
    });
    renderDetail();

    expect(await screen.findByText("下次自动拉取")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /立即拉取/ }),
    ).not.toBeInTheDocument();
  });
});

describe("详情页 · 取选项接口地址", () => {
  it("凭据清单里每字段一个地址，复制的是显示的那一份原值", async () => {
    const user = userEvent.setup();
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });

    stubFeishuApi({
      datasourceList: () => listPage([pullSource()]),
      optionList: () => listPage([]),
      pullSchedule: () => ({ interval_seconds: 900, next_run_at: null }),
    });
    renderDetail();

    // 一条数据源有 N 个字段 = N 个地址，所以地址在**凭据清单**里一行一个：
    // 页面顶部那块「取选项接口地址」在表级化之后没有单一值可填，已经删掉。
    const shown = await screen.findByText(
      /\/api\/v1\/feishu\/approval\/options\/expense_category$/,
    );
    await user.click(screen.getByRole("button", { name: "复制 URL" }));

    // 复制的是**显示的那一份原值**——截断只影响呈现，不影响剪贴板。
    expect(writeText).toHaveBeenCalledWith(shown.textContent);
  });
});
