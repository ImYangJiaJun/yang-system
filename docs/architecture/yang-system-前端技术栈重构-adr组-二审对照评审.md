# yang-system 前端技术栈重构 ADR 组（二审）对照评审

## 一、结论摘要

**总体判断：质量显著提升，有条件批准进入 M0/M1 落地验证。**

重写后的 ADR 组（`docs/architecture/frontend-rebuild/`）把上一轮评审指出的结构性缺口基本全部吸收：决策粒度拆分、路由收敛、契约三轨分离、session 状态机独立、验收指标量化、工作量重估、旧前端 CI 保留。决策推导诚实，退出成本如实标注。

**但有条件批准，条件如下（按优先级）：**

1. **ADR-4 的"后端已产出 OpenAPI"是未证实前提**，仓库中未找到 OpenAPI 生成器或文档端点（`Cargo.toml:24` 仅有 feature 声明，源码无 `utoipa`/`OpenApi` 实现）。openapi-typescript 轨必须降级为"待 spike 验证"，spike 失败则按 ADR-4 自身回退条款执行。
2. **前后端两套校验逻辑（前端 Ajv / 后端真实校验）之间没有自动化交叉验证机制**，需补契约测试定义权威方向。
3. **350KB 首屏 JS 预算对目标栈偏紧**，建议分两层（硬上限 450KB / 目标 350KB）并用打包分析器持续追踪。
4. **功能冻结期（6–8 周+延期风险）是显式产品决策**，建议在 README 决策索引中登记确认，而非默认接受。
5. **一次性 cutover 需补三件安全网**：灰度入口（内部账号先切）、切换后生产探针（五条核心链路即时验证）、明确"观测期达标才算切换成功"的标准。

---

## 二、上轮评审缺口逐项核验

| # | 上轮评审要求 | 二审回应 | 核验结论 |
|---|---|---|---|
| 1 | 拆分散决策（去 Quasar / 换 React 分开） | `adr-1`（构建+原语）、`adr-3`（框架+状态）独立成文；`adr-3-framework-and-state.md:13-28` 诚实承认"去 Quasar 不能推出必须去 Vue" | ✅ 已吸收 |
| 2 | OpenAPI / zod / 动态 JSON Schema 三轨分离 | `adr-4-contracts-and-validation.md:9-19` 明确三轨：openapi-typescript（静态类型）/ zod（固定协议）/ Ajv（动态 Schema） | ✅ 已吸收 |
| 3 | session 与 TanStack Query 边界 | `adr-3-framework-and-state.md:52-62` 三层分离，SessionController 独立、不承载 React API | ✅ 已吸收 |
| 4 | 量化视觉/性能/无障碍验收 | `adr-5-acceptance-and-cutover.md:19-56` 行数/行高/token 覆盖率/任务步数/WCAG 2.2 AA/性能预算/截图基线/三引擎 | ✅ 已吸收 |
| 5 | 旧前端并存期 CI 门禁 | `adr-5-acceptance-and-cutover.md:104` "旧前端 CI 门禁维持现状不动" | ✅ 已吸收 |
| 6 | 浏览器兼容边界 | `adr-1-build-and-ui-primitives.md:52-54` 新契约 Chrome/Edge ≥111、Firefox ≥128、Safari ≥16.4，显式放弃 Safari 14 | ✅ 已吸收（但有商业代价未显式说明，见 §四.4） |
| 7 | TanStack Router vs react-router 收敛 | `adr-3-framework-and-state.md:43-50` 选 react-router，理由充分 | ✅ 已吸收 |
| 8 | 工作量重估（3 周不可信） | `adr-5-acceptance-and-cutover.md:86-96` 能力清单驱动，M0–M4 合计 6–8 周含 20% 缓冲，"3 周"作废 | ✅ 已吸收 |

**上一轮评审中"渐进替换与可逆切换"建议，经需求方裁定不予采纳（`README.md:8-10`），本评审尊重该裁定，不再建议渐进路线。**

