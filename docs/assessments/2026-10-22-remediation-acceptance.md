# 2026-10-22 修复验收报告

> 基线：`main` = `9f68346`（与 `origin/main` 一致）
> 分支：`fix/full-project-review-2026-10-20`，最终代码 HEAD = `a5bd63e`（共 25 个提交）
> 本地门禁：`full` + `integration` 全绿；GitHub Actions `CI`：`39657689603`（`3854903`）、`39659584751`（`e7e1e15`）均成功；`a5bd63e` 的最终运行 `39662660039` 见第 2 节。

## 1. OCR 缺陷闭环

66 条发现的最终状态：

| 状态 | 数量 | 说明 |
|---|---:|---|
| 已闭环 | 57 | 前一轮修复 |
| 部分闭环 → 本轮闭环 | 5 | 见下表 |
| 按决策暂缓 | 2 | `account#1`（P0-2）、`feishu-data#21`（P0-9） |
| 误报，无需修复 | 2 | `access-infra#12`、`frontend-features#48` |

| 部分闭环项 | 本轮闭环证据 | 残留 |
|---|---|---|
| `account#3` 会话行无限累积 | `session_cleanup` Worker：refresh 硬上限 90 天 + 7 天余量（97 天），6 小时一轮，数据库时钟，真实 MySQL 用例 + 摘除式变异 | 无 |
| `access-infra#13` 过期直授残留 | `authz_grant.converged_at` + 收敛索引，60 秒 Worker 递增 `authz_version` 并写 Outbox，先 CAS 标记再递增，续期重置标记，真实 MySQL 用例 + 变异 | 积压时按 200 行/轮排空，契约已限定上界 |
| `feishu-approval#38` 事件终态永久丢弃 | `list_events` / `retry_event`：终态 CAS 回队，成功事件拒绝重放；集成测试 + 变异 | 控制台 UI 暂未接入（API-only，文档已写明） |
| `tests-ops#60` Docker 上下文含凭据 | 父仓 `.dockerignore` 本地提交 `e27a2d3` | 父仓 `master` 尚未 push（共享框架仓，需单独确认） |
| `feishu-approval#30` / P0-8 历史明文 | 新写入脱敏 + 历史行读侧脱敏（解析失败 fail-closed） | 库内明文清洗与 Token 轮换仍属暂缓项 0.1 |

明确暂缓：P0-2（Step-up 接线）、P0-9（真实 Encrypt Key）、0.1（凭据轮换，含历史请求记录 Token 轮换/清洗）。

## 2. 门禁

### 本地

- `full`：架构/部署脚本/格式检查通过；Rust all-target 1206 passed / 0 failed（11 ignored）；clippy `-D warnings` 通过；pnpm audit 无已知漏洞；前端 77 files / 873 tests；dev E2E 21 passed；production E2E 4 passed；`CI profile passed: full`。
- `integration`：全部套件通过，含新增 `session_cleanup`、`grant_expiry` 真实 MySQL 用例、`user_lookup`（修复后）、`feishu_approval_dispatch_integration` 11 passed；`CI profile passed: integration`。
- 验收 Workflow 发现共用订阅 URL + Encrypt Key 的委托回归后，补修复 `a5bd63e`；定向 `feishu_approval_dispatch_integration` 12 passed（含加密 event_callback 回归），clippy / architecture check 通过，摘除修复时该用例变红。

### GitHub Actions

- 失败历史：`a83864e`、`f69ff17`、`4421449` 的 `Real MySQL 8 and Redis 7` 均在 `user_lookup_is_authenticated_only_and_filters_by_keyword` 失败，错误 `Token 已被撤销`；根因是改绑邮箱按秒写 `min_iat` 水位线，测试在同一秒重新登录被 `iat <= watermark` 判为已撤销。修复 `ba7e28d` 在两次绑定后 sleep 1.1s。
- `39657689603`（HEAD `3854903`）：3 个 job 全部 success。
- `39659584751`（HEAD `e7e1e15`）：3 个 job 全部 success。
- `39662660039`（HEAD `a5bd63e`，共用订阅 URL 修复）：3 个 job 全部 success（本轮验收的最终代码 HEAD）。
  - URL: https://github.com/ImYangJiaJun/yang-system/actions/runs/39662660039
