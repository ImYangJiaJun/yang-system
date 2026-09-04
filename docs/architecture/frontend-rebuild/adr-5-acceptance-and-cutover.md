# ADR-5：量化验收指标、迁移范围与一次性切换策略

状态：Accepted
日期：2026-08-27
适用范围：frontend-v2 验收、工作量估算、CI 安排、上线切换

## 1. 决策

1. 以**量化指标**替代"达到评审认可"式验收（吸收评审 §三.3）。
2. 以**能力清单**而非代码行数界定迁移范围并重估工作量（吸收评审 §三.4）。
3. 切换采用**一次性 cutover**：需求方明确不采纳渐进绞杀与可逆切换机制；仅保留
   制品级回退底线（§6）。
4. 现有质量门禁逐项平移，不接受降级（§7）。

## 2. 量化验收指标

### 2.1 视觉与信息密度

- 1440×900 视口、默认密度下，模块表格页首屏可见数据行 ≥ 18 行；
- 提供紧凑/默认/宽松三档密度，行高分别 ≤ 36px / 44px / 52px；
- 主题 token 覆盖率 100%：组件层禁止硬编码色值/间距字面值（lint 规则强制）；
- 暗色模式无对比度违规（axe-core 断言）。

### 2.2 任务效率（以现有最复杂模块页为基准场景）

- 完成一次"筛选 → 排序 → 分页"组合操作 ≤ 4 次点击/按键；
- 列显示配置 ≤ 2 步打开并可键盘完成；
- 行 Action / 批量 Action / 工具栏 Action 三类入口位置符合后端
  `action_presentations` 投影，不允许降级为统一溢出菜单。

### 2.3 无障碍

- WCAG 2.2 AA：`@axe-core/playwright` 在核心页面零 critical / serious 违规
  （现有 devDependencies 已有 axe，门禁平移）；
- 键盘导航：表格行操作、对话框、Step-up 流程、查询面板全部可脱离鼠标完成；
- 焦点管理：对话框开关、Action 完成后焦点回到触发元素（E2E 断言）。

### 2.4 性能预算

| 场景 | 预算 |
|---|---|
| 首屏 JS（gzip） | **硬上限 ≤ 450 KB（阻断 CI）；目标值 ≤ 350 KB（持续追踪）**，路由级代码分割；引入打包分析器（`vite-bundle-visualizer` 或 `rollup-plugin-visualizer`）进 CI，每 PR 对比增量（二审修订：单层 350KB 对"解释器 + 全量表格 + 动态表单"重型应用偏紧） |
| 表格 100 行 × 典型列 | 输入/滚动交互无掉帧（INP ≤ 200ms） |
| 1,000 行 | 触发 ADR-2 §4 虚拟滚动评估阈值；未引入虚拟化前不得允许该页大小上线 |
| 10,000 行 | 明确不支持前端全量加载，必须服务端分页 |
| Catalog 冷加载到可交互 | ≤ 2s（本地依赖环境基线） |

### 2.5 视觉回归

- 核心页面（登录、工作台、最复杂模块表格页、Action 对话框、Step-up）建立
  Playwright 截图基线，纳入 CI；
- 基线场景固定：桌面 1440×900、明暗双主题、默认密度。

### 2.6 浏览器

按 ADR-1 §5 契约：Chromium / Firefox / WebKit 三引擎 Playwright 全绿。

## 3. 迁移范围：能力清单（验收单位）

不以行数估算，逐项验收以下能力（✳ = 高失败代价，评审 §三.4 清单扩展）：

| # | 能力 | 备注 |
|---|---|---|
| 1 | Refresh Cookie 会话恢复状态机 ✳ | SessionController（ADR-3 §4） |
| 2 | 多标签页 refresh 协调 ✳ | 平移 `session-coordination.ts` 语义 |
| 3 | Catalog 拉取、revision 与缓存失效 | Query + zod envelope 校验 |
| 4 | 导航投影（identity/module/view 排序） | 平移 `module-pages.ts` |
| 5 | TableView 解释器：排序/筛选/分页/列偏好 | TanStack Table（ADR-2） |
| 6 | 复杂单元格与 relation options ✳ | 含异步选项加载与缓存 |
| 7 | 树形表格安全降级 ✳ | 后端标记不可用时回退平铺 |
| 8 | 行/批量/工具栏 Action 与确认对话框 | action_presentations 投影 |
| 9 | JsonSchemaForm 动态表单 ✳ | Ajv 校验（ADR-4） |
| 10 | Step-up proof 单次消费流程 ✳ | 与后端 Redis 原子消费语义对齐 |
| 11 | 401 / AUTHZ_STALE / Step-up 失效传播 ✳ | refresh 后同一请求最多自动重试一次 |
| 12 | 下载 / 预览 / 跳转 / multipart | 现有 Action 语义全量 |
| 13 | 认证页：登录/注册/密码重置 | 手写页面 |
| 14 | 工作台 / Dashboard / Business 页 | 手写页面 |
| 15 | custom 静态注册表 | 现仅 `DemoItemInsight` 一项 |
| 16 | locale 契约（单语言产品文案） | 校验脚本适配新入口 |
| 17 | 主题（明暗/密度）与持久化 | token 化 |
| 18 | 双环境 E2E（dev-server / production-build） | 复用 `examples/frontend_demo/` |
| 19 | 部署契约与生产构建校验 | 适配 Vite 产物布局 |

