import { useEffect, useRef } from "react";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import {
  StepUpDialogHost,
  type StepUpProofHandler,
} from "@/components/step-up/step-up-host";

/// Step-up 对话框：challenge → 输入凭据 → step-up/complete 换 proof → 重放授权。

function jsonResponse(payload: unknown, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function renderHost(sessionToken?: string) {
  const handlerRef: { current: StepUpProofHandler | undefined } = {
    current: undefined,
  };
  function Harness() {
    const ready = useRef((handler: StepUpProofHandler) => {
      handlerRef.current = handler;
    });
    useEffect(() => undefined, []);
    return <StepUpDialogHost onReady={ready.current} />;
  }
  render(<Harness />);
  return {
    request: (challenge: string) =>
      handlerRef.current!(challenge, { token: sessionToken }),
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("StepUpDialog", () => {
  it("challenge → 输入凭据 → proof 解析返回", async () => {
    const fetchMock = vi.fn(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === "string" ? input : input.toString();
        expect(url).toBe("/api/v1/users/step-up/complete");
        expect(JSON.parse(String(init?.body))).toEqual({
          challenge: "challenge-1",
          credentials: { username: "alice", password: "pw" },
        });
        expect(new Headers(init?.headers).get("authorization")).toBe(
          "Bearer access-token",
        );
        return jsonResponse({
          code: 0,
          message: "成功",
          data: { proof: "one-shot-proof", expires_in: 120 },
        });
      },
    );
    vi.stubGlobal("fetch", fetchMock);

    const { request } = renderHost("access-token");
    let resolved: string | undefined | typeof pending;
    const pending = Symbol("pending");
    resolved = pending;
    const promise = request("challenge-1").then((proof) => {
      resolved = proof;
    });

    await screen.findByRole("dialog");
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("用户名"), "alice");
    await user.type(screen.getByLabelText("密码"), "pw");
    await user.click(screen.getByRole("button", { name: "验证并继续" }));

    await promise;
    expect(resolved).toBe("one-shot-proof");
    expect(fetchMock).toHaveBeenCalledOnce();
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    // proof 不落入 Web Storage。
    expect(JSON.stringify({ ...sessionStorage })).not.toContain(
      "one-shot-proof",
    );
    expect(JSON.stringify({ ...localStorage })).not.toContain("one-shot-proof");
  });

  it("取消返回 undefined 且不发起请求", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    const { request } = renderHost();

    const promise = request("challenge-2");
    await screen.findByRole("dialog");
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "取消" }));

    await expect(promise).resolves.toBeUndefined();
    expect(fetchMock).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("凭据错误展示后端错误信息，可重试", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse({ code: 40101, message: "账号或密码错误" }, 401),
      )
      .mockResolvedValueOnce(
        jsonResponse({
          code: 0,
          message: "成功",
          data: { proof: "retry-proof", expires_in: 120 },
        }),
      );
    vi.stubGlobal("fetch", fetchMock);
    const { request } = renderHost();

    const promise = request("challenge-3");
    await screen.findByRole("dialog");
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("用户名"), "alice");
    await user.type(screen.getByLabelText("密码"), "wrong");
    await user.click(screen.getByRole("button", { name: "验证并继续" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "账号或密码错误",
    );

    await user.type(screen.getByLabelText("密码"), "correct");
    await user.click(screen.getByRole("button", { name: "验证并继续" }));

    await expect(promise).resolves.toBe("retry-proof");
  });
});
