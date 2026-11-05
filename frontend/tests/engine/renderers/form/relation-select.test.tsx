import { useState } from "react";

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RelationSelect } from "@/engine/renderers/form/RelationSelect";
import { clearStoredSession } from "@/engine/session/auth-session";
import { createSessionController } from "@/engine/session/session-controller";
import { SessionControllerContext } from "@/engine/session/use-session";

/**
 * 关系选择器的可用性回归（2026-11）：
 * 1. 搜索后已选值仍显示标签，不退化成裸 ID；
 * 2. 非必填关系字段可以清空。
 *
 * RelationSelect 通过 useSessionCredentials 取会话；注入内存 Token 即可，
 * 不需要整棵应用树。
 */

function jsonResponse(payload: unknown) {
  return new Response(JSON.stringify(payload), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

const field = {
  field: "category_id",
  title: "分类",
  description: "",
  widget: "relation_select" as const,
  required: false,
  read_only: false,
  write_only: false,
  relation: {
    operation_id: "demo.categories.options",
    value_field: "id",
    label_fields: ["name"],
  },
};

const action = {
  operation_id: "demo.categories.options",
  title: "分类选项",
  description: "",
  method: "POST" as const,
  path: "/api/v1/demo/categories/options",
  params: [],
  input_schema: {},
  output_schema: {},
  request_media_type: "json" as const,
  response_kind: "json" as const,
  requires_auth: true,
};

function Controller({ children }: { children: React.ReactNode }) {
  // 控制器必须只创建一次：每次渲染新建会让 useSyncExternalStore 的订阅
  // 每帧重建，测试表现为超时。
  const [controller] = useState(() => {
    const created = createSessionController();
    created.beginSession({ accessToken: "test-access" });
    return created;
  });
  return (
    <SessionControllerContext.Provider value={controller}>
      {children}
    </SessionControllerContext.Provider>
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  clearStoredSession();
});

describe("RelationSelect", () => {
  it("搜索后已选值仍显示标签，并可清空", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
        const body = init?.body ? JSON.parse(String(init.body)) : {};
        // 有搜索词时只返回别的记录：当前选中的 7 号「李四」不在结果里。
        const items = body.search
          ? [{ value: 9, label: "张三" }]
          : [{ value: 7, label: "李四" }];
        return jsonResponse({
          code: 0,
          message: "成功",
          data: { items, page: 1, limit: 20, total: 1 },
        });
      }),
    );
    const onChange = vi.fn();

    render(
      <Controller>
        <RelationSelect
          value={7}
          onChange={onChange}
          label="分类"
          field={field}
          action={action}
        />
      </Controller>,
    );

    // 初始已选中 7：标签取回后显示「李四」。
    expect(await screen.findByText("李四")).toBeInTheDocument();

    // 搜「张」：7 不在远端结果里，但下拉里必须仍有「李四」而不是裸 "7"。
    await userEvent.type(screen.getByLabelText("搜索分类"), "张");
    await userEvent.click(screen.getByRole("combobox", { name: "分类" }));
    expect(
      await screen.findByRole("option", { name: "张三" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "李四" })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: "7" })).toBeNull();
    await userEvent.keyboard("{Escape}");

    await userEvent.click(screen.getByRole("button", { name: "清除分类" }));
    expect(onChange).toHaveBeenCalledWith(undefined);
  });
});
