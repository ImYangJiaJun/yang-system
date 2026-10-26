/**
 * Web Locks 提供跨标签页原子互斥；无此能力时使用带心跳的 localStorage 租约。
 * shortcut: localStorage 没有 CAS，冻结/存储不可用时仍可能竞争，服务端必须保留重放防御。
 * 等待超时只报告可重试错误，不能绕过仍有效的租约发起第二个刷新。
 */

const LOCK_STORAGE_KEY = "yang.session.refresh-lock";

/// 租约有效期：心跳间隔的两倍，所以「对方死了」最多让我们等这么久。
const LEASE_MS = 4_000;
/// 心跳：持有者每半个租期续一次，保证「还活着但慢」不会被当成「死了」。
const HEARTBEAT_MS = LEASE_MS / 2;
/// 等待方的轮询间隔。隐藏标签页会被钳到 ≥1s，所以这只是「最小」间隔。
const WAIT_STEP_MS = 100;
/// 独立调用的等待上限；刷新端到端预算会更早取消。
const WAIT_LIMIT_MS = 15_000;

interface Lease {
  owner: string;
  expiresAt: number;
}

/// 本标签页的标识。`crypto.randomUUID` 在非安全上下文里同样缺席，所以留一条退路
/// （与 `session-coordination.ts` 的 `signalId` 同思路；这里只需要「不同标签页不同」）。
const TAB_ID = (() => {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `${Math.floor(Math.random() * 1e9)}-${Date.now()}`;
})();

function leaseStorage(): Storage | undefined {
  try {
    // 访问 localStorage 本身就可能抛（部分隐私模式下 getter 直接抛 SecurityError）。
    return typeof localStorage === "undefined" ? undefined : localStorage;
  } catch {
    return undefined;
  }
}

function readLease(storage: Storage): Lease | undefined {
  try {
    const raw = storage.getItem(LOCK_STORAGE_KEY);
    if (!raw) return undefined;
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return undefined;
    const lease = parsed as Record<string, unknown>;
    if (
      typeof lease.owner !== "string" ||
      typeof lease.expiresAt !== "number"
    ) {
      return undefined;
    }
    return { owner: lease.owner, expiresAt: lease.expiresAt };
  } catch {
    // 被篡改或读不到：当作「没人在续期」。最坏结果是一次竞争，不是卡死。
    return undefined;
  }
}

/// 写自己的租约。返回是否真的写进去了——写不进去时调用方要**立刻**走无锁路径，
/// 而不是空转等一个永远不会出现的租约。
function writeLease(storage: Storage, owner: string): boolean {
  try {
    storage.setItem(
      LOCK_STORAGE_KEY,
      JSON.stringify({ owner, expiresAt: Date.now() + LEASE_MS }),
    );
    return true;
  } catch {
    return false;
  }
}

function holdsLease(storage: Storage, owner: string): boolean {
  return readLease(storage)?.owner === owner;
}

/// 抢锁的三种结果。
/// **必须先看再写**：直接写会把别人**仍然有效**的租约覆盖掉，那就等于没有互斥
/// ——对方正在续期，我们却以为自己抢到了，于是两次续期并发。
function acquire(storage: Storage): "acquired" | "busy" | "unavailable" {
  const holder = readLease(storage);
  // 自己已经持着（同一标签页的重入）也算抢到，不必覆盖。
  if (holder && holder.owner !== TAB_ID && holder.expiresAt > Date.now()) {
    return "busy";
  }
  if (!writeLease(storage, TAB_ID)) return "unavailable";
  // 读回确认：挡掉「两个标签页在同一瞬间都写了」的交错里的大多数。
  return holdsLease(storage, TAB_ID) ? "acquired" : "busy";
}

function releaseLease(storage: Storage, owner: string) {
  // 只清自己的租约：别人的租约被误删会让对方以为自己还持锁。
  // ⚠️ 读与删不是原子的（localStorage 没有 CAS）：如果本标签页的租约已经过期、
  // 而别人刚接手，我们可能读到自己的旧值、把对方的活租约删掉。心跳续租让
  // 「自己的租约过期」在活着时几乎不出现，这是本方案能给的极限。
  if (!holdsLease(storage, owner)) return;
  try {
    storage.removeItem(LOCK_STORAGE_KEY);
  } catch {
    // 删不掉就等它自然过期。
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

/// 持锁跑任务，期间心跳续租。
async function runHoldingLease<T>(
  storage: Storage,
  task: () => Promise<T>,
): Promise<T> {
  const heartbeat = setInterval(() => {
    if (holdsLease(storage, TAB_ID)) writeLease(storage, TAB_ID);
  }, HEARTBEAT_MS);
  try {
    return await task();
  } finally {
    clearInterval(heartbeat);
    releaseLease(storage, TAB_ID);
  }
}

/// 等前面那位让出（释放或租约过期），然后接手。
/// 对方仍持有时到期报错，不把等待超时转成无锁刷新。
async function runAfterPeer<T>(
  storage: Storage,
  task: () => Promise<T>,
  signal?: AbortSignal,
): Promise<T> {
  const deadline = Date.now() + WAIT_LIMIT_MS;
  for (;;) {
    signal?.throwIfAborted();
    const state = acquire(storage);
    if (state === "acquired") return runHoldingLease(storage, task);
    if (state === "unavailable") return task();
    if (Date.now() >= deadline)
      throw new DOMException("等待会话刷新超时，请重试", "TimeoutError");
    await sleep(WAIT_STEP_MS);
  }
}

/// 在 `localStorage` 租约保护下执行 `task`。
async function withStorageLease<T>(
  task: () => Promise<T>,
  signal?: AbortSignal,
): Promise<T> {
  signal?.throwIfAborted();
  const storage = leaseStorage();
  if (!storage) return task();
  const first = acquire(storage);
  if (first === "unavailable") return task();
  if (first === "acquired") return runHoldingLease(storage, task);
  return runAfterPeer(storage, task, signal);
}

/**
 * 在跨标签页互斥下执行 `task`（会话续期）。
 *
 * Web Locks 可用就用它（安全上下文）；否则退到 `localStorage` 租约；两者都不行就直接执行。
 */
export async function withRefreshLock<T>(
  task: () => Promise<T>,
  signal?: AbortSignal,
): Promise<T> {
  signal?.throwIfAborted();
  if (typeof navigator === "undefined" || !navigator.locks) {
    return withStorageLease(task, signal);
  }
  return navigator.locks.request(
    "yang.session.refresh",
    { mode: "exclusive", signal },
    () => {
      signal?.throwIfAborted();
      return task();
    },
  );
}
