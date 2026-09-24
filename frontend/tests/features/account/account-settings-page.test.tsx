import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearStoredSession } from "@/engine/session/auth-session";
import { createSessionController } from "@/engine/session/session-controller";
import {
  restoreClipboard,
  stubExecCommand,
  stubInsecureClipboard,
} from "@test/helpers/clipboard";
import { renderTestApp } from "@test/helpers/render-app";

/// 账号设置页：资料加载、修改密码/用户名表单、Step-up 428 重放、停用账号入口。

// jsdom 无 canvas 实现：二维码生成替换为固定 data URL。
// 命名导出与 default 导出同时给出，覆盖 vitest/rolldown 两种互操作形态。
vi.mock("qrcode", () => {
  const toDataURL = vi.fn(async () => "data:image/png;base64,qr-stub");
  return { toDataURL, default: { toDataURL } };
});

function jsonResponse(
  payload: unknown,
  status = 200,
  headers: Record<string, string> = {},
) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}

function mePayload() {
  return {
    code: 0,
    data: {
      id: 7,
      username: "alice",
      email: "alice@example.com",
      email_verified_at: 1000,
      status: "active",
      created_at: 500,
      updated_at: 600,
    },
  };
}

/// 默认 stub：me 成功；change-password/change-username 成功（relogin_required）。
function stubAccountApi(
  options: {
    changePassword428?: boolean;
    changeUsername428?: boolean;
    totpActivateFailsOnce?: boolean;
    totpActivated?: boolean;
  } = {},
) {
  const calls: Array<{ url: string; proof?: string }> = [];
  let activateAttempts = 0;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      const proof = (init?.headers as Record<string, string> | undefined)?.[
        "x-step-up-proof"
      ];
      calls.push({ url, proof });
      if (url.endsWith("/.well-known/yang/ui-catalog")) {
        return jsonResponse({
          code: 0,
          message: "成功",
          data: {
            schema_version: "2.3",
            revision: "a".repeat(64),
            actions: [],
            table_views: [],
            modules: [],
          },
        });
      }
      if (url.endsWith("/api/v1/users/me")) {
        return jsonResponse({
          code: 0,
          data: options.totpActivated
            ? { ...mePayload().data, totp_activated: true }
            : mePayload().data,
        });
      }
      if (url.endsWith("/api/v1/users/change-password")) {
        if (options.changePassword428 && !proof) {
          return jsonResponse(
            {
              code: 700010,
              message: "敏感操作需要重新认证",
              data: { challenge: "challenge-1", expires_in: 120 },
            },
            428,
          );
        }
        return jsonResponse({
          code: 0,
          data: { relogin_required: true, immediate_convergence: true },
        });
      }
      if (url.endsWith("/api/v1/users/change-username")) {
        if (options.changeUsername428 && !proof) {
          return jsonResponse(
            {
              code: 700010,
              message: "敏感操作需要重新认证",
              data: { challenge: "challenge-2", expires_in: 120 },
            },
            428,
          );
        }
        return jsonResponse({
          code: 0,
          data: {
            username_changed: true,
            immediate_convergence: true,
            relogin_required: true,
          },
        });
      }
      if (url.endsWith("/api/v1/users/change-email-verifications")) {
        return jsonResponse({
          code: 0,
          message: "成功",
          data: { accepted: true, expires_in: 600, resend_after: 60 },
        });
      }
      if (url.endsWith("/api/v1/users/change-email")) {
        return jsonResponse({
          code: 0,
          message: "成功",
          data: {
            email_changed: true,
            immediate_convergence: true,
            relogin_required: true,
          },
        });
      }
      if (url.endsWith("/api/v1/users/step-up/complete")) {
        return jsonResponse({
          code: 0,
          data: { proof: "proof-ok", expires_in: 300 },
        });
      }
      if (url.endsWith("/api/v1/users/mfa/totp/setup")) {
        return jsonResponse({
          code: 0,
          message: "请在认证器应用中扫描二维码或手动输入密钥",
          data: {
            secret: "JBSWY3DPEHPK3PXP",
            otpauth_uri:
              "otpauth://totp/yang-system:alice?secret=JBSWY3DPEHPK3PXP",
            activated: false,
            digits: 6,
          },
        });
      }
      if (url.endsWith("/api/v1/users/mfa/totp/activate")) {
        activateAttempts += 1;
        if (options.totpActivateFailsOnce && activateAttempts === 1) {
          return jsonResponse(
            { code: 700005, message: "参数无效 [code]: 一次性验证码无效" },
            400,
          );
        }
        return jsonResponse({
          code: 0,
          message: "TOTP 已启用，请妥善保存恢复码并重新登录",
          data: {
            totp_activated: true,
            recovery_codes: [
              "aaaa1111-bbbb2222-cccc3333-dddd4444",
              "eeee5555-ffff6666-00007777-11118888",
            ],
            immediate_convergence: true,
            relogin_required: true,
          },
        });
      }
      if (url.endsWith("/api/v1/users/mfa/totp/deactivate")) {
        return jsonResponse({
          code: 0,
          message: "双重验证已关闭，请重新登录",
          data: {
            totp_activated: false,
            immediate_convergence: true,
            relogin_required: true,
          },
        });
      }
      if (
        url.endsWith("/api/v1/users/logout") ||
        url.endsWith("/api/v1/users/disable")
      ) {
        return jsonResponse({
          code: 0,
          data: {
            account_disabled: true,
            immediate_convergence: true,
            relogin_required: true,
          },
        });
      }
      throw new Error(`测试未覆盖的请求：${url}`);
    }),
  );
  return calls;
}

