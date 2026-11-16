# 审批退回重提后多维表全字段同步 — 实施计划

- 分支：`feat/feishu-approval-form-sync`
- worktree：`D:\code\lib_yang\project\yang-system-feishu-form-sync`
- 设计：`docs/architecture/2026-11-13-feishu-approval-form-sync-design.md`
- 原则：每一步先写对抗测试，再实现；每个新增守卫做一次摘除式变异（删掉判断确认测试变红）。

## 步骤

### S1 解析器扩展（`approval_export/form.rs` 或共享 `approval_instance_form`）

- `FormNode` 增加顶层 `open_ids`、附件 `ext`/URL、电话、RFC3339 → 毫秒。
- `InstanceForm` 递归索引控件组 `value` 与嵌套 `fieldList`。
- 对抗测试：
  - RFC3339：`Z`、`+08:00`、`-05:30`、小数秒、`0`、`"0"`、非法时区、闰年边界。
  - contact：顶层 `open_ids`、`value.open_ids`、只有 user_id、空数组。
  - 附件：URL 数组、逗号串、对象、缺 `ext`、空 URL、超长文件名。
  - 控件组：`leaveGroupV2/workGroup/outGroup` 子控件；嵌套 `fieldList` 单行/多行。
  - 空值三态：缺失、`null`、`""`、`[]`、`false`、`0`。

### S2 `SyncValue` 与规范化

新增 `src/addon/feishu/domain/approval_form_sync/value.rs`。

- 枚举：`Text / Number(FixedNumber) / Bool / Labels / PersonIds / Millis / Attachments / Empty`。
- 规范化：标签 trim、多选排序、人员去重排序、附件按 `name+size` 集合、数字定点、日期秒级对齐。
- 对抗测试：`100` vs `100.0`、`9.1330` vs `9.133`、`0` vs empty、`false` vs empty、附件顺序不同但集合相同、人员顺序不同、重复标签。

### S3 全字段反向映射（`plan.rs`）

- 输入 `WidgetMap + InstanceForm + FieldItem`，输出 `FieldPatch / skipped / reviews`。
- 外部选项/部门/关联通过 trait 注入，纯函数可测。
- 对抗测试（逐类型）：
  - input/textarea：富文本分段、纯空白。
  - number/amount：非法字符串、超精度、指数、单位后缀。
  - formula：目标 19/20 拒绝；非聚合目标 2 可写；聚合派生模式。
  - date/dateInterval：单列/两列/文本；start>end；缺结束列。
  - radio/checkbox/select/multiSelect：固定选项多对一、外部选项查不到/停用/同名、目标选项不存在。
  - contact：只有 user_id、跨应用 open_id、空数组。
  - department：解析失败、目标人员列拒绝。
  - connect：多 Code、超链接取第一条、同表关联反查失败/歧义、跨表拒绝。
  - attachmentV2/image/imageV2：URL 缺失、超过 20MB、文件名缺失。
  - 控件组/嵌套 fieldList：单行、多行、缺子控件。
  - 重复映射同一 `bitable_field`：fail-closed。
  - 目标列类型不匹配：配置期拒绝/运行期 review。

### S4 单元格编码（`encode.rs`）

- `SyncValue → batch_update` 值；清空按类型；`allow_clear`。
- 对抗测试：文本空串、数字 null、日期 null、单选 null、多选 []、人员 []、复选框 false、超链接对象、附件 `[{"file_token"}]`。

### S5 附件上传（`attachments.rs`）

- 签名 URL 下载 → `medias/upload_all` → `file_token`。
- 对抗测试：429 重试、5xx、超过 20MB、图片/附件 parent_type、响应缺 file_token、下载大小不一致、URL 过期。
- 去重：当前附件 `name+size` 相同不重复上传。

### S6 外部选项/部门/关联解析（`resolvers.rs`）

- `feishu_option(source_key, option_id/label)` 反查；固定选项反查；部门名解析；同表关联 Code→record_id。
- 对抗测试：查不到、停用、同名多 id、merge 选项、关联多匹配、回填列为空。

### S7 差异与应用（`apply.rs`）

- 读当前台账 → diff → 解析字段名 → `batch_update` → 写后回读。
- 对抗测试：
  - 无变化不写；只写变化字段。
  - 台账被锁定/403；记录被删除；目标列被删除/改名/类型变化。
  - 未知列 tolerant rekey；字段重名 fail-closed。
  - 批量 Fatal 对半拆毒；Retry 不拆批。
  - 写成功但状态落库失败 → 下一轮只补状态。
  - 重复事件/乱序事件 → 幂等。
  - PII：错误串不含表单值。