## 4. 里程碑与工作量重估（吸收评审：5–8 周单人，含风险缓冲）

| 里程碑 | 内容 | 估算 |
|---|---|---|
| M0 基线 | 固定 §2 指标、基准场景、浏览器矩阵、截图基线脚手架 | 2–3 天 |
| M1 垂直切片 | **检查点 0：OpenAPI spike**（ADR-4 §2.1，先于一切生成类型依赖）；纯 TS core 抽离（投影/会话/错误映射/Schema 规范化）+ Catalog → 导航 → 最复杂模块表格渲染 → 一个 Action 弹窗提交成功 | 2–2.5 周 |
| M2 解释器完整 | 能力清单 5–12 全部 | 2–2.5 周 |
| M3 页面与外壳 | 能力清单 13–17 | 1–1.5 周 |
| M4 门禁与切换 | 能力清单 18–19、CI 切换（**含 `scripts/run_ci.py` self-test 断言与 `.github/workflows/ci.yml` 前端引用的同步更新**）、文档同步、旧目录删除 | 3–5 天 |

合计 **6–8 周**（单人全情口径），含约 20% 验收返工缓冲；M1 完成时按实测重估
M2–M4。初版 ADR 的"3 周"估算作废——其低估了会话/Step-up/失效传播等高语义密度
能力的返工风险。

## 5. 并行期规则（明确非渐进）

- 新代码落在 `frontend-v2/`，旧 `frontend/` **冻结**：只接受安全修复与关键业务
  修复，且此类修复双写到 v2（按评审 §五.2 的第一条）；普通新功能等待切换后
  在 v2 实现。
- **并行期上限 10 周**：超过则停下来重估范围，禁止双栈无限持续。
- 旧前端 CI 门禁维持现状不动（零成本），切换完成前不拆除。

## 6. 一次性切换（Big Bang）

采纳需求方裁定：不建设代码级双栈、特性开关、金丝雀机制或绞杀式渐进迁移。切换流程：

1. 切换前提：§2 全部指标达标 + 能力清单逐项验收 + 双环境 E2E 全绿；
2. `scripts/run_ci.py` 前端命令指向 `frontend-v2/`（同步更新 self-test 断言与
   `.github/workflows/ci.yml` 的前端引用），目录更名为 `frontend/`，旧目录归档
   删除（git 历史保留）；
3. Nginx 指向新产物，部署契约校验通过即发布；
4. **回退底线（仅此一项）**：切换前最后一个旧前端制品在制品库归档，回滚 =
   重新部署旧制品，不依赖重新构建；除制品归档外不建设任何可逆切换机制。

### 6.1 三件安全网（二审新增；属发布顺序与观测，不是渐进机制）

1. **分批发布顺序**：内部账号 → 小批量用户 → 全量。每批间隔由 §6.2 探针与观测
   指标决定，不设定固定时长；任何一批不达标即暂停并回退到上一批范围。这是发布
   顺序，不是金丝雀基础设施。
2. **切换后 1 小时内执行生产链路探针**（人工或脚本，清单固定）：Catalog 拉取、
   登录、表格渲染、Action 提交、Step-up 流程，五条链路逐一验证并留记录。
3. **观测期成功标准（量化）**：切换后 2 周观察期内，全部满足才算切换成功——
   - JS error 率不高于旧前端基线；
   - API 错误码分布无新增类别；
   - 核心操作（登录/表格查询/Action 提交/Step-up）成功率 ≥ 旧前端基线；
   - 无 P1 级回归。
   全部达标后才清理旧制品归档；任一不达标，执行第 4 步回退并重估。

## 7. 质量门禁平移（逐项对照，不降级）

| 现有门禁 | frontend-v2 对应 |
|---|---|
| `vue-tsc --noEmit` | `tsc --noEmit`（strict） |
| ESLint `--max-warnings 0` + eslint-plugin-vue | ESLint + typescript-eslint + react 规则集，阈值不变 |
| Prettier format:check | 不变 |
| Vitest 同目录测试 | 不变；React 组件层配 Testing Library |
| `verify:locale-contract` | 保留脚本思路，适配新文案入口 |
| `verify:production-build` / `verify:deployment-contract` | 保留，适配 Vite 产物布局 |
| `pnpm audit --prod --audit-level moderate` | 不变 |
| Playwright 双环境（5310/18310、5311/18311 端口隔离） | 不变 |
| axe-core 无障碍断言 | 从现有依赖平移，纳入核心页面 e2e |

## 8. 文档同步清单（切换时）

- `AGENTS.md`：前端章节整体重写（技术栈、目录约定、命令、测试策略）；
- `README.md`：本地环境与启动命令；
- `docs/CONFIGURATION.md` 与 README：浏览器契约变更（ADR-1 §5）；
- 本 ADR 组状态翻转为 Accepted。