afterEach(() => {
  restoreClipboard();
  vi.unstubAllGlobals();
  sessionStorage.clear();
  localStorage.clear();
  clearStoredSession();
});

describe("账号设置页", () => {
  it("加载并展示当前用户资料", async () => {
    stubAccountApi();
    renderTestApp({ path: "/account", authenticated: true });

    await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
    expect(screen.getByText("alice@example.com（已验证）")).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "账号设置" }),
    ).toBeInTheDocument();
  });

  it("修改密码成功清空会话并跳转登录页", async () => {
    stubAccountApi();
    const { controller } = renderTestApp({
      path: "/account",
      authenticated: true,
    });

    const user = userEvent.setup();
    await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
    await user.type(screen.getByLabelText("当前密码"), "old-pass");
    await user.type(
      screen.getByLabelText("新密码（至少 10 位）"),
      "new-password-1",
    );
    await user.type(screen.getByLabelText("确认新密码"), "new-password-1");
    await user.click(screen.getByRole("button", { name: "修改密码" }));

    await waitFor(() => {
      expect(controller.getSnapshot().loggedIn).toBe(false);
    });
  });

  it("修改密码 428 时经 requestStepUpProof 换 proof 后重放成功", async () => {
    stubAccountApi({ changePassword428: true });
    const requestStepUpProof = vi.fn(
      async () => "proof-ok" as string | undefined,
    );
    const controller = createSessionController({ requestStepUpProof });
    controller.beginSession({ accessToken: "test-access" });
    renderTestApp({ path: "/account", authenticated: true, controller });

    const user = userEvent.setup();
    await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
    await user.type(screen.getByLabelText("当前密码"), "old-pass");
    await user.type(
      screen.getByLabelText("新密码（至少 10 位）"),
      "new-password-1",
    );
    await user.type(screen.getByLabelText("确认新密码"), "new-password-1");
    await user.click(screen.getByRole("button", { name: "修改密码" }));

    await waitFor(() => {
      expect(requestStepUpProof).toHaveBeenCalledWith("challenge-1", {
        token: "test-access",
      });
    });
    // 重放后凭据变更 → 会话被清空。
    await waitFor(() => {
      expect(controller.getSnapshot().loggedIn).toBe(false);
    });
  });

  it("修改用户名提交成功并清空会话", async () => {
    stubAccountApi();
    renderTestApp({ path: "/account", authenticated: true });

    const user = userEvent.setup();
    await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
    const usernameInput = screen.getByLabelText("新用户名");
    await user.clear(usernameInput);
    await user.type(usernameInput, "alice2");
    await user.click(screen.getByRole("button", { name: "修改用户名" }));

    await waitFor(() => {
      expect(
        screen.queryByRole("button", { name: "修改用户名" }),
      ).not.toBeInTheDocument();
    });
  });

  it("停用账号入口经 SessionController.disableAccount 生效", async () => {
    stubAccountApi();
    const { controller } = renderTestApp({
      path: "/account",
      authenticated: true,
    });

    const user = userEvent.setup();
    await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
    vi.spyOn(window, "confirm").mockReturnValue(true);
    await user.click(screen.getByRole("button", { name: "停用账号" }));

    await waitFor(() => expect(controller.getSnapshot().loggedIn).toBe(false));
  });
});