- 仓库只有 `.github/workflows/ci.yml`，没有自动化 CD/deploy job；部署仍走 `deploy.ps1` / `deploy-finance-prod.ps1` 手工脚本。

## 3. 对外口径变化（main → HEAD）

### 3.1 机器对外

| 对外面 | main | HEAD | 分类 | 影响 / 动作 |
|---|---|---|---|---|
| `POST /api/v1/feishu/approval/dispatch/{config_key}` | 请求 `token/record_id/applicant/requested_by/notification_recipient`；响应 `DispatchOutcome`；按配置 token 鉴权 | 结构/鉴权不变；新增共享令牌桶，失败码 `42901`；显式接收人只在终态由 HTTP 层发送，中间态交 worker；请求记录 token 脱敏 | behavioral | 已有工作流不改请求；需能处理 `42901` 可重试；接受中间态通知稍后由 worker 发 |
| `POST /api/v1/feishu/approval/options/{source_key}`（外部数据源取选项） | public + 数据源 token 自校验 | `src/addon/feishu/option/` 零 diff | compatible | 无需调用方配合；仍需飞书凭证段已配置（`can_pull()`） |
| `POST /api/v1/feishu/approval/event`、`/api/v1/feishu/event/challenge` | token 不匹配只告警；配置 Encrypt Key 仍收明文；无 token 也注册 | token 缺失/不匹配 401/403；配置 Encrypt Key 拒明文；未配 `event_verification_token` 时两个端点都不注册（404）。**共用订阅 URL + Encrypt Key 的委托回归已修复（`a5bd63e`）**：challenge 解密后委托事件 handler 时不再重复传 `encrypt_key`，否则已解密 body 会被误判为明文注入 401；集成测试 `shared_challenge_url_accepts_encrypted_event_callback` 覆盖，摘除修复时变红 | breaking（配置不符时） | 部署前确认 token 与飞书后台逐字一致、Encrypt Key 与飞书加密推送匹配；共用 URL 现已可用 |
| `POST /api/v1/users/registration-email-verifications` | 公开验证码端点 | 整体删除 | breaking（机器） | 仓库内无消费方；外部注册页/脚本需改用 `username+password` 注册 |
| `GET /api/v1/feishu/portal/{slug}/suggest` | 匿名可用，不判 access_mode、不限速 | public 匿名可用；login 要求登录；passphrase 一律 40301；按 portal_id 限速 | breaking（受保护门户） | 外部对 passphrase 门户调用 suggest 的表单会 403；改用 `/query` 或 public 门户 |
| `POST /api/v1/feishu/portal/{slug}/query` | 开放列子值只取 100 行；配置 JSON 损坏静默空 | 分页取全（上限 10000，超限明确提示）；配置损坏显式 ConfigError | behavioral | 响应结构不变；大数据量门户观察 DB/延迟 |

### 3.2 控制台 / 管理 API

| 对外面 | 变化 | 分类 | 影响 / 动作 |
|---|---|---|---|
| OpenAPI 操作集合 | 85 → 86：+`list_events`、+`retry_event`、-`request_registration_email` | additive + breaking（删除） | 已有 operationId 无改名；控制台前端已同步 |
| 门户 create/update | 请求体删除 `query_field_ids`（`deny_unknown_fields`） | breaking（直连脚本） | 控制台已不再发送；外部脚本需删该字段，否则 400 |
| `configs/delete` | 从“只删 pending 任务”改为“删该配置全部任务 + task_applicant”；响应新增 `deleted_tasks`/`deleted_applicants` | behavioral | 有意的数据销毁；需保留任务历史则删除前先导出 |
| `requests/query` 的 `request_body` | 新行落库脱敏；历史明文读侧脱敏；解析失败固定占位 | behavioral | 字段名/类型不变；历史 Token 轮换仍暂缓 0.1 |
| `users/lookup` 的 `email` | 非 `account.users.read` 调用方一律 null | behavioral | 外部收邮箱工具需单独授权 |
| `access/grants*` 的管理员等价清单 | 新增 `feishu.approval.write`；委派子集闸门 | behavioral | 非 system_admin 委派账号不能再授予/组内携带该权限；已有授予事实不受影响 |
| `access/grants/revoke` | 只递增版本 + Outbox，不再杀 Refresh 会话 | behavioral | 运维若把“撤销权限”当“踢下线”，改用会话撤销 |
| `datasources/table` update/delete | pull 坐标按合并后状态校验；删除数据源级联删除引用门户 | behavioral | 现有 pull 源坐标缺失时 update 会 400；删除源会让相关门户 URL 失效 |
| `approval/configs/create|update` | 时区 IANA、回填列重复在配置期 400 | behavioral | 只影响新写入；编辑旧配置前先确认 |

