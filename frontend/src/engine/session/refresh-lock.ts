/**
 * 刷新令牌的跨标签页互斥。
 *
 * # 为什么必须串行化（不是「最好这样」）
 *
 * 服务端把重放的 refresh token 当**泄露信号**：`SET NX` 落败即 `revoke_by_subject`，
 * 写 subject 水位线撤销该用户**全部**会话，而水位线判定含等号（`iat <= min_iat`），
 * 所以**并发里落败的一方会把获胜方刚签发的新 Token 对一并撤销**——两个标签页同时刷新，
 * 代价不是「其中一个被登出」，而是这个账号在**所有设备上**一起失效。
 * 服务端源码里把这条写成了客户端义务：
 *
 *   crates/yang-base/src/token/manager.rs（`rotate` 的文档注释）
 *   「…因此客户端必须串行化刷新（单飞/去重），不得对同一 Refresh Token 并发发起多次轮换。」
 *
 * 同标签页内的重复调用由 `auth-session.ts` 的 `activeRefresh` 合并；**跨标签页**就靠这里。
 *
 * # 为什么要有降级（2026-09-24 修）
 *
 * 首选 Web Locks（`navigator.locks`）：浏览器保证跨标签页的原子性。但它是**安全上下文
 * 专属**的 API——明文 HTTP 部署（`http://<公网IP>:<端口>`，即 `-EdgeBindAddr 0.0.0.0`
 * 的测试部署）上 `navigator.locks` 是 `undefined`，原来的实现直接 `return task()`，
 * **静默**丢掉了互斥；实测确认那个源上 `navigator.clipboard`、`crypto.subtle`、
 * `navigator.locks` 一起缺席。
 *
 * 所以退到 `localStorage` 上的**租约**（不能退到 `sessionStorage`：它是每标签页独立的，
 * 两个标签页互相看不见）。持有者带**心跳续租**，因此「对方还活着但慢」不会被误判成
 * 「对方死了」；只有租约真的过期（对方标签页崩了/被冻结）才轮到下一个。
 *
 * # 这个租约保证了什么、没保证什么
 *
 * 保证：一个**活着的**标签页在刷新期间始终持锁；其他标签页不会在它干活时插进去。
 *
 * 不保证（都是 `localStorage` 没有 CAS 与浏览器调度的硬限制，写在这里以免被高估）：
 *
 * 1. **抢锁的瞬间不是原子的**：两个标签页可以双双通过读回确认。「先写再读回」把窗口压到
 *    一次读写的间隔，但没有消除它。
 * 2. **后台标签页的定时器会被钳制**：隐藏标签页里 `setTimeout` 最小 ~1s（链接的定时器
 *    跑久了还会变成 1 分钟一次），所以等待方的轮询精度是「秒级」，不是 `WAIT_STEP_MS`。
 * 3. **被冻结的标签页**心跳会停：那时它的租约到期，我们可能接着它做**同一次**
 *    刷新——但它的请求要么已经完成（cookie 已换新，我们拿新 cookie 再刷是合法的），
 *    要么压根没发出去；真正危险的是「请求在飞、标签页被冻住」，此时只能靠
 *    `WAIT_LIMIT_MS` 这个安全网兜住（15 秒），而它本身也不是万无一失。
 *
 * 拿不到存储（隐私模式禁用 `localStorage`）或写不进去时**立刻不带锁地执行**：
 * 宁可偶发一次竞争，也不能因为拿不到锁就拒绝续期——那会变成必然登出。
 */

const LOCK_STORAGE_KEY = "yang.session.refresh-lock";

/// 租约有效期：心跳间隔的两倍，所以「对方死了」最多让我们等这么久。
const LEASE_MS = 4_000;
/// 心跳：持有者每半个租期续一次，保证「还活着但慢」不会被当成「死了」。
const HEARTBEAT_MS = LEASE_MS / 2;
/// 等待方的轮询间隔。隐藏标签页会被钳到 ≥1s，所以这只是「最小」间隔。
const WAIT_STEP_MS = 100;
/// 等待上限——**安全网**，不是常规路径：正常情况是等对方释放或租约过期。
/// 取一个远大于任何合理刷新耗时的值，好让「请求在飞 + 标签页被冻结」这种
/// 难以观测的交错几乎不可能落在窗口里。
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
    writeLease(storage, TAB_ID);
  }, HEARTBEAT_MS);
  try {
    return await task();
  } finally {
    clearInterval(heartbeat);
    releaseLease(storage, TAB_ID);
  }
}

/// 等前面那位让出（释放或租约过期），然后接手。
/// 走到 `WAIT_LIMIT_MS` 安全网就直接跑——那是「对方既没释放、租约又一直在续」的
/// 不可观测交错，继续等下去的风险（永远不续期 → 必然登出）比一次竞争更大。
async function runAfterPeer<T>(
  storage: Storage,
  task: () => Promise<T>,
): Promise<T> {
  const deadline = Date.now() + WAIT_LIMIT_MS;
  for (;;) {
    const state = acquire(storage);
    if (state === "acquired") return runHoldingLease(storage, task);
    if (state === "unavailable") return task();
    if (Date.now() >= deadline) return task();
    await sleep(WAIT_STEP_MS);
  }
}

/// 在 `localStorage` 租约保护下执行 `task`。
async function withStorageLease<T>(task: () => Promise<T>): Promise<T> {
  const storage = leaseStorage();
  if (!storage) return task();
  const first = acquire(storage);
  if (first === "unavailable") return task();
  if (first === "acquired") return runHoldingLease(storage, task);
  return runAfterPeer(storage, task);
}

/**
 * 在跨标签页互斥下执行 `task`（会话续期）。
 *
 * Web Locks 可用就用它（安全上下文）；否则退到 `localStorage` 租约；两者都不行就直接执行。
 */
export async function withRefreshLock<T>(task: () => Promise<T>): Promise<T> {
  if (typeof navigator === "undefined" || !navigator.locks) {
    return withStorageLease(task);
  }
  return navigator.locks.request(
    "yang.session.refresh",
    { mode: "exclusive" },
    () => task(),
  );
}