---

## 三、二审相对初版的实质改进

### 3.1 决策粒度与推导质量

初版把"去 Quasar / 换 React / 上 TanStack+shadcn"绑成单一结论；二审拆成 5 份独立 ADR，且每份都带：

- **驱动因素及权重表**（如 `adr-3-framework-and-state.md:18-24`）；
- **可验证事实**（如 `adr-1-build-and-ui-primitives.md:25-29` 的耦合证据）；
- **被否决路线及理由**（每份 §被否决路线）；
- **退出成本**（`adr-1:71` 中 / `adr-2:57` 低 / `adr-3:76` 高 / `adr-4:59` 低）；
- **何时重新评估**（每份 §何时重新评估）。

这是合格的 ADR 结构，比初版有本质提升。

### 3.2 框架决策的诚实性

`adr-3-framework-and-state.md:26-28` 明确承认："Vue 3 + 原生 Vite + TanStack Table Vue + headless 原语"路线在纯技术评分上是有效默认方案；不采纳的唯一原因是组织级方向已定，而非技术不成立。这是评审最看重的诚实——它把组织决策与技术评分分开，留出了未来复评的对照基准。

### 3.3 虚拟滚动的显式触发器

`adr-2-table-engine.md:34-42` 明确虚拟滚动不是一期硬依赖，给出两个引入触发器（单页行数 >100 或实测达不到性能预算），并列出引入时需一并解决的三项复杂度（动态行高、键盘导航、sticky 列交互）。这纠正了初版把虚拟滚动列为"一等需求"的过度承诺。

### 3.4 能力清单驱动的工作量估算

`adr-5-acceptance-and-cutover.md:62-82` 用 19 项能力清单（✳ 标记 8 项高失败代价）界定迁移范围，M0–M4 里程碑合计 6–8 周含 20% 缓冲，并规定 M1 完成时按实测重估 M2–M4（`adr-5:94`）。这比初版"3 周单人"可信得多。

---

## 四、外部视角的关键发现（二审仍未解决的问题）

以下是我以仓库现状交叉核验后，二审 ADR 组尚未解决的真实问题。

### 4.1 【高优先级】"后端已产出 OpenAPI"是未证实前提

ADR-4 决策表第一轨（`adr-4-contracts-and-validation.md:13`）写"后端已产出 OpenAPI"。我抽查了仓库：

- `Cargo.toml:24`：`yang-base` 依赖声明含 `"openapi"` feature，但**项目自身没有引入 utoipa 或任何 OpenAPI 生成器**；
- `src/app.rs`、`examples/frontend_demo/`、`src/addon/` 全量搜索 `utoipa|OpenApi|openapi`，**无任何命中**；
- 没有 `/docs`、`/api-docs`、`/swagger` 之类的文档端点。

**这意味着 openapi-typescript 轨目前没有任何输入源。** ADR-4 自己写了回退条款（`adr-4-contracts-and-validation.md:35-36`："覆盖不足时退回手写 zod 契约 + 契约测试现状方案，并记录差距"），但把它放在"实施 spike 时验证"，而不是决策表里的待验证项。

**建议：** 将 ADR-4 决策表第一轨状态改为"待 spike 验证"，并指定验证标准——后端能否产出覆盖全部 endpoint 的 OpenAPI 文档；不能则激活回退条款。同时把该前提写进 M1 垂直切片的第一个检查点。

### 4.2 【高优先级】前后端两套校验逻辑之间没有交叉验证机制

后端 Catalog 的 `input_schema` 是动态 JSON Schema，但它对前端是"声明式的 UI 提示"，**真实请求体校验在后端**（`src/addon/account/user/mod.rs` 等路由处理器）。前端 Ajv 白名单校验（`adr-4-contracts-and-validation.md:15-19`）与后端真实校验之间：