### 3.3 Schema / 部署 / Worker

| 对外面 | 变化 | 分类 | 动作 |
|---|---|---|---|
| `authz_grant` | 新增 `converged_at` + `idx_authz_grant_expiry_convergence` | additive | 启动 schema 同步自动加；过期直授会在 60 秒内收敛并失效旧 Access Token |
| `feishu_user_token` | 新增 `slot` NOT NULL default 1 + 唯一索引 | additive，可能阻塞启动 | 部署前确认表内至多一行，否则唯一索引失败、进程不启动 |
| `feishu_approval_task_applicant` | `task_id` 唯一索引 | additive，可能阻塞启动 | 部署前确认无重复 `task_id` |
| `feishu_approval_event.retry_count` | 增加 filterable（应用层） | additive | 无 DDL |
| `query_field_ids` 列 | API/表声明删除，数据库列保留 | compatible | schema 同步不删列；旧数据可忽略 |
| 部署脚本 `EdgeBindAddr` | 默认 `0.0.0.0` → `127.0.0.1` | breaking（ops） | 当前公网部署必须显式 `-EdgeBindAddr 0.0.0.0` 或配 TLS 反代，否则公网不可达 |
| `logging.filter` | 默认追加 `,error` | behavioral（ops） | 生产 config 若显式写旧 filter，需手工加 `,error` |
| 新 Worker | `SessionCleanupWorker`（97 天保留）、`GrantExpiryWorker`（60 秒）、事件恢复 Action | behavioral | 无需配置；发布后观察会话清理/过期授权收敛 |
| 前端 | token 刷新回写、登出兜底、表格选择、日期 locale、权限页、Vite 8.3.4 | internal | 无外部契约变化 |

## 4. 直接重部署判定

**结论：不能零参数直接重部署；按下列前置动作执行后，审批派发与外部数据源核心契约兼容，可无缝切换。**

### 4.1 阻塞部署的前置动作

1. **公网边缘绑定**
   - 当前若仍以 `公网 IP:18654` 直接访问（飞书外部选项/审批工作流/门户/控制台），部署命令必须显式：
     - 测试/财务：`-EdgeBindAddr 0.0.0.0`
     - 或改为宿主机 TLS 反代 + 保持 `127.0.0.1`
   - 否则新容器只绑 loopback，发布完成即外部不可达。

2. **数据库预检（只读）**
   ```sql
   SELECT COUNT(*) AS rows FROM feishu_user_token;
   -- 必须 <= 1；多行时 ADD COLUMN slot DEFAULT 1 后 ADD UNIQUE INDEX 会失败

   SELECT task_id, COUNT(*) AS rows
   FROM feishu_approval_task_applicant
   GROUP BY task_id
   HAVING COUNT(*) > 1;
   -- 必须 0 行；重复会让 schema_sync 预检拒绝全部 DDL、启动中止

   SELECT id, slug FROM feishu_datasource_portal
   WHERE (exposed_columns IS NOT NULL AND JSON_VALID(exposed_columns) = 0)
      OR (result_labels IS NOT NULL AND JSON_VALID(result_labels) = 0);
   -- 必须 0 行；损坏 JSON 现在会让该门户公开查询直接 5xx
   ```
   - `feishu_user_token` 多行 → `slot` 唯一索引创建失败，进程不启动；该列是新增列，schema_sync 会跳过唯一索引预检，失败只能在 DDL 阶段暴露。
   - `task_applicant` 重复 `task_id` → 唯一索引预检失败，启动中止。
   - 如不满足，先在维护窗口人工去重/修正（DB 写操作，需单独批准）。
   - `authz_grant` 的 `converged_at` + 索引是普通 DDL；大表需预留元数据锁时间。

