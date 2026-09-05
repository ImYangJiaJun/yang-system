import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import App from "@/shell/App";

function jsonResponse(payload: unknown, status: number) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
});

describe("App", () => {
  it("匿名启动经会话恢复门控落到登录页", async () => {
    // 无 Refresh Cookie：恢复失败 → anonymous → 登录页。
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        jsonResponse({ code: 40102, message: "刷新会话 Cookie 缺失" }, 401),
      ),
    );

    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "用户登录" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "YANG System" }),
    ).toBeInTheDocument();
  });
});
