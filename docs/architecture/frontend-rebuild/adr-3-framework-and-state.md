# ADR-3：视图框架（Vue → React）、路由与状态分层

状态：Accepted
日期：2026-08-27
适用范围：视图框架、路由库、状态管理分层

## 1. 决策

1. 视图框架从 Vue 3 切换到 **React 19**（破坏性重构，不保留兼容层）。
2. 路由采用 **react-router**（非 TanStack Router）。
3. 状态管理三层分离（§4），TanStack Query **不**承载会话状态机。

## 2. 框架决策的诚实推导（吸收评审 §三.1）

评审正确指出："去 Quasar"的仓库证据**不能**推出"必须去 Vue"；Vue 的响应式与组件
模型并未被证明阻碍系统目标。本 ADR 不回避这一点，决策依据如下，按权重排列：

| 驱动因素 | 权重 | 说明 |
|---|---|---|
| **组织级技术方向** | 决定性 | 需求方（项目所有者）明确长期 React 化，并显式授权破坏性重构、放弃渐进路线的风险缓释。评审自身给出的 React 充分条件第一条即"团队或组织明确统一 React"（评审 §七阶段 3），该条件已满足 |
| headless 原语生态成熟度 | 高 | Radix/shadcn 是源码自有原语路线中维护体量最大、AI 协作资料最完整的一支；Vue 对应路线（Reka UI 等）明显更年轻，ADR-1 的原语决策在 React 侧落地风险更低 |
| 生态与人才密度 | 中 | 企业后台模板、组件、问题解决的供给 React 侧最大 |
| AI 协作效率 | 中 | React+TS 是 AI 编码代理语料密度最高的组合。**仅作为工程效率指标，不作为压倒迁移成本的第一性原理**（采纳评审定性）。同时承认其二刃性：React 语料混入大量 RSC/SSR/并发特性噪音，其中相当一部分不适用于本项目（无 SSR 的纯 SPA 控制台）。实施期间以**经人工审查后的缺陷率**而非生成速度评估 AI 协作效率，并作为复评输入（二审 §4.6） |
| 迁移风险 | 成本项 | Big Bang 重写，由 ADR-5 的能力清单、量化验收与工作量缓冲对冲 |

**承认的备选有效性**：评审给出的"Vue 3 + 原生 Vite + TanStack Table Vue +
headless 原语"渐进路线在纯技术评分上是有效默认方案；本决策不采纳它的唯一原因
是组织级方向已定，而非该路线技术上不成立。此结论如实记录，供未来复评对照。

### 2.1 假设

- 团队接受 React 19（hooks、并发特性）心智成本；
- 团队接受 ADR-1 所述本地 shadcn 设计系统的长期维护责任；
- 重构窗口内旧前端只接安全与关键修复（见 ADR-5 §5）。

### 2.2 何时重新评估

- 组织级前端方向变化；
- React 侧 shadcn/Radix 生态恶化到 ADR-1 §8 触发条件；
- 垂直切片（ADR-5 §4 里程碑 M1）暴露出核心解释器在 React 模型下无法达到验收
  指标——届时保留纯 TS core，框架决策回炉，且已抽离的 core 在 Vue 路线同样可用。

## 3. 路由：react-router（吸收评审 §四.3）

- 导航由 Catalog 驱动，真实静态路由数量有限；本项目复杂性在动态
  module/view/action 解析，不在静态路由参数。
- TanStack Router 的核心收益（file-based routing、typed search params、loader
  生命周期）在当前需求下无对应场景，**不为"全家桶一致性"引入概念成本**。
- 复评触发：出现真实的多视图深链分享（typed search）或 loader 数据预取需求时，
  再评估 TanStack Router，切换成本仅限路由声明层。

## 4. 状态管理三层分离（吸收评审 §四.2）

| 层 | 选型 | 承载 | 明确不承载 |
|---|---|---|---|
| 可重取服务端状态 | **TanStack Query v5** | Catalog 缓存与 revision、表格数据、relation options | 会话协议逻辑 |
| 会话状态机 | **独立 `SessionController`**（纯 TS，无框架 import） | 内存 access token、Cookie 恢复状态机、并发恢复去重、多标签页协调、logout/reset 副作用、401/AUTHZ_STALE/Step-up 后的失效传播 | 任何 React API |
| 框架订阅与本地 UI 状态 | React context + 极薄 store（Zustand 或内建状态） | 组件订阅 SessionController/主题/布局偏好 | 协议与命令逻辑 |

会话不是服务端数据缓存，不硬塞进 Query cache。现有 `api/session-coordination.ts`
（多标签页 refresh 协调）等纯 TS 逻辑平移进 `SessionController`，并按新边界补
单元测试。

## 5. 被否决路线

- **Vue 3 渐进路线**：见 §2，技术上有效，因组织级方向不采纳。
- **TanStack Router**：§3。
- **Pinia 式全量 store / Redux**：服务端状态归 Query、会话归 SessionController
  后，不存在需要重型全局 store 的剩余问题域。
- **Refine / react-admin**：其 resource/data-provider 抽象与本项目 Action、
  Step-up、授权版本失效语义错位，引入即绕过，形成重复抽象（评审共识，维持否决）。

## 6. 迁移与退出成本

- 迁入：全部 `.vue` 组件重写为 React；composable 逻辑拆分为纯 TS core + hooks
  薄壳。退出成本：**高**（框架切换本身），这正是本决策要求组织级依据的原因。