- 没有自动化机制验证"前端能渲染的字段 = 后端能校验的字段"；
- ADR-4 说"关键词集合对齐，由契约测试锁定双向一致"（`adr-4:30-31`），但**没有定义谁是权威、谁反向同步谁**。

**建议：** 在 ADR-4 补一条契约测试定义：以**后端校验规则为权威**，前端 Ajv 白名单从中导出；契约测试在后端 Catalog 测试用例集上跑前端校验器，断言"后端通过的请求前端必能构造，前端拒绝的请求后端必拒绝"。

### 4.3 【中优先级】350KB 首屏 JS 预算对目标栈偏紧

ADR-5 性能预算（`adr-5-acceptance-and-cutover.md:42`）：首屏 JS ≤ 350KB gzip。但目标栈的基线依赖：

- React 19 + react-dom：约 45KB gzip；
- TanStack Table + Query：约 40–60KB gzip；
- shadcn/ui 原语（拷入仓库的源码）：按组件数量每组件约 2–8KB；
- react-hook-form + Ajv（2020-12 完整 dialect）：约 30–40KB gzip；
- zod 4：约 15KB gzip；
- react-router + openapi-typescript 生成类型（类型无运行时，但业务代码增加）。

这些**还没算业务代码**（解释器、表格、表单、19 项能力清单）。"路由级代码分割"能缓解但不能消除——表格页本身就是首屏核心场景，分割收益有限。350KB 对纯 React 项目不算夸张，但对"元数据驱动解释器 + 全量表格 + 动态表单"这种重型应用是偏紧预算。

**建议：** 分两层预算——硬性上限 450KB（阻断 CI）+ 目标值 350KB（持续追踪）；引入打包分析器（`rollup-plugin-visualizer` 或 `vite-bundle-visualizer`）进 CI，每 PR 对比增量。

### 4.4 【中优先级】放弃 Safari 14 是商业决策，不应作为技术附赠品

`adr-1-build-and-ui-primitives.md:52-54` 把浏览器契约变更描述为"Tailwind 4 与 React 19 的现代特性基线要求收紧契约"。但真实情况是：

- 放弃 Safari 14 及以下 = 放弃仍在使用旧 Safari 的存量用户（通常是被企业 IT 锁定的 Mac/iOS 设备）；
- 对内部管理控制台，这个决策**大概率正确**（旧 Safari 用户占比小且多为非目标用户）；
- 但它是影响真实用户可达性的决策，应显式列出"放弃这些浏览器"的代价与理由，写进 `docs/CONFIGURATION.md` 和 README 的客户端要求章节（ADR-1 已说要写，`adr-1:56`，但没写"为什么"）。

**建议：** ADR-1 §5 补一段"该变更影响的用户面与接受理由"，由需求方确认。

### 4.5 【中优先级】功能冻结期是显式产品决策

`adr-5-acceptance-and-cutover.md:100-103`：旧前端冻结只接安全与关键修复；普通新功能等待切换后在 v2 实现。这意味着从拍板到上线的 6–8 周（含延期风险，并行期上限 10 周），**整个产品至少 2 个月不加功能**。

对一个仍在演化的系统，这是真实的业务代价。ADR-5 把它作为既定规则陈述，但没有在 README 决策索引里登记为"需求方显式确认的产品冻结决策"。

**建议：** 在 README 决策索引表登记一条：功能冻结窗口（预计 6–8 周，上限 10 周），标注需求方确认记录。

### 4.6 【低优先级】"AI 语料密度"优势的双刃性

ADR-3 承认 AI 协作效率仅作为"工程效率指标，不作为压倒迁移成本的第一性原理"（`adr-3-framework-and-state.md:23`），这是进步。但"React+TS 是 AI 语料密度最高的组合"这句仍隐含单向优势：

