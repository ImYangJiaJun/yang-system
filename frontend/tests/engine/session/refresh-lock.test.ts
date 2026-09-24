/**
 * 刷新令牌的跨标签页互斥。
 *
 * 守的是 2026-09-24 发现的两个问题：
 *
 * 1. `navigator.locks` 是安全上下文专属 API，`http://<公网IP>:<端口>` 上它是 `undefined`，
 *    原来的实现直接跳过加锁运行——**静默**丢掉互斥。
 * 2. 代价被低估了：服务端把重放的 refresh token 当泄露信号，`revoke_by_subject` 撤销该用户
 *    **全部**会话，且水位线判定含等号（`iat <= min_iat`），所以并发里落败方会把获胜方刚签发
 *    的新 Token 一起撤销（`crates/yang-base/src/token/manager.rs` 的 `rotate` 文档注释写明
 *    「客户端必须串行化刷新」）。所以「对手慢」绝不能被当成「对手死」。
 *
 * jsdom 里 `navigator.locks` 天然不存在，所以「不装桩」正好就是线上那个源的真实形态。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { withRefreshLock } from "@/engine/session/refresh-lock";

const LOCK_KEY = "yang.session.refresh-lock";
const LEASE_MS = 4_000;
const WAIT_LIMIT_MS = 15_000;

function leaseAt(owner: string, expiresAt: number) {
  return JSON.stringify({ owner, expiresAt });
}

function readLease(): { owner: string; expiresAt: number } | null {
  const raw = localStorage.getItem(LOCK_KEY);
  return raw === null ? null : JSON.parse(raw);
}

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
  localStorage.clear();
});

describe("withRefreshLock：安全上下文走 Web Locks", () => {
  it("有 navigator.locks 时用它，不碰 localStorage 租约", async () => {
    const request = vi.fn(
      async (
        _name: string,
        _options: LockOptions,
        callback: (lock: object) => Promise<unknown>,
      ) => callback({ name: "yang.session.refresh", mode: "exclusive" }),
    );
    vi.stubGlobal("navigator", { ...navigator, locks: { request } });

    await expect(withRefreshLock(async () => "已续期")).resolves.toBe("已续期");

    expect(request).toHaveBeenCalledOnce();
    expect(localStorage.getItem(LOCK_KEY)).toBeNull();
  });
});

describe("withRefreshLock：明文 HTTP（无 Web Locks）退到存储租约", () => {
  it("没有 navigator.locks 时任务照旧执行——不能因为拿不到锁就不续期", async () => {
    await expect(withRefreshLock(async () => "已续期")).resolves.toBe("已续期");
  });

  it("跑完把租约清掉，不把其他标签页锁在后面", async () => {
    await withRefreshLock(async () => "已续期");
    expect(localStorage.getItem(LOCK_KEY)).toBeNull();
  });

  it("任务抛错也要释放租约", async () => {
    await expect(
      withRefreshLock(async () => {
        throw new Error("续期失败");
      }),
    ).rejects.toThrow("续期失败");

    expect(localStorage.getItem(LOCK_KEY)).toBeNull();
  });

  it("干活期间心跳续租，让「还活着但慢」不会被当成「死了」", async () => {
    // 这条是本次修复的核心：没有心跳，一个耗时超过租期的刷新会被别人抢走，
    // 于是两次续期并发撞上 Token Rotation —— 而这个账号在所有设备上一起失效。
    vi.useFakeTimers();
    let release: (value: string) => void = () => undefined;
    const task = vi.fn(
      () =>
        new Promise<string>((resolve) => {
          release = resolve;
        }),
    );

    const pending = withRefreshLock(task);
    await vi.advanceTimersByTimeAsync(10);

    // 任务还在跑：跨过一整个租期，租约必须仍然有效且仍属于本标签页
    const first = readLease();
    expect(first).not.toBeNull();
    await vi.advanceTimersByTimeAsync(LEASE_MS + 500);
    const renewed = readLease();
    expect(renewed).not.toBeNull();
    expect(renewed?.expiresAt).toBeGreaterThan(first?.expiresAt ?? 0);
    expect(renewed?.expiresAt).toBeGreaterThan(Date.now());

    release("已续期");
    await expect(pending).resolves.toBe("已续期");
    expect(localStorage.getItem(LOCK_KEY)).toBeNull();
  });

  it("别的标签页持有有效租约时先等它，拿到之后才续期", async () => {
    vi.useFakeTimers();
    localStorage.setItem(LOCK_KEY, leaseAt("别的标签页", Date.now() + 1_000));
    const task = vi.fn(async () => "已续期");

    const pending = withRefreshLock(task);
    // 等待期间绝不能开始续期——那正是「撞上 Rotation、全账号被撤销」的那一刻
    await vi.advanceTimersByTimeAsync(500);
    expect(task).not.toHaveBeenCalled();

    // 对方释放之后才轮到我们
    localStorage.removeItem(LOCK_KEY);
    await vi.advanceTimersByTimeAsync(500);
    await expect(pending).resolves.toBe("已续期");
    expect(task).toHaveBeenCalledOnce();
  });

  it("租约已过期（对方崩了）就不必一直等，直接接手", async () => {
    localStorage.setItem(LOCK_KEY, leaseAt("已经崩掉的标签页", Date.now() - 1));

    const task = vi.fn(async () => "已续期");
    await expect(withRefreshLock(task)).resolves.toBe("已续期");
    expect(task).toHaveBeenCalledOnce();
  });

  it("对方的租约一直有效时一直等，直到安全网才自己上", async () => {
    // 安全网是给「既没释放、租约又一直在续」这种不可观测交错兜底的：
    // 继续等下去会变成「永远不续期 → 必然登出」，那比一次竞争更糟。
    vi.useFakeTimers();
    localStorage.setItem(
      LOCK_KEY,
      leaseAt("一直续租的标签页", Date.now() + 60_000),
    );
    const task = vi.fn(async () => "已续期");

    const pending = withRefreshLock(task);
    await vi.advanceTimersByTimeAsync(WAIT_LIMIT_MS - 1_000);
    expect(task).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(2_000);
    await expect(pending).resolves.toBe("已续期");
    expect(task).toHaveBeenCalledOnce();
  });
});

describe("withRefreshLock：存储不可用时不许空转、也不许卡住", () => {
  it("localStorage 直接抛（隐私模式）时立刻执行任务", async () => {
    const throwing = {
      getItem: () => {
        throw new Error("SecurityError");
      },
      setItem: () => {
        throw new Error("SecurityError");
      },
      removeItem: () => {
        throw new Error("SecurityError");
      },
    };
    vi.stubGlobal("localStorage", throwing);

    await expect(withRefreshLock(async () => "已续期")).resolves.toBe("已续期");
  });

  it("读得动但写不进去时也立刻执行，不空等满一个安全网", async () => {
    // 老版本 Safari 隐私模式就是这种形态：getItem 正常、setItem 抛 QuotaExceededError。
    // 那时若还去轮询等一个永远等不到的租约，每次 401 恢复都要白等十几秒。
    vi.useFakeTimers();
    const storage = {
      getItem: () => null,
      setItem: () => {
        throw new Error("QuotaExceededError");
      },
      removeItem: () => undefined,
    };
    vi.stubGlobal("localStorage", storage);
    const task = vi.fn(async () => "已续期");

    const pending = withRefreshLock(task);
    // 不推进任何时间也必须已经跑完——推时间就会暴露「偷偷等了一会儿」
    await expect(pending).resolves.toBe("已续期");
    expect(task).toHaveBeenCalledOnce();
    expect(vi.getTimerCount()).toBe(0);
  });
});
