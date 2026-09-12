import { useQuery } from "@tanstack/react-query";

import { useSessionCredentials } from "@/engine";
import { fetchAvatar } from "./api";
import { cn } from "@/shared/lib/utils";
import avatarDefaultUrl from "@/shared/assets/avatar-default.png";

/**
 * 用户头像：avatarVersion 为 null（无头像）时直接渲染默认图、不发请求；
 * 否则按 ["avatar", userId, avatarVersion] 拉取 data URL——key 含内容版本，
 * 版本变化自动重取（staleTime Infinity），加载失败/无头像回退默认图。
 */
export function UserAvatar({
  userId,
  avatarVersion,
  size = 24,
  alt = "用户头像",
  className,
}: {
  userId: number | undefined;
  avatarVersion: string | null;
  size?: number;
  alt?: string;
  className?: string;
}) {
  const session = useSessionCredentials();
  const enabled = userId !== undefined && avatarVersion !== null;
  const query = useQuery({
    enabled,
    queryKey: ["avatar", userId, avatarVersion],
    queryFn: ({ signal }) => {
      if (userId === undefined || avatarVersion === null) {
        return Promise.resolve({ etag: null, dataUrl: null });
      }
      return fetchAvatar(userId, session.token, signal);
    },
    staleTime: Number.POSITIVE_INFINITY,
  });
  const dataUrl = enabled ? (query.data?.dataUrl ?? null) : null;
  return (
    <img
      src={dataUrl ?? avatarDefaultUrl}
      alt={alt}
      width={size}
      height={size}
      className={cn("shrink-0 rounded-full object-cover", className)}
    />
  );
}
