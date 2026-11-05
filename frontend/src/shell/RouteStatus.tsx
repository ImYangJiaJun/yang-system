/**
 * 路由层的两个兜底界面：未匹配路径（404）与渲染期异常。
 *
 * 之前两处都缺失：任意页面渲染抛错会落到 react-router 的英文默认屏整树白屏；
 * 输错 URL 会得到同样一段英文 404，既没有站内样式，也没有返回入口。
 * 这里的文案与外观由 `shared/ui` 构件构成，并保留 `role="alert"` 供读屏播报。
 */

import { isRouteErrorResponse, Link, useRouteError } from "react-router";

import { Button } from "@/shared/ui/button";

export function RouteNotFound() {
  return (
    <StatusPanel
      title="页面不存在"
      message="这个地址没有对应的页面。可能是链接已失效，或该模块已被移除。"
    />
  );
}

export function RouteErrorElement() {
  const error = useRouteError();
  const detail = describeRouteError(error);
  return (
    <div role="alert">
      <StatusPanel
        title="页面出错了"
        message="这个页面没能正常渲染。可以先返回应用中心重试；如果反复出现，请把下面的信息提供给管理员。"
        detail={detail}
      />
    </div>
  );
}

function describeRouteError(error: unknown): string | undefined {
  if (isRouteErrorResponse(error)) {
    return `${error.status} ${error.statusText}${
      typeof error.data === "string" && error.data ? ` · ${error.data}` : ""
    }`;
  }
  if (error instanceof Error) return error.message;
  if (typeof error === "string" && error) return error;
  return undefined;
}

function StatusPanel({
  title,
  message,
  detail,
}: {
  title: string;
  message: string;
  detail?: string;
}) {
  return (
    <div className="flex min-h-[60svh] flex-col items-center justify-center gap-3 p-12 text-center">
      <h1 className="text-lg font-semibold">{title}</h1>
      <p className="max-w-md text-sm text-muted-foreground">{message}</p>
      {detail && (
        <p className="max-w-md font-mono text-xs break-all text-muted-foreground">
          {detail}
        </p>
      )}
      <div className="flex gap-2">
        <Button variant="outline" asChild>
          <Link to="/">返回应用中心</Link>
        </Button>
        <Button onClick={() => window.location.reload()}>重新加载</Button>
      </div>
    </div>
  );
}
