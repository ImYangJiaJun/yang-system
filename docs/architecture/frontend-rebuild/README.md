# 前端技术栈重构 ADR 组

状态：Proposed（二审）
日期：2026-08-27
适用范围：`frontend/` 控制台整体重构（破坏性，需求方明确授权 Big Bang，不做渐进绞杀与可逆切换）

本目录取代已删除的 `docs/architecture/frontend-stack-rebuild-adr.md`（单文件初版）。
初版经架构评审（`yang-system-前端技术栈重构-adr-架构评审.md`）后按评审意见拆分为
五个独立决策；评审中"渐进替换与可逆切换"相关建议经需求方裁定不予采纳，其余意见
全部吸收。

## 共同背景与系统本质

本前端不是普通 CRUD 管理台，而是**后端 Catalog/Registry 驱动的 UI 解释器**：

```text
后端 Catalog/Registry（唯一事实源）
  → UiCatalog 投影（纯 TS）
  → 通用解释器：TableView（表格）+ JsonSchemaForm（表单）+ Action 语义
  → 静态 custom registry 逃生舱
  → 手写页面：登录 / 注册 / 密码重置 / Step-up
```

## 不变量（所有 ADR 共同遵守）

1. 后端 Catalog 是唯一事实源；前端不引入第二套元数据格式（拒绝 amis 类低代码渲染器）。
2. 自定义视图只能通过构建期静态注册表解析，禁止根据后端字符串构造动态 import。
3. 保持 SPA + Nginx 静态部署形态；不引入 SSR/SSG（无 SEO 诉求，只增加复杂度）。
4. 现有质量门禁不降级：typecheck / lint / Vitest / locale 契约 / 构建与部署契约 /
   dev-server 与 production-build 双环境 Playwright。
5. 后端零改动：HTTP 语义、错误码体系、Refresh Cookie 会话、Step-up proof 契约不变。
6. 跨栈共享逻辑（投影、会话、校验、错误映射）沉淀为无框架 import 的纯 TS core。

## 决策索引

| ADR | 决策 | 结论 |
|---|---|---|
| [ADR-1](adr-1-build-and-ui-primitives.md) | 构建链与 UI 原语 | 解除 Quasar 耦合；原生 Vite + Tailwind 4 + shadcn/ui（源码自有） |
| [ADR-2](adr-2-table-engine.md) | 表格引擎 | TanStack Table v8 headless；虚拟滚动按需引入，非一期硬依赖 |
| [ADR-3](adr-3-framework-and-state.md) | 视图框架、路由与状态分层 | React 19 + react-router；Query / SessionController / 本地状态三层分离 |
| [ADR-4](adr-4-contracts-and-validation.md) | 契约与运行时验证边界 | OpenAPI codegen（静态类型）+ zod（固定协议 envelope）+ Ajv（动态 JSON Schema）三轨分离 |
| [ADR-5](adr-5-acceptance-and-cutover.md) | 验收指标与切换策略 | 量化视觉/性能/无障碍指标；能力清单驱动的工作量估算；一次性 cutover |

## 需求方显式确认项登记（二审新增）

以下为影响真实用户或产品节奏、必须由需求方显式确认的决策，不视为技术默认：

| 决策 | 内容 | 状态 |
|---|---|---|
| **功能冻结窗口** | 重构期（预计 6–8 周，并行期硬上限 10 周）内旧前端只接安全与关键修复（双写 v2），普通新功能等待切换后实现（ADR-5 §5） | ✅ 需求方已确认（2026-08-27） |
| **浏览器契约收紧** | 放弃 Safari ≤ 14 等旧浏览器，影响不可升级的存量设备可达性（ADR-1 §5） | ✅ 需求方已确认（2026-08-27） |
| **Big Bang 一次性切换** | 不建渐进绞杀/金丝雀/可逆切换机制，仅保留制品级回退底线 + 三件安全网（ADR-5 §6） | ✅ 需求方已确认（2026-08-27） |

## 二审状态

经 `yang-system-前端技术栈重构-adr组-二审对照评审.md` 评审：ADR-1/2/3 批准，
ADR-4/5 有条件批准；全部条件（OpenAPI spike 降级、前后端校验交叉验证契约、
双层 bundle 预算、冻结窗口登记、三件切换安全网、CI self-test 同步、浏览器变更
理由说明）已落实到对应 ADR。状态保持 **Proposed**，进入 M0/M1 落地验证，M1 完成
后按实测重估 M2–M4 并做终审。

## 进度记录

### 2026-08-27 M0 + M1 完成

- **M1 检查点 0（OpenAPI spike）：通过**。`DefinitionCatalog::to_openapi` 投影覆盖
  全部 18 个已注册 Action（路径/方法/operationId 逐一命中，写操作携带与调度同源的
  输入 Schema）；已固化为 `src/app.rs` 契约测试
  `openapi_projection_covers_all_catalog_actions`（CI 回归锁定）。导出机制落地为
  `src/bin/openapi-dump.rs`（惰性资源、不依赖真实 MySQL/Redis），ADR-4 §2.1 的
  剩余缺口关闭。
- **M0 基座**：`frontend-v2/`（React 19 + Vite 8 + Tailwind 4 + shadcn 源码组件 +
  TanStack Query/Table + zod 4 + Ajv 2020-12 + react-hook-form），typecheck/lint/
  test/build/format 全绿。
- **M1 纯 TS core**：旧前端 contracts/module-pages/api/session 全部平移（框架零
  耦合）；SessionController 独立状态机 + React 薄壳；Ajv 关键词白名单（白名单外
  显式报错）；`pnpm gen:contracts` 契约生成链（openapi-typescript 仅类型）与 18 个
  operation 全覆盖的交叉验证契约测试。Vitest 80 用例绿。
- **M1 垂直切片**：Catalog 拉取 → 导航投影 → 通用模块页（TanStack Table，查询参数
  契约与旧实现逐字段一致）→ Action 弹窗（react-hook-form + Ajv resolver）提交成功
  端到端；对接 `examples/frontend_demo` 真实联调通过（catalog 与 list Action 经
  Vite 代理 curl 验证）。集成测试断言请求方法/路径/body 精确匹配。Vitest 累计
  20 文件 119 用例绿，门禁由父级复核通过。

**M2–M4 重估（按 M1 实测）**：M1 实际耗时远低于估算（人机协作口径约 1 个工作日
vs 估算 2–2.5 周），核心解释器的两个最大风险项（表格查询契约对齐、动态表单校验）
已在切片中验证。M2（能力清单 5–12 完整化）重估为 3–5 个工作日；M3（页面与外壳）
2–3 天；M4（门禁/部署契约/CI 切换/文档）2–3 天。已知留待 M2 的事项：bulk Action
链路的真实后端覆盖（演示后端未声明 bulk presentation）、download/preview/redirect
的集成测试、无视图模块的 primaryAction 回退、enum/relation 控件从原生 select
升级为 Radix Select（需补 jsdom pointer-capture mock）、bundle 分包优化
（当前产物 812 kB，目标值 350 kB，硬上限 450 kB 需 code-splitting 后达标）。
