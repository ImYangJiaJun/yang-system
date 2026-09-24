/**
 * 剪贴板复制的降级助手。
 *
 * # 这一份用例守的是哪次事故
 *
 * 2026-09-24：控制台部署在 `http://<公网IP>:18654`（明文 HTTP，走 `-EdgeBindAddr 0.0.0.0`）。
 * `navigator.clipboard` 是**安全上下文专属**的 API，在那个源上**根本不存在**——不是「拒绝」
 * 而是 `undefined`。四个复制点全都直接 `navigator.clipboard.writeText(...)`，于是抛
 * `TypeError: Cannot read properties of undefined (reading 'writeText')`，又被空 `catch` 吞掉，
 * 用户看到的就是「复制按钮点了没反应」。
 *
 * 所以这里钉住的核心行为是：**Clipboard API 不在（或被拒）时，必须自己降级，而不是失败**。
 * 降级走 `document.execCommand("copy")`——它不要求安全上下文，在出事的那个源上实测可用
 * （含「先 await 一次回显请求再复制」的时序，延迟 6.7 秒仍成功）。
 *
 * 第二组用例守的是降级路径的**副作用**：临时 textarea 不能留在 DOM 里，焦点必须还给
 * 触发元素——凭据清单的复制按钮和 Radix 弹窗共存，抢焦点会让焦点陷阱错乱。
 */

import { afterEach, describe, expect, it, vi } from "vitest";

import { copyText } from "@/shared/lib/clipboard";
import {
  restoreClipboard,
  stubClipboardApi as stubClipboard,
  stubExecCommand,
  stubExecCommandWith,
  stubInsecureClipboard as stubNoClipboard,
} from "@test/helpers/clipboard";

afterEach(() => {
  restoreClipboard();
  vi.restoreAllMocks();
});

describe("复制文本：优先 Clipboard API", () => {
  it("Clipboard API 可用时用它，不碰 execCommand", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    stubClipboard(writeText);
    const execCommand = stubExecCommand(true);

    await expect(copyText("fldeysrdna")).resolves.toBe("copied");
    expect(writeText).toHaveBeenCalledWith("fldeysrdna");
    expect(execCommand).not.toHaveBeenCalled();
  });

  it("Clipboard API 存在但被拒时降级，而不是直接失败", async () => {
    // 「存在但拒绝」是另一种真实形态：有权限策略或文档未聚焦时的 NotAllowedError。
    stubClipboard(
      vi.fn().mockRejectedValue(new Error("NotAllowedError: 用户未授予权限")),
    );
    const execCommand = stubExecCommand(true);

    await expect(copyText("fldeysrdna")).resolves.toBe("copied");
    expect(execCommand).toHaveBeenCalledWith("copy");
  });
});

describe("复制文本：非安全上下文的降级路径", () => {
  it("Clipboard API 不存在时降级到 execCommand 并报成功", async () => {
    // 这条就是线上那台服务器的真实形态：http://47.109.148.207:18654
    stubNoClipboard();
    const execCommand = stubExecCommand(true);

    await expect(copyText("fldeysrdna")).resolves.toBe("copied");
    expect(execCommand).toHaveBeenCalledWith("copy");
  });

  it("execCommand 也返回 false 时算失败，绝不谎报已复制", async () => {
    // 谎报「已复制」比不响应更糟：用户会去别处粘一个空的。
    stubNoClipboard();
    stubExecCommand(false);

    await expect(copyText("fldeysrdna")).resolves.toBe("unavailable");
  });

  it("execCommand 抛异常时算失败，不把异常漏给调用方", async () => {
    stubNoClipboard();
    stubExecCommandWith(() => {
      throw new Error("NotAllowedError");
    });

    await expect(copyText("fldeysrdna")).resolves.toBe("unavailable");
  });

  it("两条路都不可用（execCommand 压根不存在）时返回失败而不是抛", async () => {
    stubNoClipboard();

    await expect(copyText("fldeysrdna")).resolves.toBe("unavailable");
  });
});

describe("复制文本：降级路径不许留下副作用", () => {
  it("临时节点用完即删，DOM 里不留残留", async () => {
    stubNoClipboard();
    // 复制**进行中**那个带标记的节点必须在场，否则下面的「删掉了」可能只是因为它从没被建出来。
    let seenDuringCopy = false;
    stubExecCommandWith(() => {
      seenDuringCopy =
        document.querySelector("textarea[data-yang-copy-area]") !== null;
      return true;
    });
    const before = document.body.childElementCount;

    await copyText("fldeysrdna");

    expect(seenDuringCopy).toBe(true);
    expect(document.body.childElementCount).toBe(before);
    expect(document.querySelector("[data-yang-copy-area]")).toBeNull();
  });

  it("复制的是完整值：多行恢复码不被截断，且真的被选中了", async () => {
    // 复制按钮旁边那段文字是 CSS 截断显示的，剪贴板必须拿原值，
    // 否则多行恢复码会被悄悄切掉尾巴。
    //
    // ⚠️ 断言必须落在**选区**上，不能只断言 textarea 的 `value`：`copy` 命令复制的是
    // 选区，而 `value` 在 `select()`/`setSelectionRange()` 那一行被删掉之后依然是完整的
    // ——只看 value 的话，把「选中」整段删掉测试照样全绿，而那时复制出来的是空串
    // （2026-09-24 实测：只 focus 不 select 时 selectionStart === selectionEnd）。
    stubNoClipboard();
    const lines = "AAAA-BBBB\nCCCC-DDDD\nEEEE-FFFF";
    let selected = "";
    stubExecCommandWith(() => {
      const area = document.activeElement as HTMLTextAreaElement | null;
      selected =
        area === null
          ? ""
          : area.value.slice(area.selectionStart, area.selectionEnd);
      return true;
    });

    await expect(copyText(lines)).resolves.toBe("copied");
    expect(selected).toBe(lines);
  });

  it("按钮在对话框里时，临时节点挂进对话框而不是 body", async () => {
    // TOTP 设置弹窗是 Radix Dialog（自带焦点陷阱）：只允许焦点留在弹窗子树内。
    // 把临时节点挂到 body，focus 的那一刻就会被守卫抢回焦点、选区丢失，
    // 而 execCommand 仍返回 true —— 那就成了一次谎报成功的静默失败。
    stubNoClipboard();
    let parentTag = "";
    stubExecCommandWith(() => {
      const area = document.activeElement as HTMLElement | null;
      parentTag = area?.closest('[role="dialog"]') ? "dialog" : "body";
      return true;
    });

    const dialog = document.createElement("div");
    dialog.setAttribute("role", "dialog");
    const button = document.createElement("button");
    dialog.appendChild(button);
    document.body.appendChild(dialog);
    button.focus();

    await expect(copyText("fldeysrdna")).resolves.toBe("copied");

    expect(parentTag).toBe("dialog");
    expect(dialog.querySelector("textarea")).toBeNull();
    dialog.remove();
  });

  it("焦点还给触发它的那个按钮", async () => {
    // 复制按钮就长在凭据表格里，轮换确认框是 Radix Dialog（自带焦点陷阱）。
    // 临时 textarea 抢走焦点后不还，弹窗里的交互会错乱。
    stubNoClipboard();
    stubExecCommand(true);
    const button = document.createElement("button");
    document.body.appendChild(button);
    button.focus();
    expect(document.activeElement).toBe(button);

    await copyText("fldeysrdna");

    expect(document.activeElement).toBe(button);
    button.remove();
  });
});
