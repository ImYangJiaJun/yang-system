import { afterEach, describe, expect, it, vi } from "vitest";

import {
  fetchBatchTasks,
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
      status: 1,
      support_api_operate: true,
    };
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValue(
          jsonResponse({
            code: 0,
            data: { tasks: [task, task], truncated: false },
          }),
        ),
    );

    const { tasks } = await fetchBatchTasks("token");

    expect(tasks).toHaveLength(1);
    expect(tasks[0]?.status).toBe("PENDING");
    expect(taskOperability(tasks[0]!).operable).toBe(true);
  });
});