### S8 Schema / 配置 / Action

- 配置表：`form_sync_mode`、`form_sync_allow_clear`、`form_sync_row_key_widget`、`form_sync_aggregate_derived_mode`。
- task/group：状态、最近退回毫秒、哈希、同步时间、调度、重试、错误、租约；组加 `member_row_keys`。
- `create_config`/`update_config`/`list_configs`/`check_configs`；校验目标列可写、系统列冲突、重复映射、聚合行键。
- 对抗测试：旧客户端缺字段、类型错误、系统列冲突、目标列 19/20/18/21、重复目标、`form_sync_mode=live` 但无映射。

### S9 事件 worker 接入

- `detect.rs`：timeline/task_list 判定退回目标+重提证据；秒/毫秒归一。
- `process_event_inner` 在 `write_extended` 后调用同步（shadow/live）。
- `run_loop` 增加 `form_sync_due`，不受 `events.is_empty()` 影响。
- 对抗测试：
  - 正常审批不触发；退回未重提只标 `awaiting_resubmit`。
  - 退回给前一个审批人不触发；退回给发起人+重提触发。
  - 事件丢失、重提无事件、重复事件、旧 ROLLBACK 重放。
  - `instance_code` 为 NULL 的竞态；实例码不匹配 superseded。
  - 同步失败不消耗事件 10 次预算；同步状态写库失败才 Failed。

### S10 聚合组

- create 前冻结 `member_row_keys`；行键/下标匹配；`aggregate_derived_mode`。
- 对抗测试：行数不等、成员集合变化、重排、删一行加一行、行键缺失/重复、顶层公式/附件/多选模式、部分成员失败、大组 100+ 分片。

### S11 前端

- 配置详情模式/清空/行键/派生模式；任务页同步状态/错误/重试；`api.ts`/`types.ts`；Vitest。

### S12 集成与验收

- 真实 MySQL：新列默认值/可空/filterable/sortable/varchar(32)、租约并发、配置删除清理、`member_row_keys` 冻结。
- 真机 P0：form 最新性、台账锁可写、附件/外部选项回读、清空语义、聚合行编辑。
- 每个守卫摘除式变异。
- `cargo test --lib --locked`、`cargo test --all-targets --locked`、`cargo clippy -D warnings`、`cargo fmt --check`；前端 `pnpm check`。

## 当前状态（2026-11-16）

- [x] worktree 创建与基线
- [x] S1 解析器扩展
- [x] S2 `SyncValue` 与规范化
- [x] S3 全字段反向映射
- [x] S4 单元格编码
- [x] S5 附件上传
- [x] S6 外部选项/部门/关联解析
- [x] S7 差异与应用
- [x] S8 Schema / 配置 / Action
- [x] S9 事件 worker 接入（含周期对账、退避与耗尽转 review）
- [x] S10 聚合组（冻结行序、行键匹配、派生值模式）
- [x] S11 前端（配置项、任务同步状态列）
- [x] S12 本地门禁：`cargo test --lib` / `cargo test --all-targets` /
      `cargo clippy --all-targets -D warnings` / `cargo fmt --check` /
      前端 `typecheck` + `lint` + 全量 `vitest`（87 文件 / 1080 用例）/
      `python3.12 scripts/check_architecture.py`
  - 本机 Node 26 的 `localStorage` 需要
    `NODE_OPTIONS=--no-experimental-webstorage`，否则 jsdom 的 localStorage 被
    Node 的实验实现遮蔽，视图测试会在 `localStorage.clear()` 处整片失败。

### 尚未完成（均需外部环境，不能在本工作区伪造通过）

- [ ] 真实 MySQL 集成测试：新列默认值、`varchar(32)`、filterable/sortable、
      `member_row_keys` 只写一次、租约并发、配置删除清理。
- [ ] 真机 P0：同实例重提后 `GET /instances/:id` 的 `form` 是否最新；
      台账锁定后应用身份能否 `batch_update`；附件/外部选项/日期/清空的回读形态；
      聚合明细行增删与重排；事件信封 v1/v2。
- [ ] 前端 bundle 预算与 e2e：未在本次环境完整执行。