- React 19 于 2024 年底发布，2026 年 8 月写 ADR 时刚跨过"2 年成熟期"；其语料中混有大量关于 RSC、并发特性、新 hook 的噪音，**很多 React 语料回答的是不适用于本项目（无 SSR、纯 SPA 控制台）的问题**；
- Vue 3.5 语料少，但多是稳定模式的积累，对"解释器式控制台"这种场景的针对性可能并不差。

这不是否决理由（组织级方向已定），但建议在实施时用**经过审查的缺陷率**而非"生成速度"来评估 AI 协作效率（评审 §七阶段 2 的建议，ADR 未回应此点）。

---

## 五、Big Bang 一次性切换的完备性评估

按"需求方明确不做渐进式"的约束，评估如下：

### 5.1 已具备的合理部分

| 要素 | 证据 | 评价 |
|---|---|---|
| 旧制品归档回滚 | `adr-5-acceptance-and-cutover.md:114-116` | ✅ 回滚=重新部署旧制品，不依赖重建，这是底线 |
| 并行期上限 | `adr-5:103` 10 周，超限重估 | ✅ 防止双栈无限持续 |
| 旧前端 CI 保留 | `adr-5:104` 维持现状不动 | ✅ 比初版"移出门禁"正确 |
| 质量门禁逐项平移 | `adr-5:120-132` 含 pnpm audit、axe-core | ✅ 不降级 |
| 切换前提 | `adr-5:110` §2 指标全达标 + 能力清单逐项验收 + 双环境 E2E 全绿 | ✅ 明确 |

### 5.2 缺失的三件安全网（与"渐进式"无关，任何一次性切换都需要）

**1. 灰度入口。** `adr-5:108` 明确"不建设金丝雀"，但没有说"内部账号先切"。一次性切换 ≠ 全体用户同一分钟切换。建议：切换发布顺序 = 内部账号 → 小批量 → 全量，间隔按观测指标决定。这不属于金丝雀机制，只是发布顺序。

**2. 切换后生产探针。** `adr-5:117-118` 有"切换后 2 周观察生产指标（JS error 率、API 错误码分布、核心操作成功率）"，但缺**即时探针**——切换完成后立即验证五条核心链路：Catalog 拉取、登录、表格渲染、Action 提交、Step-up 流程。建议加一条"切换后 1 小时内执行生产链路探针清单"。

**3. 切换成功标准。** 观察期 2 周的"达标"没有定义。建议量化：JS error 率 < 基线、API 错误码分布无新增类别、核心操作成功率 ≥ 旧前端基线、无 P1 级回归——全部满足才清理旧制品归档。

### 5.3 一个需要确认的细节

`adr-5:111` "目录更名为 `frontend/`，旧目录归档删除（git 历史保留）"。git 历史保留是对的，但**旧目录的 CI 引用（`.github/workflows/ci.yml:47-55`）也需要在切换时同步更新**，`scripts/run_ci.py` 的 self-test（`scripts/run_ci.py:170-268`）断言前端命令指向 `frontend/`，切换后这些断言会失败——ADR-5 没有明确"CI 切换"这一项包含更新 self-test 断言。建议在 M4 清单（能力 18-19，`adr-5:81-82`）中显式加上。

---

## 六、对 ADR-4 的具体修订建议

ADR-4 是五份 ADR 中最需要修改的一份：

```markdown
## 1. 决策（修订建议）

| 轨道 | 对象 | 选型 | 状态 |
|---|---|---|---|
| 静态类型 | 固定 HTTP endpoint | openapi-typescript | **待 spike 验证**（前提：后端产出覆盖全部 endpoint 的 OpenAPI；失败则按 §回退条款执行） |
| 固定协议运行时校验 | Catalog envelope | zod 4 | 确认（沿用） |
| 动态表单校验 | 运行时 JSON Schema | Ajv 2020-12 | 确认（新增） |
```

**并补两条：**

