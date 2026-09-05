import { useQuery, type UseQueryResult } from "@tanstack/react-query";

import { CatalogCache } from "./catalog-cache";
import { fetchUiCatalog } from "../http/client";
import {
  useSessionCredentials,
  useSessionSnapshot,
} from "../session/use-session";
import type { UiCatalog } from "@/engine/contracts/ui-catalog";

/// 进程内不可变目录复用（revision 内容寻址），语义见 api/catalog-cache.ts。
const catalogCache = new CatalogCache();

export function useUiCatalog(): UseQueryResult<UiCatalog> {
  const session = useSessionCredentials();
  const snapshot = useSessionSnapshot();
  return useQuery({
    // Catalog 只在认证会话就绪后拉取；登录/切换会话使 token 变化 → 自动重拉。
    enabled: snapshot.loggedIn,
    queryKey: ["ui-catalog", session.token ?? "anonymous"],
    queryFn: async ({ signal }) => {
      const fetched = await fetchUiCatalog(session, signal, catalogCache.value);
      return catalogCache.accept(fetched);
    },
    staleTime: 30_000,
  });
}
