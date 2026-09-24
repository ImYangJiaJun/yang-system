/**
 * 把一段文本放进剪贴板，必要时降级。
 *
 * # 为什么需要这一层
 *
 * `navigator.clipboard` 是**安全上下文专属**的 API。控制台一旦跑在明文 HTTP 上
 * （`http://<公网IP>:<端口>`，即 `-EdgeBindAddr 0.0.0.0` 的测试部署），它在页面上
 * **根本不存在**——不是「拒绝」而是 `undefined`，所以 `navigator.clipboard.writeText(...)`
 * 抛的是 `TypeError`，而不是一个可以被 `catch` 读出原因的 `NotAllowedError`。
 * 2026-09-24 实测：`http://47.109.148.207:18654` 上四个复制点全部因此静默失效，
 * 用户看到的是「复制按钮点了没反应」。
 *
 * 降级走 `document.execCommand("copy")`：它不要求安全上下文，在出事的那个源上实测可用。
 * （同一个源上 `crypto.subtle` / `navigator.locks` / `crypto.randomUUID` 也一样缺席，
 * 所以这不是剪贴板一个 API 的偶发问题，见 `engine/session/auth-session.ts` 的刷新互斥降级。）
 *
 * # 为什么返回结果而不是抛
 *
 * 调用方必须能区分「复制成功」与「复制不了」——谎报「已复制」比不响应更糟
 * （用户会去别处粘一个空的）。所以这里把成败**显式**交回给调用方去说，
 * 由调用方决定是展示「已复制」还是给出可手动复制的退路。
 */

export type CopyOutcome = "copied" | "unavailable";

/// 降级用的临时 textarea 的键，抽出来便于测试与阅读时不猜。
const COPY_AREA_ATTRIBUTE = "data-yang-copy-area";

function clipboardApi(): Clipboard | undefined {
  if (typeof navigator === "undefined") return undefined;
  const clipboard = navigator.clipboard;
  // 只认「有 writeText」的形态：非安全上下文里整个对象缺席，
  // 而某些实现会给出一个没有 writeText 的空壳。
  return clipboard && typeof clipboard.writeText === "function"
    ? clipboard
    : undefined;
}

/// 记录当前焦点与选区，供降级路径用完还原。
function captureFocus(): {
  active: Element | null;
  selection: Selection | null;
  ranges: Range[];
} {
  const selection =
    typeof document.getSelection === "function"
      ? document.getSelection()
      : null;
  const ranges: Range[] = [];
  if (selection) {
    for (let index = 0; index < selection.rangeCount; index += 1) {
      ranges.push(selection.getRangeAt(index));
    }
  }
  return { active: document.activeElement, selection, ranges };
}

/// 还原焦点与选区。临时节点被移除后焦点会掉到 `body`，
/// 而这里的调用方长在表格与 Radix Dialog（自带焦点陷阱）里，抢走焦点不还会让弹窗错乱。
function restoreFocus(captured: ReturnType<typeof captureFocus>) {
  const { active, selection, ranges } = captured;
  if (selection) {
    selection.removeAllRanges();
    for (const range of ranges) selection.addRange(range);
  }
  if (active instanceof HTMLElement && document.contains(active)) {
    active.focus({ preventScroll: true });
  }
}

/// 临时节点挂到哪：优先挂进当前打开着的对话框，否则挂 `body`。
///
/// 按钮长在 Radix Dialog 里时（TOTP 设置弹窗）不能挂 `body`：对话框的焦点陷阱
/// 只允许焦点留在弹窗子树内，挂到外面的节点会在 `focus()` 的那一刻被守卫抢回焦点，
/// 选区随之丢失，`copy` 就什么都复制不到——而它**仍然返回 true**，于是变成一次
/// 谎报成功的静默失败。挂进对话框就没有这个问题。
function copyAreaParent(): HTMLElement | undefined {
  const active = document.activeElement;
  if (active instanceof HTMLElement) {
    const dialog = active.closest('[role="dialog"], [role="alertdialog"]');
    if (dialog instanceof HTMLElement) return dialog;
  }
  return typeof document.body === "undefined" || document.body === null
    ? undefined
    : document.body;
}

/// 降级路径：离屏 textarea + `execCommand("copy")`。返回是否真的复制成功。
function copyViaExecCommand(value: string): boolean {
  if (typeof document === "undefined") return false;
  if (typeof document.execCommand !== "function") return false;
  const parent = copyAreaParent();
  if (!parent) return false;

  const captured = captureFocus();
  const area = document.createElement("textarea");
  area.setAttribute(COPY_AREA_ATTRIBUTE, "");
  area.value = value;
  // readOnly 有两个作用：移动端不弹键盘（弹键盘会带着页面一起滚），
  // 以及避免 `select()` 在可编辑节点上触发输入法。
  area.setAttribute("readonly", "");
  // 刻意**不**加 `aria-hidden`：这个节点马上要被程序化聚焦，而「aria-hidden 的元素
  // 不许可聚焦」是一条无障碍违规（axe 的 aria-hidden-focus）。它不可见靠的是
  // 1px + 透明，`tabIndex=-1` 保证键盘也 Tab 不到它。
  area.tabIndex = -1;
  // 放在视口内但不可见：用 `position: fixed` + 1px + 透明，
  // 而不是 `left: -9999px`——后者在某些移动端浏览器上会带着滚动位置跳一下。
  area.style.position = "fixed";
  area.style.top = "0";
  area.style.left = "0";
  area.style.width = "1px";
  area.style.height = "1px";
  area.style.padding = "0";
  area.style.border = "none";
  area.style.outline = "none";
  area.style.boxShadow = "none";
  area.style.background = "transparent";
  area.style.opacity = "0";

  parent.appendChild(area);
  let copied = false;
  try {
    // 必须先聚焦：copy 命令复制的是**当前选区**，而选区得由一个已聚焦的
    // 可编辑节点持有；只 `select()` 不 `focus()` 时某些浏览器复制出的是空串。
    area.focus({ preventScroll: true });
    area.select();
    // iOS Safari 只有把选区设成整个文本才认这次 copy。
    area.setSelectionRange(0, area.value.length);
    copied = document.execCommand("copy");
  } catch {
    // 浏览器禁用了这条命令：按失败处理，由调用方给出可手动复制的退路。
    // （`copied` 初值就是 false，这里不需要再赋一次。）
  } finally {
    area.remove();
    restoreFocus(captured);
  }
  return copied;
}

/**
 * 复制 `value` 到剪贴板。
 *
 * 先试 Clipboard API（要求在安全上下文），被拒或被禁用时降级到 `execCommand`；
 * 两条路都不行就返回 `"unavailable"`，**绝不谎报成功**。
 */
export async function copyText(value: string): Promise<CopyOutcome> {
  const clipboard = clipboardApi();
  if (clipboard) {
    try {
      await clipboard.writeText(value);
      return "copied";
    } catch {
      // 权限被拒 / 文档未聚焦 / 策略不允许：继续走降级路径，而不是直接失败。
    }
  }
  return copyViaExecCommand(value) ? "copied" : "unavailable";
}
