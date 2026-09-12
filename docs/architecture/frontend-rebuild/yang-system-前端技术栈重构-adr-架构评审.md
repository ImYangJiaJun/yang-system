# yang-system 前端技术栈重构 ADR 架构评审

## 一、结论摘要

**总体判断：方向合理，决策粒度和迁移策略需要重写。**

ADR 对系统本质的判断是准确的：前端不是普通 CRUD 管理台，而是“后端 Catalog/Registry 驱动的 UI 解释器”。因此，降低 UI 框架耦合、采用 headless 表格、保持静态 custom registry、继续 SPA 静态部署，都是正确方向。

但当前 ADR 将三个本应独立验证的决策绑定在一起：

1. 去除 Quasar；
2. 从 Vue 切换到 React；
3. 采用 shadcn/ui + TanStack 全家桶。

仓库现状可以充分支持第 1 项，也较强支持 TanStack Table；但**不能仅凭“生态更大、AI 语料更多、视觉上限更高”推出第 2 项**。这些因素可以作为工程效率指标，却不是足以压倒迁移成本与业务风险的第一性原理。

### 建议的决策

- **暂不按当前版本批准 React Big Bang 重写。**
- 将 ADR 拆为四个决策：
  1. 构建链是否脱离 Quasar CLI；
  2. UI 原语是否转向源码可控/headless；
  3. 视图框架是否从 Vue 切到 React；
  4. 契约生成、运行时校验和服务端状态分别采用什么方案。
- 默认首选路线：**Vue 3 + 原生 Vite + TanStack Table Vue + headless UI 原语，渐进移除 Quasar**。
- React 仍可作为候选，但应先通过一条真实垂直切片验证，且必须与 Vue 渐进路线在同一验收标准下对比。

---

## 二、ADR 做对了什么

### 1. 正确识别了前端的核心资产

ADR 第 20–28 行把系统抽象为：Catalog 投影、TableView、JsonSchemaForm、Action 语义和静态 custom registry。这与仓库实现一致。

`module-pages.ts` 基本是框架无关的纯 TS 投影；`custom/registry.ts` 也确实通过静态映射约束自定义视图加载。保留这些不变量是合理的。

### 2. 去 Quasar 的论据成立

当前构建链直接依赖 `quasar dev/build`，并有 `postinstall: quasar prepare`。UI 中大量使用 `q-table`、`q-dialog`、`q-btn`、`q-select`、`q-menu` 等组件，`quasar.config.ts` 还承载 boot、品牌色、通知和路由模式。

因此，ADR 对以下问题的描述基本成立：

- 构建链被 Quasar CLI 包装；
- UI 组件及全局服务形成框架私有耦合；
- 深度视觉定制会持续受到 Quasar 设计语言影响。

### 3. Headless 表格与 Catalog 解释器高度匹配

TanStack Table 的价值不是“React 生态更强”，而是它把列定义、排序、筛选、选择、分页等状态模型与 DOM 渲染分离。这确实符合后端元数据驱动的解释器模型。

当前代码也显示表格领域逻辑已有良好拆分：

- `useTableQuery`
- `useTableSelection`
- `useTableActions`
- `useRelationOptions`
- `useColumnPreferences`
- `table-view-model.ts`

真正被 Quasar 强绑定的主要是 `QTableColumn`、`q-table` 插槽和具体 UI 原语。因此，采用 headless table 有明确架构收益。

### 4. 拒绝 SSR 和通用 admin 元框架是合理的

这是登录后控制台，没有 SEO 或首屏服务端渲染需求。引入 Next.js/TanStack Start 会增加运行部署边界，收益不明显。

Refine/react-admin 的标准 resource/data-provider 模型也未必能自然表达本项目的 Action、Step-up、授权版本失效和自定义交互语义。除非只把它们当组件工具箱，否则容易形成重复抽象。

### 5. 保留既有测试与部署契约是正确底线

当前 `scripts/run_ci.py` 明确覆盖：

- typecheck、Vitest；
- 生产依赖审计；
- 完整前端 `check`；
- dev-server 与 production-build 两套 Playwright；
- Nginx 部署契约。

ADR 要求这些门禁不降级，是正确的。

---

## 三、从第一性原理看，当前推导哪里不够严密

## 1. “去 Quasar”不等于“必须去 Vue”

系统真正需要的是：

- 解释器逻辑与渲染解耦；
- UI 原语可组合、可替换；
- 构建链透明；
- 动态契约可验证；
- 可维护、可测试、可发布。