it("TOTP：弹窗展示二维码/密钥 → 满 6 位自动激活 → 一次性回显恢复码 → 重新登录", async () => {
  const calls = stubAccountApi();
  const { controller } = renderTestApp({
    path: "/account",
    authenticated: true,
  });

  const user = userEvent.setup();
  await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
  await user.click(screen.getByRole("button", { name: "启用双重验证" }));

  // 弹窗内展示二维码（img，qrcode 已 mock 为固定 data URL）、密钥（带复制按钮）与验证码输入。
  const dialog = await screen.findByRole("dialog");
  expect(
    await within(dialog).findByRole("img", { name: "TOTP 二维码" }),
  ).toBeInTheDocument();
  expect(within(dialog).getByText("JBSWY3DPEHPK3PXP")).toBeInTheDocument();
  expect(
    within(dialog).getByRole("button", { name: "复制" }),
  ).toBeInTheDocument();

  // 满 6 位自动提交激活，无需点击按钮。
  await user.type(within(dialog).getByLabelText(/认证器验证码/), "123456");

  // 激活成功：弹窗关闭，恢复码在页面一次性回显。
  expect(
    await screen.findByText("aaaa1111-bbbb2222-cccc3333-dddd4444"),
  ).toBeInTheDocument();
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(calls.map((call) => call.url)).toEqual(
    expect.arrayContaining([
      expect.stringContaining("/api/v1/users/mfa/totp/setup"),
      expect.stringContaining("/api/v1/users/mfa/totp/activate"),
    ]),
  );

  await user.click(
    screen.getByRole("button", { name: "我已保存恢复码，重新登录" }),
  );
  await waitFor(() => expect(controller.getSnapshot().loggedIn).toBe(false));
});

it("TOTP：验证码错误时弹窗内提示并清空输入，可重试", async () => {
  stubAccountApi({ totpActivateFailsOnce: true });
  renderTestApp({ path: "/account", authenticated: true });

  const user = userEvent.setup();
  await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
  await user.click(screen.getByRole("button", { name: "启用双重验证" }));

  const dialog = await screen.findByRole("dialog");
  const codeInput = within(dialog).getByLabelText(/认证器验证码/);
  await user.type(codeInput, "000000");

  // 错误提示出现在弹窗内，输入框被清空，弹窗保持打开。
  expect(
    await within(dialog).findByText(/一次性验证码无效/),
  ).toBeInTheDocument();
  expect(codeInput).toHaveValue("");

  // 重试正确验证码 → 激活成功。
  await user.type(codeInput, "123456");
  expect(
    await screen.findByText("aaaa1111-bbbb2222-cccc3333-dddd4444"),
  ).toBeInTheDocument();
});

it("TOTP：已激活账号展示状态而非设置入口", async () => {
  stubAccountApi();
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      if (url.endsWith("/.well-known/yang/ui-catalog")) {
        return jsonResponse({
          code: 0,
          message: "成功",
          data: {
            schema_version: "2.3",
            revision: "a".repeat(64),
            actions: [],
            table_views: [],
            modules: [],
          },
        });
      }
      if (url.endsWith("/api/v1/users/me")) {
        return jsonResponse({
          code: 0,
          data: { ...mePayload().data, totp_activated: true },
        });
      }
      if (url.endsWith("/api/v1/users/sessions")) {
        return jsonResponse({
          code: 0,
          message: "成功",
          data: { sessions: [] },
        });
      }
      void init;
      throw new Error(`测试未覆盖的请求：${url}`);
    }),
  );
  renderTestApp({ path: "/account", authenticated: true });

  expect(await screen.findByText("已启用")).toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "启用双重验证" }),
  ).not.toBeInTheDocument();
  // 已激活账号提供自助关闭入口。
  expect(
    screen.getByRole("button", { name: "关闭双重验证" }),
  ).toBeInTheDocument();
});

it("TOTP：关闭双重验证 → 确认后经 Step-up 通道停用，会话清空回到登录页", async () => {
  const calls = stubAccountApi({ totpActivated: true });
  const { controller } = renderTestApp({
    path: "/account",
    authenticated: true,
  });

  const user = userEvent.setup();
  await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
  vi.spyOn(window, "confirm").mockReturnValue(true);
  await user.click(screen.getByRole("button", { name: "关闭双重验证" }));

  await waitFor(() => expect(controller.getSnapshot().loggedIn).toBe(false));
  expect(calls.map((call) => call.url)).toEqual(
    expect.arrayContaining([
      expect.stringContaining("/api/v1/users/mfa/totp/deactivate"),
    ]),
  );
});

