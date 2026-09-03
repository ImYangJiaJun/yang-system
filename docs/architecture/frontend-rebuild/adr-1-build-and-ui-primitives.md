# ADR-1：解除 Quasar 耦合——构建链与 UI 原语

状态：Proposed（二审）
日期：2026-08-27
适用范围：前端构建链、组件层、设计系统、浏览器契约

## 1. 决策

1. 构建链脱离 `quasar dev/build` CLI，使用**原生 Vite**。
2. UI 原语采用 **Tailwind CSS 4 + shadcn/ui**：组件源码拷入 `src/components/ui/`，
   归仓库所有，不是 npm 黑盒依赖。
3. 浏览器契约显式变更（见 §5）。

## 2. 决策驱动因素及权重

| 驱动因素 | 权重 | 说明 |
|---|---|---|
| 构建链透明性 | 高 | 构建、预览、产物布局完全由仓库内标准 Vite 配置表达，无 CLI 封装层 |
| 组件可定制性 | 高 | 解释器式架构要求任意深度定制组件，不允许"组件库不支持"死胡同 |
| 视觉可控性 | 中 | 主题 token、密度、暗色模式源码可控，脱离固定 Material 观感 |
| 维护责任 | 成本项 | 源码自有 = 承担一个小型设计系统的维护责任（见 §4） |

## 3. 可验证事实（现状耦合证据）

- `frontend/package.json`：`postinstall: quasar prepare`，scripts 全部为 `quasar dev/build`；
- 24 个 `.vue` 文件广泛使用 `q-table/q-dialog/q-btn/q-select/q-menu` 等私有组件；
- 全局服务耦合：`useQuasar/$q.dialog/$q.notify/$q.dark`、`useDialogPluginComponent`、
  `QTableColumn` 类型直接出现在业务代码；
- `boot/theme.ts`、`quasar.config` 承载品牌色、通知、路由模式等隐式约定。

Quasar 的核心卖点（一套代码发 SPA/PWA/Electron/移动端）在本项目完全用不到，
却承担其全部耦合代价。同赛道开源中后台项目（Vue Vben Admin、Soybean Admin、
vue-pure-admin、Refine、react-admin、NocoBase）无一使用 Quasar。

## 4. 对"源码自有"的诚实描述（耦合形式转换，非消除耦合）

采纳评审意见：shadcn/ui 消除的是"第三方黑盒组件约束"，不是所有耦合。明确接受
以下代价并给出缓解：

| 代价 | 缓解措施 |
|---|---|
| 上游 bug/安全/无障碍修复不自动进入 | `components/ui/` 记录每个组件拷贝时的上游 commit；按季度评估同步 |
| 本地修改使升级 diff 变贵 | 原语层（`components/ui/`）只做 token 适配，不改行为；业务定制在上层封装 |
| 组件间交互/token 漂移 | 主题 token 集中在单一 CSS 变量层；新增组件须复用既有 token（lint 检查） |
| 小型设计系统维护责任 | 组件层改动必须过评审并附视觉回归截图（见 ADR-5） |

## 5. 浏览器契约（显式变更）

现有 `quasar.config` 支持目标含 Safari 14 / Firefox 115 / Chrome 115。Tailwind 4
与 React 19 的现代特性基线要求收紧契约：

- **新契约**：Chrome/Edge ≥ 111、Firefox ≥ 128、Safari ≥ 16.4（与 Tailwind 4
  官方基线对齐）；明确放弃 Safari 14 及以下。
- 不引入降级构建或 polyfill 层；构建目标在 Vite `build.target` 显式声明。
- Playwright 覆盖 Chromium / Firefox / WebKit 三引擎最新稳定版，与声明契约对应。
- 该变更写入 `docs/CONFIGURATION.md` 与 README 的客户端要求章节。

**受影响用户面与接受理由（二审补充，需求方已于 2026-08-27 确认）**：放弃 Safari ≤ 14 意味着
仍在使用旧版 Safari 的存量设备（典型是被企业 IT 策略锁定、无法升级系统的
Mac/iOS）将无法访问控制台。本项目是登录后的内部管理控制台，目标用户为受控的
运营/管理员群体，设备环境可由组织侧约束，旧 Safari 用户占比预期极小且可通过
升级浏览器解决；因此接受该可达性损失，换取 Tailwind 4 / React 19 的现代特性
基线（不承担降级构建与 polyfill 层的长期成本）。**若产品侧后续确认存在不可升级
的存量用户，ADR-1 整体需重审。**

## 6. 被否决路线

- **继续 Quasar**：§3 耦合证据；跨端能力无使用场景。
- **Element Plus / Naive UI / Ant Design Vue 整体替换**：仍是黑盒组件库，
  深度定制与视觉天花板问题不解决，只换了一个供应商。
- **全自研组件**：重复建设焦点管理、无障碍、弹出层定位等高危原语（shadcn 底层
  Radix 已解决），投入产出比不成立。

## 7. 迁移与退出成本

- 迁入：24 个展示组件重写（计入 ADR-5 工作量）。
- 迁出：shadcn 组件即仓库源码，无供应商锁定；Tailwind 类名渗透模板，更换设计
  系统需重写皮肤层但不动逻辑层（headless 表格与解释器 core 不受影响）。
  退出成本评估：**中**。

## 8. 何时重新评估

- Tailwind 4 生态出现重大倒退或停止维护；
- Radix/shadcn 上游停止维护且 fork 维护成本超过一个黑盒组件库的定制成本；
- 浏览器契约需要下探到旧 WebKit（如出现该需求，Tailwind 4 选型本身需重审）。