这些需求都可以在 Vue 3 中实现。TanStack Table 本身支持 Vue，headless UI 也存在 Vue 路线。当前大量领域逻辑已经由 composable 和纯 TS 模块承担，并不是所有逻辑都“焊死”在 Quasar 中。

所以现有证据证明的是：

> Quasar 不再适合作为核心 UI/构建框架。

但尚未证明：

> Vue 的响应式模型、组件模型或生态阻碍了系统目标。

React 是否更优，应由长期团队能力、跨项目统一、招聘、共享组件、预期扩展规模等组织级约束决定，而不能主要由“AI 语料密度”决定。

## 2. “组件源码自有”并不等于“没有框架私货”

shadcn/ui 的优势真实存在：源码可见、视觉可控、可以深入修改。但它仍依赖 React 组件模型、Radix 类原语、Tailwind 约定和项目本地 fork。

它消除的是“第三方黑盒组件约束”，不是所有技术耦合。代价包括：

- 上游 bug、安全修复和无障碍修复不会自动进入项目；
- 本地修改后升级 diff 会越来越昂贵；
- 不同组件可能逐渐产生不一致的交互与 token；
- 项目实际上要承担一个小型设计系统的维护责任。

因此，ADR 应把“源码所有权”描述为**耦合形式转换**，而不是消除耦合。

## 3. “视觉上限高”不是可验收的架构指标

“不好看”是有效痛点，但当前验收标准只写“达到评审认可”，不可重复、不可比较。

建议把视觉目标转换为可验证指标：

- 信息密度：典型 1440px 视口可见行数、列数；
- 表格任务效率：完成筛选、批量操作、列配置的步骤数；
- 响应式断点；
- WCAG 2.2 AA、键盘导航、焦点管理；
- 主题 token 覆盖率；
- 典型页面视觉回归截图；
- 100、1,000、10,000 行场景下的交互性能边界。

没有这些指标，“视觉上限”容易变成偏好，而非决策依据。

## 4. “代码量只有 7.8k 行”低估了语义密度

迁移风险并不与行数线性相关。当前前端包含：

- Refresh Cookie 与 access token 会话恢复；
- 多标签页 refresh 协调；
- Catalog revision/cache；
- 动态 Schema 表单；
- relation options；
- 树形表格安全降级；
- 行、批量、工具栏 Action；
- Step-up proof 的单次消费；
- 下载、预览、跳转和 multipart；
- locale、部署、双环境 E2E 契约。

这些代码量不大，但失败代价高。ADR 的“3 周单人”估算缺少按能力清单拆分、风险缓冲和验收返工预算，可信度偏低。

---

## 四、几个关键技术判断需要修正

## 1. OpenAPI 类型生成、Zod 与动态 JSON Schema 是三件不同的事

ADR 第 63、135–136、215–216 行把它们描述得过于接近。

实际应区分：

1. **OpenAPI codegen**：为固定 HTTP endpoint 生成静态 TypeScript 类型或客户端；
2. **Zod**：在运行时验证 Catalog envelope、版本和固定协议；
3. **JSON Schema**：描述运行时由后端下发的动态 Action 输入。

`react-hook-form + zod resolver` 更适合编译期已知的表单结构。对于运行时动态 JSON Schema，必须明确以下路线之一：

- JSON Schema → 独立验证器，例如 Ajv；
- JSON Schema → 动态转换为 Zod，但需承担转换完整性和语义差异；
- 后端同时输出专用 UI Schema 与验证规则，前端解释执行。

当前仓库中的 `JsonSchemaNode` 只实现了有限子集，例如 `$ref`、`anyOf/oneOf`、对象、长度和数值边界；不能因为引入 react-hook-form 就自动获得完整 Schema 校验。

**建议：动态表单验证优先使用 Ajv 2020-12，Zod继续用于固定 Catalog/响应 envelope。** 不要强行把两者统一成一个工具。

## 2. TanStack Query 不能简单“替代 Pinia 的 session 职能”

Catalog 和表格数据适合 Query 缓存；但 session 不只是服务端数据缓存，还包含：

- 内存 access token；
- Cookie 恢复状态机；
- 并发恢复去重；
- 多标签页协调；
- logout/reset 副作用；
- 401、AUTHZ_STALE、Step-up 后的失效传播。

这些是客户端状态机与命令协调，不应硬塞进 Query cache。

建议分层：

- TanStack Query：Catalog、表格数据、relation options 等可重取服务端状态；
- 独立 `SessionController`/external store：token、restore、refresh 协调和登出；
- React context 或极薄 Zustand adapter：只负责订阅状态，不承载协议逻辑。

