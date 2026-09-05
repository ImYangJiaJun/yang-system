export const SESSION_SIGNAL_STORAGE_KEY = "yang.session-signal";

const SESSION_CHANNEL = "yang.session.v1";
const SIGNAL_VERSION = 1;
const TAB_ID = signalId();

export type SessionEndReason = "credentials-changed" | "expired" | "logout";

interface SessionEndSignal {
  version: typeof SIGNAL_VERSION;
  id: string;
  sender: string;
  type: "session-ended";
  reason: SessionEndReason;
}

function signalId(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random()}`;
}

function sessionEndSignal(reason: SessionEndReason): SessionEndSignal {
  return {
    version: SIGNAL_VERSION,
    id: signalId(),
    sender: TAB_ID,
    type: "session-ended",
    reason,
  };
}

function parseSignal(value: unknown): SessionEndSignal | undefined {
  if (!value || typeof value !== "object") return undefined;
  const signal = value as Record<string, unknown>;
  if (
    signal.version !== SIGNAL_VERSION ||
    typeof signal.id !== "string" ||
    typeof signal.sender !== "string" ||
    signal.type !== "session-ended" ||
    (signal.reason !== "credentials-changed" &&
      signal.reason !== "expired" &&
      signal.reason !== "logout")
  ) {
    return undefined;
  }
  return signal as unknown as SessionEndSignal;
}

export function publishSessionEnd(reason: SessionEndReason) {
  if (typeof window === "undefined") return;
  const signal = sessionEndSignal(reason);
  if (typeof BroadcastChannel !== "undefined") {
    const channel = new BroadcastChannel(SESSION_CHANNEL);
    channel.postMessage(signal);
    channel.close();
  }
  try {
    localStorage.setItem(SESSION_SIGNAL_STORAGE_KEY, JSON.stringify(signal));
    localStorage.removeItem(SESSION_SIGNAL_STORAGE_KEY);
  } catch {
    // 隐私模式或存储策略可能禁用 localStorage；BroadcastChannel 仍可工作。
  }
}

export function subscribeSessionEnd(
  listener: (reason: SessionEndReason) => void,
): () => void {
  if (typeof window === "undefined") return () => undefined;
  const seen = new Set<string>();
  const accept = (candidate: unknown) => {
    const signal = parseSignal(candidate);
    if (!signal || signal.sender === TAB_ID || seen.has(signal.id)) return;
    seen.add(signal.id);
    listener(signal.reason);
  };
  const channel =
    typeof BroadcastChannel === "undefined"
      ? undefined
      : new BroadcastChannel(SESSION_CHANNEL);
  const onMessage = (event: MessageEvent<unknown>) => accept(event.data);
  const onStorage = (event: StorageEvent) => {
    if (event.key !== SESSION_SIGNAL_STORAGE_KEY || !event.newValue) return;
    try {
      accept(JSON.parse(event.newValue));
    } catch {
      // 非法或被篡改的跨标签页消息必须 fail-closed 为“忽略”。
    }
  };
  channel?.addEventListener("message", onMessage);
  window.addEventListener("storage", onStorage);
  return () => {
    channel?.removeEventListener("message", onMessage);
    channel?.close();
    window.removeEventListener("storage", onStorage);
  };
}
