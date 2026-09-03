import { Navigate, useOutletContext } from "react-router";

import { buildNavigationPages } from "@/app/navigation";
import type { ShellContext } from "@/layout/AppLayout";

/// “/” 重定向到第一个导航模块页（模块页路由见 app/routes.tsx）。
export default function HomeRedirect() {
  const { catalog } = useOutletContext<ShellContext>();
  const first = buildNavigationPages(catalog)[0];
  if (!first) {
    return (
      <div className="flex flex-col items-center gap-2 p-12 text-center">
        <h2 className="text-lg font-medium">暂无可用模块</h2>
        <p className="max-w-prose text-sm text-muted-foreground">
          当前账号没有可访问的业务模块。如需访问，请联系管理员分配权限后重新登录。
        </p>
      </div>
    );
  }
  return <Navigate to={`/m/${first.id}`} replace />;
}
