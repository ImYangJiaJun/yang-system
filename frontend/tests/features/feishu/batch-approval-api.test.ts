import { afterEach, describe, expect, it, vi } from "vitest";

import {
  fetchBatchTasks,
  fetchBatchViewConfig,
  saveBatchViewConfig,
  taskOperability,
} from "@/features/feishu/batch-approval-api";

function jsonResponse(payload: unknown) {
  return new Response(JSON.stringify(payload), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("batch approval task parsing", () => {
  it("maps numeric pending status and drops duplicate task rows", async () => {
    const task = {
      task_id: "t1",
      instance_code: "i1",
      title: "测试审批",
      serial_number: "202611200024",
      status: 1,
      support_api_operate: true,
    };
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        jsonResponse({
          code: 0,
          data: { tasks: [task, task], truncated: false },
        }),
      ),
    );

    const { tasks } = await fetchBatchTasks("token");

    expect(tasks).toHaveLength(1);
    expect(tasks[0]?.status).toBe("PENDING");
    expect(tasks[0]?.serialNumber).toBe("202611200024");
    expect(taskOperability(tasks[0]!).operable).toBe(true);
  });
});

describe("batch approval view config", () => {
  it("reads and writes column display and copy options", async () => {
    const posted: Array<{ columns: Array<Record<string, unknown>> }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
        if (init?.method === "POST") {
          posted.push(JSON.parse(String(init.body)));
          return jsonResponse({
            code: 0,
            data: { view: JSON.parse(String(init.body)) },
          });
        }
        return jsonResponse({
          code: 0,
          data: {
            view: {
              computed_fields: [],
              columns: [
                {
                  key: "form:account",
                  visible: true,
                  display_mode: "full",
                  copyable: true,
                  trim_whitespace: true,
                },
                { key: "form:amount", visible: true },
              ],
            },
          },
        });
      }),
    );

    const view = await fetchBatchViewConfig("D1", "token");
    expect(view.columns[0]).toMatchObject({
      displayMode: "full",
      copyable: true,
      trimWhitespace: true,
    });
    // 旧配置没有新字段时按「固定宽度、不可复制」处理。
    expect(view.columns[1]).toMatchObject({
      displayMode: "fixed",
      copyable: false,
      trimWhitespace: false,
    });

    await saveBatchViewConfig("D1", view, "token");
    expect(posted).toHaveLength(1);
    expect(posted[0]?.columns[0]).toMatchObject({
      key: "form:account",
      display_mode: "full",
      copyable: true,
      trim_whitespace: true,
    });
  });
});
