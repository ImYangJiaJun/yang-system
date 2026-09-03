# ADR-4：契约与运行时验证边界

状态：Proposed（二审）
日期：2026-08-27
适用范围：OpenAPI 类型生成、zod、动态 JSON Schema 表单校验

## 1. 决策（三轨分离，吸收评审 §四.1；按二审 §4.1/§六 修订）

OpenAPI codegen、zod、动态 JSON Schema 是三件不同的事，**不强行统一为单一工具**：

| 轨道 | 对象 | 选型 | 职责 | 状态 |
|---|---|---|---|---|
| 静态类型 | 固定 HTTP endpoint 的请求/响应 | **openapi-typescript**（只生成类型，不生成客户端运行时） | 编译期类型安全，消除手写契约漂移 | **待 spike 验证**（见 §2.1） |
| 固定协议运行时校验 | Catalog envelope、revision、固定响应结构 | **zod 4**（沿用） | 运行时边界验证，防后端漂移与缓存损坏 | 确认 |
| 动态表单校验 | 后端运行时下发的 Action 输入 JSON Schema | **Ajv（2020-12 dialect）** | 运行时动态校验 | 确认（新增） |

### 2.1 openapi-typescript 轨的前提与 spike 验收标准（二审修订）

二审核验发现：当时仓库没有暴露 OpenAPI 文档的机制（无 `/docs`、`/api-docs` 类端点）。
2026-08-27 完成 spike（M1 检查点 0），结论：**通过**。

- spike 证据：`src/app.rs` 契约测试 `openapi_projection_covers_all_catalog_actions`——
  全量构建真实应用（account + access + demo 三 Addon），经
  `DefinitionCatalog::to_openapi` 投影 OpenAPI 3.1 文档，断言 **Catalog 全部 18 个
  已注册 Action 的路径、方法、operationId 在文档中逐一命中**，且写操作 requestBody
  携带与调度同源的输入 Schema。文档与运行时行为的一致性由"同一 Catalog 单一事实源"
  结构性保证，并由该测试锁定（CI 回归）。
- **剩余缺口（不阻塞本轨，列入实施任务）**：文档目前只能在进程内生成，需补一个
  导出机制（开发期 dump 子命令或受信管理端点），供 openapi-typescript 消费；
  在该机制落地前，固定协议轨道沿用"手写 zod 契约 + 契约测试"，两条轨并存不冲突。
- 若后续验证发现导出文档与运行时行为出现结构性偏差（不应发生，因同源投影），
  即激活回退条款并在本文档记录差距。

表单状态管理用 react-hook-form，但校验不经 zod resolver，而是经 **Ajv 适配层**
（自定义 resolver）：react-hook-form 只管状态与生命周期，校验规则来自后端下发的
动态 Schema。

## 2. 理由与事实

1. **react-hook-form + zod resolver 只覆盖编译期已知结构**。Action 输入 Schema 是
   运行时数据，zod resolver 无法直接消费；动态转 zod 需承担转换完整性与语义差异
   风险（评审指出的真实陷阱）。
2. **现有 `JsonSchemaNode` 只实现有限子集**（`$ref`、`anyOf/oneOf`、对象、长度与
   数值边界）。引入 Ajv 后：
   - 支持的关键词按 Ajv strict 模式白名单显式启用；
   - **不支持的关键词必须显式报错，禁止静默忽略**——校验缺口比报错更危险；
   - 后端 Schema 产出侧（`fields!`/`params!`）的关键词集合与前端白名单对齐，
     由契约测试锁定双向一致。
3. **openapi-typescript 而非 hey-api**：只取类型生成；HTTP 客户端保持自有
   （现有 `api/` 的拦截、错误码映射、会话协调逻辑已成熟，且与 SessionController
   深度协作），不引入 codegen 生成的客户端运行时。实施 spike 时验证后端 OpenAPI
   产出对全部 endpoint 的覆盖度；覆盖不足时退回"手写 zod 契约 + 契约测试"现状
   方案，并记录差距。
4. zod 继续用于 Catalog envelope 等**固定协议**的运行时验证：类型生成不替代
   运行时边界检查（后端版本错配、本地缓存损坏都只能靠运行时校验捕获）。

## 2.2 前后端校验的交叉验证契约（二审新增）

前端 Ajv 白名单校验与后端真实请求体校验是两套逻辑，必须定义权威方向并自动化
交叉验证，禁止双向漂移：

1. **后端校验规则是唯一权威**。前端 Ajv 白名单从后端 `fields!`/`params!` 实际产出
   的关键词集合导出，不允许前端单方面支持后端不认的关键词；
2. **契约测试断言双向一致**：在后端 Catalog 测试用例集上运行前端校验器，断言
   "后端通过的请求，前端必能构造并通过校验；前端拒绝的请求，后端必拒绝"；
3. 该契约测试纳入 CI 门禁（`run_ci.py`），任一侧变更打破断言即失败；
4. 出现不一致时，修复方向永远是"前端向后端对齐"，除非后端校验本身被判定为缺陷
   并走后端修复流程。

## 3. 与现有契约层的衔接

- 现有 `contracts/ui-catalog.ts`、`contracts/json-schema.ts`、`contracts/
  table-data.ts` 的 zod schema 与测试平移；其中能被 OpenAPI 生成类型覆盖的部分
  改为"生成类型 + zod 运行时校验"双层，以契约测试断言二者一致。
- `JsonSchemaForm` 解释器重写时，渲染分支表（field kind → 控件）保持与后端
  `fields!` 输出的一一对应，新增分支必须同步契约测试。

## 4. 被否决路线

- **JSON Schema 动态转 zod**：转换完整性风险，语义差异（如 `format`、联合类型
  求值顺序）难以对齐。
- **hey-api / orval 全量客户端生成**：自有 HTTP 客户端承载会话与错误码语义，
  生成客户端会制造第二套请求路径。
- **只靠编译期类型、去掉运行时校验**：后端与前端版本错配时静默损坏，不可接受。

## 5. 迁移与退出成本

- 迁入：Ajv 适配层 + resolver 为新写；白名单关键词集合与后端对齐需一次联调。
- 迁出：三轨各自独立，任一轨替换不影响其他两轨。退出成本：**低**。

## 6. 何时重新评估

- 后端 Schema 输出演进为专用 UI Schema + 验证规则分离格式（评审列举的路线三），
  届时校验轨道随之调整；
- Ajv 白名单与后端产出的对齐维护成本超过自研解释器时。
