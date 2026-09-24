/**
 * 路由级 lazy 页面加载中的兜底界面。
 *
 * # 为什么必须有它（2026-09-24）
 *
 * 只有**路由级 `lazy`** 的页面会触发这条路径：`createBrowserRouter` 在构造时发现
 * 初始匹配里含 `lazy` 路由，就把 `renderFallback` 置为 true，于是「首次导航完成前」
 * 需要一个兜底界面。没给的话 react-router 会打一条警告
 * （`No \`HydrateFallback\` element provided to render during initial hydration`），
 * 而且那段窗口里**页面是空白的**——不是理论问题：nginx 对 HTML 发 `no-store`，
 * 每次硬导航都要重新拉一次页面 chunk，所以远程访问者真的会看见这片空白。
 *
 * 兜底挂在**路由对象上**（`hydrateFallbackElement`）而不是 lazy 模块里：
 * 模块得先加载完才能渲染它的兜底，那就等于没有兜底。
 *
 * 文案与骨架刻意做成通用页面的形状（标题 + 几行），不假装是哪个具体页面：
 * 这是「正在加载」，不是「页面长这样」。
 */

import { Skeleton } from "@/shared/ui/skeleton";

export function RouteFallback() {
  return (
    <div
      // `role="status"` + `aria-live`：读屏用户在页面切换时要能听到「正在加载」，
      // 而不是面对一段静默。
      role="status"
      aria-live="polite"
      aria-label="页面加载中"
      className="space-y-6 p-6"
    >
      <div className="space-y-2">
        <Skeleton className="h-7 w-56" />
        <Skeleton className="h-4 w-80 max-w-full" />
      </div>
      <div className="space-y-3">
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-10 w-full" />
      </div>
      <p className="text-sm text-muted-foreground">正在加载页面…</p>
    </div>
  );
}