## 3. TanStack Router 的收益可能不足以覆盖概念成本

导航来自 Catalog，真正的路由数量有限。类型安全路由主要解决静态路由参数，而本项目的主要复杂性在动态 module/view/action 解析。

因此，ADR 里保留“TanStack Router 或 react-router”说明这个选择还没有完成。更简单的 react-router 可能足够。除非能证明需要 file-based routing、typed search params 或 loader 生命周期，否则不应为了“TanStack 全家桶一致性”选 Router。

## 4. “虚拟滚动”应先证明需求

ADR 把虚拟滚动列为表格一等要求，但当前是服务端分页，`q-table` 也未显示使用虚拟滚动。若每页上限较低，虚拟化会增加固定行高、键盘导航、sticky column 和动态内容测量的复杂度。

建议先定义数据规模与性能预算；需要时再组合 `@tanstack/virtual`，不要让它成为第一阶段硬依赖。

## 5. 浏览器兼容边界必须显式迁移

现有 `quasar.config.ts` 指定了 `firefox115`、`chrome115`、`safari14` 等目标。切换到 React 19、Vite、Tailwind 4 后，需要明确：

- 是否仍支持 Safari 14；
- CSS 特性是否需要降级；
- 构建目标和 polyfill 策略；
- Playwright 是否覆盖声明的浏览器范围。

ADR 当前只说部署形态不变，没有说明浏览器契约是否不变。

---

## 五、迁移策略中最需要修改的部分

## 1. 旧前端仍在生产时，不应移出 CI

ADR 第 160–161 行提出双目录并存期间 CI 只门禁新目录。这与“旧前端冻结、仍承担生产回滚”冲突。

只要旧前端仍可发布或承担回滚，它至少必须保留：

- lockfile 安装验证；
- typecheck/build；
- 生产依赖 audit；
- 最小认证和核心路径 E2E smoke；
- 部署契约检查。

可以降低旧栈测试频率，但不能完全移出门禁。

## 2. “新需求一律进入 v2”会造成业务与迁移耦合

如果 v2 尚未生产，新需求只进 v2 会导致：

- 生产版长期缺功能；
- 切换压力持续增大；
- v2 变成无法独立稳定的移动目标。

更稳妥的规则是：

- 安全和关键业务修复双写；
- 普通功能根据切换窗口决定是否延后；
- 跨栈共享逻辑先抽到纯 TS core；
- 明确最长并行周期，避免双栈无限持续。

## 3. 切换与回滚标准不完整

“E2E 全绿后 Nginx 指向新产物”不足以覆盖真实发布。还需要：

- 新旧前端兼容同一后端 Catalog schema 版本区间；
- 可通过制品版本或配置快速回滚；
- 回滚不依赖重新构建；
- 生产观测：JS error、API error code、白屏率、核心操作成功率；
- canary 或内部用户先行；
- 回滚窗口和删除旧制品的条件。

即使不做长期双跑，也应做短期 canary 与可逆切换。

---

## 六、候选路线比较

以下评分基于当前仓库事实，5 分最好。

| 路线 | 架构匹配 | 迁移风险 | 视觉可控 | 生态/招聘 | 长期维护 | 综合判断 |
|---|---:|---:|---:|---:|---:|---|
| Vue 3 + 原生 Vite + TanStack Table Vue + headless UI | 5 | 5 | 4 | 4 | 5 | **当前最优默认方案** |
| React 19 + shadcn + TanStack，渐进迁移 | 5 | 3 | 5 | 5 | 4 | 有组织级 React 目标时合理 |
| React 19 + shadcn + TanStack，Big Bang | 5 | 2 | 5 | 5 | 3 | 当前证据不足，不建议直接批准 |
| Vue + Naive UI/Element Plus 整体替换 | 4 | 4 | 3 | 4 | 4 | 快速交付备选，但仍有组件库边界 |
| React + Ant Design/ProComponents | 4 | 3 | 3 | 5 | 4 | 若交付速度高于品牌差异化，可考虑 |
| Refine/react-admin/amis | 2 | 2 | 3 | 4 | 2 | 与自有 Catalog 解释器重叠，不建议 |

### 为什么首选 Vue 渐进路线

它保留当前已经稳定的：

- Vue 组件和 composable 心智模型；
- 页面与路由；
- 测试结构；
- 已抽离的纯 TS 逻辑；
- 生产运行经验。

同时可以逐步获得 ADR 真正追求的收益：

- 去除 Quasar CLI；
- 表格逻辑 headless；
- 组件源码可控；
- Tailwind/token 化主题；
- 更清晰的解释器边界。

