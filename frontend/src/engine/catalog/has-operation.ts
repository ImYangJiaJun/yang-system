/**
 * 权限门控的通用判据：权限就是「目录里有没有这个 operation_id」（设计 §4.5）。
 *
 * 目录本身已按身份投影（服务端用 `policy.allows(context)` 过滤），所以不要解析 JWT，
 * 也不要用 `presentation.availability`——那是声明期静态提示，后端测试明写它不能
 * 替代服务端授权。
 *
 * 放在 engine/ 而不是某个业务域：它是「UI 目录 → 这个入口该不该渲染」的通用判据，
 * 任何域都用得上。此前它只活在业务域的 api 里，第二个域要用就只能抄一份——
 * 而抄出来的副本会各自漂移。
 *
 * 命中判据是**整串相等**，不是前缀/包含：写成前缀匹配的话，「目录里有
 * `access.groups.list_groups`」会被读成「这个身份有 `access.groups.list`」，
 * 于是一个不存在的权限位会把按钮渲染出来，最后以服务端 403 收场。
 */

import type { UiCatalog } from "@/engine/contracts/ui-catalog";

export function hasOperation(
  catalog: UiCatalog | undefined,
  operationId: string,
): boolean {
  return (
    catalog?.actions.some((action) => action.operation_id === operationId) ??
    false
  );
}
