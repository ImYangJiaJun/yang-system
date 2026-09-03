# ADR-2：表格引擎——TanStack Table headless

状态：Proposed（二审）
日期：2026-08-27
适用范围：TableView 解释器、查询面板、列偏好、选择模型

## 1. 决策

1. 表格状态引擎采用 **TanStack Table v8（headless）**：列定义、排序、筛选、分页、
   行选择收敛为无头状态机，皮肤层（shadcn 组件）可自由替换。
2. **虚拟滚动不是第一阶段硬依赖**：默认服务端分页（页大小 ≤ 100）直接 DOM 渲染；
   仅当实测触发 §4 的性能阈值时引入 `@tanstack/react-virtual`。

## 2. 可验证事实

- 现有表格领域逻辑已有良好拆分（评审确认）：`useTableQuery`、`useTableSelection`、
  `useTableActions`、`useRelationOptions`、`useColumnPreferences`、
  `table-view-model.ts`；真正绑定 Quasar 的只有 `QTableColumn` 类型、`q-table`
  插槽和 UI 原语。
- 现有实现为服务端分页，`q-table` 未启用虚拟滚动——即当前没有虚拟滚动的已证明
  需求（评审 §四.4，采纳）。
- `TableView.vue`/`TableDataGrid.vue`/`TableQueryPanel.vue` 本质是解释器，
  headless 分层（逻辑/渲染分离）与之同构；`q-table` 逻辑与渲染焊死是结构性失配。

## 3. 决策驱动因素

| 驱动因素 | 权重 | 说明 |
|---|---|---|
| 逻辑/渲染分离 | 高 | 解释器只写一遍状态逻辑，皮肤可换；与 Catalog 驱动模型同构 |
| 功能覆盖面 | 高 | 列配置、排序、筛选、选择、分组均为内置状态模型，不自研 |
| 无头 = 无供应商皮肤锁定 | 中 | 与 ADR-1 的源码自有原语天然组合 |
| 学习/概念成本 | 成本项 | column def 与状态模型有一层抽象，低于自研状态机的长期成本 |

## 4. 虚拟滚动的引入条件（显式触发器，不预设）

满足任一条件才引入 `@tanstack/react-virtual`：

- 单页行数上限上调至 > 100；或
- 实测 100 行 × 典型列数（含复杂单元格）渲染/滚动达不到 ADR-5 的性能预算。

引入时需一并解决：动态行高测量、键盘导航、sticky 列与虚拟化的交互——这些复杂度
不允许在一期默认承担。

## 5. 被否决路线

- **组件库内置表格**（AntD Table / Naive DataTable / q-table）：逻辑与渲染焊死，
  复杂单元格与 Action 语义依赖插槽黑盒。
- **ag-grid**：企业功能最全，但黑盒 + 商业 License 边界 + 主题深度定制成本，
  与本项目"源码自有"原则冲突。
- **自研表格状态机**：重复建设排序/筛选/选择模型，无差异化收益。

## 6. 迁移与退出成本

- 迁入：表格三个解释器组件重写（ADR-5 工作量核心项）；现有六个 composable/纯 TS
  模块的逻辑语义平移至 core。
- 迁出：状态机调用集中在 core 的 adapter 层，更换引擎只换 adapter，不触业务。
  退出成本：**低**。

## 7. 何时重新评估

- TanStack Table 停止维护；或出现其状态模型无法表达的表格需求（如跨页服务端
  选择的复杂语义），届时按 adapter 边界局部替换。