3. **飞书事件配置**
   - `feishu.event_verification_token` 必须非空且与飞书后台逐字一致；未配置时 `/api/v1/feishu/event/challenge` 与事件接收端点不注册（404）。
   - 如配置 `feishu.event_encrypt_key`，确认飞书侧已启用加密推送；否则合法推送会因明文被拒。
   - 事件订阅是运行时状态，发布后 worker 会按启用配置重订；确认订阅成功。

4. **日志过滤器**
   - 生产 `config.toml` 若显式写 `[logging] filter`，改为包含 `,error`（或删除该行使用新默认）；否则 `yang_base/yang_db/yang_runtime` 的 ERROR 仍被静默丢弃。

5. **父仓 Docker 上下文**
   - 父仓 `.dockerignore` 的修复（`e27a2d3`）仍在本地 `master`；从远端 `lib_yang` 重新构建镜像前需 push/合并，否则部署配置/口令文件仍可能进入构建上下文。

### 4.2 不影响核心服务、但需调用方/运维确认的变化

- **审批派发**：请求/响应结构未变；新增 `42901`（共享 90 次/分钟令牌桶），中间态通知改由 worker 发送，显式接收人只在终态由 HTTP 层发送。工作流需能重试 `42901`；Redis 抖动也会让单击派发返回 `42901`，高峰下点击可能等待数百毫秒到数秒。
- **外部数据源取选项**：`feishu.option.*` 零 diff；已有「关联外部选项」无需改动，但公网可达性与飞书凭证段仍需满足。
- **门户**：公开门户 URL/查询继续工作；passphrase 门户的 suggest 变为 403；限速键改为 portal_id（滚动部署重叠期新旧键短暂各算各的预算）；删除数据源会级联删除相关门户；开放列分页取全最坏约 100 次 DB 查询/请求，发布后观察公开端点延迟与 DB 负载；损坏的 `exposed_columns`/`result_labels` JSON 现在会让该门户查询 5xx，部署前按 §4.1 的 `JSON_VALID` 检查。
- **账号/会话**：撤销会话后 refresh fail-closed；全量失效路径会标记所有会话；旧 Token 若缺 `session_id` 仍兼容；历史缺会话行的用户可能需重新登录。撤销权限不再等于踢下线。
- **控制台**：事件恢复从「翻库」变为 `list_events`/`retry_event` API（无 UI）；删除配置会删除全部任务；请求记录历史 Token 读侧脱敏；非管理员 lookup 不回显 email；`query_field_ids` 直连脚本需删字段；注册验证码端点已删除。
- **历史请求记录**：库内明文与派发 Token 轮换仍按暂缓项 0.1；如需彻底消除，需单独轮换受影响配置 Token 并清洗该列（DB 写，需批准）。

### 4.3 建议的预发验证

在预发/灰度用真实飞书订阅分别验证四条链路：challenge 应答、加密 `event_callback`（直接 URL 与共用 challenge URL 各一次）、单条派发（含 42901 与通知时序）、外部选项取数。验证通过后再切生产。

## 5. 残留 / 暂缓

- **0.1**：历史请求记录明文 Token、凭据轮换未处理。
- **P0-2**：Step-up 未接线，仅持 Access Token 仍可执行高危账号操作。
- **P0-9**：仓库内真实 Encrypt Key/生产密文未清理、未轮换。
- **事件恢复 UI**：API-only，无控制台页面。
- **父仓 `.dockerignore`**：本地提交未 push。
- **生产配置/数据**：未读取；上述 preflight（user_token/task_applicant 行数、event token/encrypt、logging filter、实际绑定）需部署前由运维确认。

## 6. 结论

- OCR 非暂缓缺陷已全部闭环；5 条部分闭环已推进完成。
- 本地 full + integration 与 GitHub Actions CI 均全绿。
- 对外业务口径：审批派发/外部数据源核心请求响应兼容；事件回调、门户 suggest、门户 CRUD 死字段、部署绑定等存在需要确认的破坏性/行为性变化。
- **重部署判定：`safe_with_required_actions`**。完成 §4.1 的 5 项前置后可重新部署；不做前置直接部署会导致公网不可达或 schema 预检失败。