如果完成这些后仍发现 Vue 在核心场景形成障碍，再迁 React，决策证据会更充分，而且已抽离的 core 能直接复用。

---

## 七、推荐的实施方案

## 阶段 0：先定义评价标准

在选框架前固定以下基准：

- 选一个最复杂的真实 TableView；
- 包含搜索、筛选、排序、分页、列偏好、relation、row/bulk/toolbar Action；
- 包含一个动态表单、multipart、Step-up；
- 固定桌面尺寸、浏览器目标、性能预算和无障碍要求；
- 固定视觉稿或视觉回归基线。

## 阶段 1：抽离框架无关内核

把以下能力变为无 Vue/React import 的纯 TS：

- Catalog 投影；
- table query reducer/state machine；
- action presentation 分组；
- selection model；
- relation options orchestration；
- session controller；
- API error mapping；
- JSON Schema 规范化和验证接口。

这一步无论最终选 Vue 还是 React都有收益。

## 阶段 2：做两个有限原型，而不是两个完整前端

用同一垂直切片分别验证：

- 原型 A：Vue + TanStack Table Vue + headless UI；
- 原型 B：React + TanStack Table + shadcn/ui。

比较：

- 完成功能所需代码量；
- 自定义复杂单元格和 Action 的难度；
- 测试可读性；
- 无障碍；
- bundle；
- 性能；
- 团队修改一个陌生功能所需时间；
- AI 生成代码经审查后的缺陷率，而不是只比较生成速度。

## 阶段 3：做正式框架决策

只有出现以下至少一项时，React 切换才具有充分理由：

- 团队或组织明确统一 React；
- 未来多个前端共享 React 组件/人才；
- Vue 原型无法满足核心 headless/设计系统能力；
- React 原型在量化指标上显著胜出；
- 愿意长期承担本地 shadcn 设计系统维护。

## 阶段 4：渐进替换与可逆切换

- 旧前端持续最小 CI；
- 新前端按垂直能力切片推进；
- 共享纯 TS core 避免双写协议逻辑；
- 新旧制品同时可部署；
- 先内部 canary，再扩大；
- 达到稳定窗口后删除旧前端。

---

## 八、建议重写 ADR 的结构

当前 ADR 仍有多个未决项，例如 TanStack Router/React Router、hey-api/openapi-typescript。这说明它更像“方案提案合集”，还不是一个可执行的最终 ADR。

建议拆为：

1. **ADR-1：解除 Quasar 构建与组件耦合**；
2. **ADR-2：表格引擎选择**；
3. **ADR-3：Vue 与 React 的框架决策**；
4. **ADR-4：OpenAPI codegen 与运行时验证边界**；
5. **ADR-5：前端重构发布、兼容和回滚策略**。

每份 ADR 都写清：

- 决策驱动因素及权重；
- 可验证事实；
- 假设；
- 被否决路线的可复现证据；
- 何时重新评估；
- 迁移和退出成本。

---

## 九、最终建议

### 如果目标是最低风险地解决当前痛点

采用：

> **Vue 3 + 原生 Vite + TanStack Table Vue + headless UI 原语 + Tailwind/token 设计系统**

渐进移除 Quasar。它能取得约 70%–85% 的目标收益，同时显著降低重写风险。

### 如果组织已经确定长期 React 化

采用：

> **React 19 + Vite + shadcn/ui + TanStack Table/Query，但使用渐进绞杀迁移，不做无回滚 Big Bang。**

同时：

- Router 优先选更简单的 react-router，除非 typed search/loader 有明确需求；
- 动态 JSON Schema 验证使用 Ajv，Zod负责固定协议；
- session 使用独立状态机，不把它等同于 Query cache；
- 旧前端在切换前保留最小生产门禁；
- 工作量按 5–8 周单人更审慎，待原型后重估。

### 对当前 ADR 的审核意见

**建议状态保持 Proposed，不直接进入 Accepted。**

批准以下方向：

- 去 Quasar；
- 原生 Vite；
- headless 表格；
- 保持 SPA、Catalog 唯一事实源、静态 custom registry 和现有质量门禁。

退回补证以下决策：

- Vue → React 的必要性；
- Big Bang 的风险收益；
- 动态 Schema 与 Zod/OpenAPI 的边界；
- session 状态模型；
- 双目录期间旧前端 CI；
- 可量化视觉、性能、无障碍和回滚标准。

一句话概括：**这份 ADR 找对了问题，也选出了一套优秀的 React 技术栈，但尚未证明“优秀的候选栈”就是“yang-system 当前约束下的最优迁移决策”。**