it("TOTP：关闭双重验证在确认框取消时不发起请求", async () => {
  const calls = stubAccountApi({ totpActivated: true });
  const { controller } = renderTestApp({
    path: "/account",
    authenticated: true,
  });

  const user = userEvent.setup();
  await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
  vi.spyOn(window, "confirm").mockReturnValue(false);
  await user.click(screen.getByRole("button", { name: "关闭双重验证" }));

  expect(calls.some((call) => call.url.includes("/mfa/totp/deactivate"))).toBe(
    false,
  );
  expect(controller.getSnapshot().loggedIn).toBe(true);
});

it("更换邮箱：发送验证码 → 输入验证码 → 换绑成功清空会话", async () => {
  stubAccountApi();
  const { controller } = renderTestApp({
    path: "/account",
    authenticated: true,
  });

  const user = userEvent.setup();
  await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
  await user.type(screen.getByLabelText("新邮箱"), "bob@example.com");
  await user.click(screen.getByRole("button", { name: "发送验证码" }));

  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: /重发|s 后重发/ }),
    ).toBeInTheDocument(),
  );
  await user.type(screen.getByLabelText("验证码"), "123456");
  await user.click(screen.getByRole("button", { name: "更换邮箱" }));

  await waitFor(() => expect(controller.getSnapshot().loggedIn).toBe(false));
});

/// 走到「恢复码一次性回显」那一屏的公共前置：这些码只在这一次显示，抄丢没有第二次，
/// 所以它在明文 HTTP 上的复制行为值得单独钉住（2026-09-24 事故：复制按钮点了没反应）。
async function activateTotpAndShowRecoveryCodes() {
  const user = userEvent.setup();
  stubAccountApi();
  renderTestApp({ path: "/account", authenticated: true });
  await waitFor(() => expect(screen.getByText("alice")).toBeInTheDocument());
  await user.click(screen.getByRole("button", { name: "启用双重验证" }));
  const dialog = await screen.findByRole("dialog");
  await user.type(within(dialog).getByLabelText(/认证器验证码/), "123456");
  await screen.findByText("aaaa1111-bbbb2222-cccc3333-dddd4444");
  return user;
}

it("恢复码：复制彻底失败时就地报错，且不谎称「已复制」", async () => {
  // 反馈必须出现在按钮**旁边**。这条页面里的通用错误区在最底部（危险区之后），
  // 离这里的按钮隔了三节，写在那儿等于没反馈——而恢复码只在这一次显示。
  const user = await activateTotpAndShowRecoveryCodes();
  // 剪贴板桩必须在 userEvent.setup() **之后**：user-event 会自己装一个可用的
  // clipboard，先装就被它覆盖了（这就是本次第一版用例「复制成功」假象的来源）。
  stubInsecureClipboard();
  stubExecCommand(false);

  await user.click(screen.getByRole("button", { name: "复制恢复码" }));

  const notice = await screen.findByText(/没允许写剪贴板/);
  // 与「我已保存恢复码」那个按钮同处一个区块 = 就在按钮旁边，而不是页面底部
  const block = screen.getByRole("button", { name: "我已保存恢复码，重新登录" })
    .parentElement?.parentElement;
  expect(block?.contains(notice)).toBe(true);
  // 按钮不能同时说「已复制」——两句互相打脸的文案里，这一句更危险
  expect(
    screen.getByRole("button", { name: "复制恢复码" }),
  ).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "已复制" })).toBeNull();
});

it("恢复码：失败之后再成功，失败提示要跟着消失", async () => {
  // 两个标记必须互斥：否则一次失败会把后面每一次成功都染红。
  const user = await activateTotpAndShowRecoveryCodes();
  stubInsecureClipboard();
  const execCommand = stubExecCommand(false);

  await user.click(screen.getByRole("button", { name: "复制恢复码" }));
  expect(await screen.findByText(/没允许写剪贴板/)).toBeInTheDocument();

  // 降级路径这次能用了（例如用户换了个浏览器）
  execCommand.mockReturnValue(true);
  await user.click(screen.getByRole("button", { name: "复制恢复码" }));

  expect(screen.queryByText(/没允许写剪贴板/)).toBeNull();
  expect(screen.getByRole("button", { name: "已复制" })).toBeInTheDocument();
});
