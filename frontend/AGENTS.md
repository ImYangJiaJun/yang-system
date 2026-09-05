# frontend/AGENTS.md

前端目录结构规则。本文件是 `frontend/` 结构的**约束性记录**：任何改动不得破坏
下述分层与边界；确需演进结构时，必须在同一提交内同步本文件、根 `AGENTS.md`、
`docs/architecture/frontend-rebuild/README.md` 进度日志与相关门禁脚本。

设计依据：本前端是**后端 Catalog 的通用解释器**（Schema 驱动管理台），采用
「引擎/应用分离」结构，与 react-admin / Strapi / Directus 同构。完整决策记录见
`docs/architecture/frontend-rebuild/README.md` 2026-09-05 条目。

## 分层与依赖方向

```text
src/
├── engine/      # 通用解释引擎：业务无关，禁止出现任何业务名词
│   ├── renderers/   # table/form/action/module 解释器
│   ├── contracts/   # zod + Ajv 白名单校验 + OpenAPI 生成类型
│   ├── catalog/     # 后端 Catalog 的导航投影与缓存
│   ├── http/        # Action 调用协议与 HTTP 基础设施
│   ├── session/     # 浏览器会话协议（状态机/跨标签页协调/Step-up/生命周期）
│   └── index.ts     # 引擎公共出口
├── features/    # 业务域（当前仅 auth）与自定义视图
│   ├── auth/        # 登录/注册/重置/身份选择页面、流程请求 api.ts、StepUpDialog、身份 store
│   ├── <域>/views/  # 各域自定义视图（如 demo/views/DemoItemInsight.tsx）
│   ├── registry.ts          # 自定义视图静态注册表（唯一入口）
│   └── custom-view-boundary.tsx
├── shell/       # 应用外壳：routes/auth-gate/session-bridge/AppLayout/通用页面编排
├── shared/      # ui/（shadcn 源码组件）与 lib/（产品文案、工具函数）
├── main.tsx     # 唯一 createRoot 入口（架构门禁锁定）
└── index.css
```

依赖方向（只允许向下依赖）：

```text
shared  ←  engine  ←  features  ←  shell
```

## 强制规则

1. **engine/ 禁止 import features/ 与 shell/**。引擎不感知业务；会话凭据等
   横切能力由 `engine/session/` 的薄壳 hooks 提供（renderers 直接消费
   `useSessionCredentials` 是先例，勿反向注入业务实现）。
2. **features/ 引用引擎能力一律走 `@/engine` 公共出口**（`engine/index.ts`）。
   出口没有的能力，先在 `engine/index.ts` 显式导出，禁止长期积累深度路径
   import。（深度路径的机器禁令是记录在案的后续增强，落地前靠评审执行。）
3. **features/ 各域之间禁止互相 import**。两个域需要的共享逻辑下沉 `shared/`；
   拿不准就先复制，三次重复再下沉。
4. **自定义视图必须经 `features/registry.ts` 静态注册**：键是后端 Catalog 字符串，
   值必须是静态字符串字面量的 `lazy(() => import(...))`；禁止按后端字符串拼接
   import 路径（架构门禁机器强制）。未注册或加载失败自动回退通用 TableView。
5. **shell/ 只做组合不写业务**：路由、守卫、布局、页面编排；业务交互逻辑归
   features/，通用解释逻辑归 engine/。
6. **shared/ 禁止 import engine/features/shell**（它是依赖方向最底层）。
7. **新文件归位判断顺序**：与业务有关 → `features/<域>/`；是 Schema/契约的
   通用解释 → `engine/`；是路由/布局/外壳编排 → `shell/`；是纯 UI 构件或
   领域无关工具 → `shared/`。都拿不准时，先放离调用点最近的层并在 PR 说明。

## 测试与工具链约束

- `src/` 只承载生产代码；单元测试在 `tests/` 镜像 src 目录结构，经 `@/` 别名
  引用被测源码，`@test/` 别名指向 `tests/`（helper 在 `tests/helpers/`、fixture
  在 `tests/fixtures/`）。新增/移动 src 文件时同步 tests/ 镜像位置。
- 生成物：`src/engine/contracts/api-types.ts` 由 `pnpm gen:contracts` 产出，
  禁止手改；`contracts/openapi.json` 同理（仓库根 `contracts/` 目录）。
- shadcn 约定：`shared/ui/` 组件允许同时导出 variants 辅助函数（eslint 已豁免）；
  新增 shadcn 组件用 CLI，别名见 `components.json`。

## 机器门禁（改动结构时必须同步）

| 门禁                                          | 锁定的内容                                                                                                                                                                                      |
| --------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `scripts/check_architecture.py`               | `main.tsx` 唯一 createRoot（全 src 扫描）；`engine/renderers/table/TableView.tsx` ≤400 行 + 三个 hook 行为边界；`shell/routes.tsx` workbench DEV 门控；`features/registry.ts` 静态字面量 import |
| `frontend/scripts/verify-locale-contract.mjs` | `shared/lib/product-locale.ts` 唯一 locale 权威常量；src 与 tests 禁用隐式 locale API                                                                                                           |
| `frontend/scripts/verify-bundle-budget.mjs`   | 首屏 JS gzip 预算（目标 350 kB / 硬上限 450 kB）；自定义视图走 lazy chunk，不得进首屏                                                                                                           |
| `frontend/components.json`                    | shadcn 别名（ui → `@/shared/ui` 等）                                                                                                                                                            |

提交前门禁：`pnpm check`（format/lint/typecheck/Vitest/locale 契约/build/
bundle 预算/部署契约）+ 仓库根 `python scripts/check_architecture.py`。
