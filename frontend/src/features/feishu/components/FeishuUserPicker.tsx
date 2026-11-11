import { useEffect, useState } from "react";

import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";

import { searchFeishuUsers, type FeishuUser } from "../batch-approval-api";

/// 飞书通讯录选人器（转交 / 加签）。
///
/// 不能复用账号域的 `UserPicker`：那个选的是本系统账号的数字 id，而飞书写接口要的是
/// 飞书 `open_id`。这里只走 `/batch-approval/users/search`，且输入去抖 300ms。
export function FeishuUserPicker({
  label,
  accessToken,
  value,
  onChange,
  multiple = false,
  disabled = false,
  placeholder = "输入姓名搜索飞书通讯录",
}: {
  label: string;
  accessToken: string | undefined;
  value: FeishuUser[];
  onChange: (users: FeishuUser[]) => void;
  multiple?: boolean;
  disabled?: boolean;
  placeholder?: string;
}) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<FeishuUser[]>([]);
  const [searching, setSearching] = useState(false);
  const [errorMessage, setErrorMessage] = useState("");

  useEffect(() => {
    const trimmed = query.trim();
    if (!trimmed) {
      setResults([]);
      setSearching(false);
      setErrorMessage("");
      return;
    }
    const controller = new AbortController();
    setSearching(true);
    const timer = setTimeout(() => {
      void searchFeishuUsers(trimmed, accessToken, controller.signal)
        .then((users) => {
          if (!controller.signal.aborted) {
            setResults(users);
            setErrorMessage("");
          }
        })
        .catch((cause: unknown) => {
          if (!controller.signal.aborted) {
            setResults([]);
            setErrorMessage(
              cause instanceof Error ? cause.message : "飞书通讯录搜索失败",
            );
          }
        })
        .finally(() => {
          if (!controller.signal.aborted) setSearching(false);
        });
    }, 300);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [query, accessToken]);

  const selectedIds = new Set(value.map((user) => user.openId));

  const choose = (user: FeishuUser) => {
    if (multiple) {
      if (!selectedIds.has(user.openId)) onChange([...value, user]);
    } else {
      onChange([user]);
    }
    setQuery("");
    setResults([]);
  };

  const remove = (openId: string) => {
    onChange(value.filter((user) => user.openId !== openId));
  };

  return (
    <div className="space-y-2">
      <Label htmlFor="feishu-user-picker-input">{label}</Label>
      <Input
        id="feishu-user-picker-input"
        autoComplete="off"
        disabled={disabled}
        placeholder={placeholder}
        value={query}
        onChange={(event) => setQuery(event.target.value)}
      />
      {value.length > 0 && (
        <ul className="flex flex-wrap gap-2" aria-label={`已选${label}`}>
          {value.map((user) => (
            <li
              key={user.openId}
              className="flex items-center gap-1 rounded-md border border-border bg-muted/50 px-2 py-0.5 text-sm"
            >
              <span>{user.name}</span>
              {user.department && (
                <span className="text-xs text-muted-foreground">
                  {user.department}
                </span>
              )}
              <button
                type="button"
                className="text-muted-foreground hover:text-destructive"
                aria-label={`移除 ${user.name}`}
                disabled={disabled}
                onClick={() => remove(user.openId)}
              >
                ×
              </button>
            </li>
          ))}
        </ul>
      )}
      {query.trim() !== "" && (
        <div className="rounded-md border border-border bg-background">
          {searching ? (
            <p
              className="px-3 py-2 text-sm text-muted-foreground"
              role="status"
            >
              搜索中…
            </p>
          ) : errorMessage ? (
            <p role="alert" className="px-3 py-2 text-sm text-destructive">
              {errorMessage}
            </p>
          ) : results.length === 0 ? (
            <p className="px-3 py-2 text-sm text-muted-foreground">
              没有匹配的飞书用户
            </p>
          ) : (
            <ul role="listbox" aria-label={`${label}搜索结果`}>
              {results.map((user) => (
                <li key={user.openId}>
                  <button
                    type="button"
                    role="option"
                    aria-selected={selectedIds.has(user.openId)}
                    className="flex w-full items-center justify-between gap-3 px-3 py-2 text-left text-sm hover:bg-muted disabled:cursor-not-allowed disabled:opacity-50"
                    disabled={disabled || selectedIds.has(user.openId)}
                    onClick={() => choose(user)}
                  >
                    <span>{user.name}</span>
                    <span className="text-xs text-muted-foreground">
                      {selectedIds.has(user.openId)
                        ? "已选择"
                        : (user.department ?? "")}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