1. **交叉验证契约**：以后端校验规则为权威，前端 Ajv 白名单从中导出；契约测试断言"后端通过的请求前端必能构造，前端拒绝的请求后端必拒绝"。
2. **spike 验收标准**：M1 第一个检查点验证后端 OpenAPI 产出覆盖度；覆盖不足即激活回退条款（手写 zod 契约 + 契约测试），并记录差距。

---

## 七、最终审核意见

| ADR | 意见 | 理由 |
|---|---|---|
| ADR-1（构建+UI 原语） | **批准** | 耦合证据充分，浏览器契约变更需补商业代价说明（§四.4） |
| ADR-2（表格引擎） | **批准** | headless 分层与解释器同构，虚拟滚动触发器明确 |
| ADR-3（框架+状态） | **批准** | 组织级方向已定，推导诚实，SessionController 边界正确 |
| ADR-4（契约+校验） | **有条件批准** | "后端已产出 OpenAPI"未证实，需降级为 spike 验证项；补交叉验证契约（§六） |
| ADR-5（验收+切换） | **有条件批准** | 补三件安全网（灰度入口/生产探针/成功标准）+ CI self-test 断言更新（§五） |

**状态建议：** 保持 Proposed（二审），补充上述修订后进入 M0/M1 落地验证。M1 垂直切片完成时，按实测重估 M2–M4（ADR-5 已有此机制，`adr-5:94`），届时再做终审。

---

## 八、CLAIM 核验行

1. `README.md:5` — 适用范围声明"破坏性，需求方明确授权 Big Bang，不做渐进绞杀与可逆切换"。
2. `README.md:8-10` — 初版经评审拆分为五个独立决策；渐进式建议经需求方裁定不予采纳。
3. `README.md:14` — 前端本质是"后端 Catalog/Registry 驱动的 UI 解释器"。
4. `README.md:38` — ADR-1 决策：原生 Vite + Tailwind 4 + shadcn/ui（源码自有）。
5. `README.md:40` — ADR-3 决策：React 19 + react-router；Query / SessionController / 本地状态三层分离。
6. `README.md:41` — ADR-4 决策：OpenAPI codegen + zod + Ajv 三轨分离。
7. `adr-1-build-and-ui-primitives.md:25-29` — 可验证事实：`quasar prepare`、24 个 `.vue` 广泛使用 q-* 组件、全局服务耦合证据。
8. `adr-1-build-and-ui-primitives.md:52-54` — 新浏览器契约 Chrome/Edge ≥111、Firefox ≥128、Safari ≥16.4，放弃 Safari 14 及以下。
9. `adr-2-table-engine.md:9-12` — TanStack Table v8 headless；虚拟滚动非一期硬依赖。
10. `adr-3-framework-and-state.md:20` — 框架决策决定性因素为"组织级技术方向：需求方明确长期 React 化并授权破坏性重构"。
11. `adr-3-framework-and-state.md:26-28` — 诚实承认 Vue 渐进路线"在纯技术评分上是有效默认方案"，不采纳唯一原因是组织方向已定。
12. `adr-4-contracts-and-validation.md:13` — "openapi-typescript（只生成类型，不生成客户端运行时）"——但仓库未发现 OpenAPI 生成器（`Cargo.toml:24` 仅 feature 声明，源码无 `utoipa`/`OpenApi` 命中），前提未证实。
13. `adr-4-contracts-and-validation.md:15-19` — Ajv 2020-12 处理动态 JSON Schema，react-hook-form 只管状态，校验经 Ajv 适配层。
14. `adr-5-acceptance-and-cutover.md:11-12` — 一次性 cutover，仅保留制品级回退底线。
15. `adr-5-acceptance-and-cutover.md:86-96` — 能力清单驱动，M0–M4 合计 6–8 周含 20% 缓冲；"3 周"作废。
16. `adr-5-acceptance-and-cutover.md:114-118` — 唯一回退底线：旧制品归档保留一个发布周期，切换后 2 周观察生产指标，无回归后清理。

（共 16 条，未超限。）
