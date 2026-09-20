import type { QueryClient } from "@tanstack/react-query";

/**
 * 会话建立/清空时的级联重置。
 *
 * `SessionController` 在 `beginSession`（登录）与 `clearSession`（登出/失效）时回调
 * `onSessionReset`，在 `acceptRefreshedTokenPair`（access token 轮换）时**不**回调。
 * 这正是「会话边界」而不是「token 变化」的语义，因此在这里清空缓存是安全的：
 * 不会因为每小时一次的令牌轮换而丢弃可用数据。
 *
 * 必须清空的是**查询缓存**而不只是身份 store：表数据查询键是
 * `["table-data", view_id, state]`，不含会话维度。同一标签页登出 A 后登录 B，
 * B 打开与 A 相同的视图（同一 view_id、默认分页/筛选 ⇒ 同一个键）时会命中 A 的缓存
 * 并先渲染 A 的整页行；若 B 取数失败（403/网络错误），React Query 保留上一次成功的
 * data，这些行会一直留在屏幕上，行级操作还会把 A 的行值当作表单初值 —— 相当于跨身份
 * 读取后端按角色投影裁剪过的字段与行。
 */
export function createSessionResetHandler(options: {
  clearIdentity: () => void;
  queryClient: QueryClient;
}): () => void {
  const { clearIdentity, queryClient } = options;
  return () => {
    clearIdentity();
    queryClient.clear();
  };
}
