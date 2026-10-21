# 2026-10-22 修复验收报告

> 基线：`main` = `9f68346`（与 `origin/main` 一致）
> 分支：`fix/full-project-review-2026-10-20`，最终代码 HEAD = `10449f0`（验收报告提交前共 26 个提交）
> 本地门禁：`full` + `integration` 全绿；`quick` 在最终部署/门户兼容改动后再次通过；GitHub Actions `CI`：`39657689603`（`3854903`）、`39659584751`（`e7e1e15`）、`39662660039`（`a5bd63e`）均成功；`10449f0` 的最终运行 `39667444577` 见第 2 节。

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
| `tests-ops#60` Docker 上下文含凭据 | 父仓 `.dockerignore` 提交 `e27a2d3` 已 push 到 `lib_yang/master` | 无（父仓 CI 历史失败为既有问题，不影响本次 nested CI） |
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
- `39662660039`（HEAD `a5bd63e`，共用订阅 URL 修复）：3 个 job 全部 success。
- `39667444577`（HEAD `10449f0`，部署/门户兼容改动）：3 个 job 全部 success（本轮验收的最终代码 HEAD）。
  - URL: https://github.com/ImYangJiaJun/yang-system/actions/runs/39667444577
- 仓库只有 `.github/workflows/ci.yml`，没有自动化 CD/deploy job；部署仍走 `deploy.ps1` / `deploy-finance-prod.ps1` 手工脚本。
- 父仓 `lib_yang` 的 `master` CI 在本次 `e27a2d3` 之前的 `c049459` 已有失败（dependency policy / 部分 feature matrix）；本次只改 `.dockerignore`，不涉及这些 job，且 yang-system 的 nested CI 使用该 master 已通过。父仓 CI 的历史失败需在框架仓单独处理。

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
| 门户 create/update | 请求体重新接受 `query_field_ids` 但完全忽略（不校验/不落库）；新客户端不应发送 | compatible（deprecated） | 旧直连客户端不再 400；响应仍不返回该字段，读取它的脚本需删字段；死配置不再入库/读取 |
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
| 部署脚本 `EdgeBindAddr` | 默认 `0.0.0.0` → `auto`（新部署 loopback） | compatible（auto 兼容模式） | 入口脚本默认 `auto`：已有线上容器沿用实际绑定，直接重部署不改变既有暴露面；全新部署才回落 loopback；显式 `-EdgeBindAddr` 可改暴露面 |
| 部署前只读预检 | 无 | fail-closed | `deploy-blue-green.sh` 在起绿容器/切流量前检查 user_token≤1、task_applicant 无重复、portal JSON 合法、event token 非空；失败即中止，线上不受影响；外部 MySQL 退回人工 SELECT |
| `logging.filter` | 默认追加 `,error` | behavioral（ops） | 生产 config 若显式写旧 filter，需手工加 `,error` |
| 新 Worker | `SessionCleanupWorker`（97 天保留）、`GrantExpiryWorker`（60 秒）、事件恢复 Action | behavioral | 无需配置；发布后观察会话清理/过期授权收敛 |
| 前端 | token 刷新回写、登出兜底、表格选择、日期 locale、权限页、Vite 8.3.4 | internal | 无外部契约变化 |

## 4. 直接重部署判定

**结论：对当前由部署脚本管理的 MySQL、已有线上容器的部署，可以直接重新部署。**
部署脚本保证两种结果之一：要么成功且保持现有暴露面/数据不变，要么在起绿容器或切流量之前 fail-closed 中止——不会静默改变公网可达性或把脏数据带进线上。

### 4.1 部署脚本已自动保障

1. **暴露面**：`deploy.ps1` / `deploy-finance-prod.ps1` 的 `EdgeBindAddr` 默认 `auto`；
   `deploy-blue-green.sh` 的 `resolve_bind_addr` 读取 `LIVE_BACKEND` 的实际 8081 绑定并复用。
   - 已有公网容器（当前 `0.0.0.0:18654`）→ 新容器继续 `0.0.0.0:18654`，**无需手工传参**；
   - 已有 loopback 容器 → 继续 loopback；
   - 没有线上容器（全新部署）→ 回落 `127.0.0.1`，公网需显式 `-EdgeBindAddr 0.0.0.0` 或 TLS 反代；
   - 显式 `-EdgeBindAddr 127.0.0.1` 可主动收回公网。

2. **存量数据预检**：`deploy-blue-green.sh` 在 `prepare()` 内、起绿容器之前只读检查：
   - `feishu_user_token` ≤ 1 行（`slot` 唯一索引）；
   - `feishu_approval_task_applicant` 无重复 `task_id`（唯一索引）；
   - `feishu_datasource_portal` 的 `exposed_columns` / `result_labels` 均为合法 JSON。
   任一失败即 `die`，不起绿容器、不切流量；按提示人工去重/修正（DB 写需单独批准）后重跑即可。

3. **飞书事件配置预检**：`[feishu].enabled = true` 时检查 `event_verification_token` 非空；
   缺失则中止，避免新版本静默不注册事件端点。确不使用审批事件时显式传
   `-AllowMissingEventToken`（或远端 `ALLOW_MISSING_EVENT_TOKEN=1`）。

4. **日志过滤器**：显式 `[logging].filter` 未包含 `error` 时只 warn（不阻塞）；框架层
   ERROR 可见性需要该全局兜底。

5. **父仓 Docker 上下文**：`.dockerignore` 修复 `e27a2d3` 已 push 到 `lib_yang/master`，
   远端构建不再把部署配置/口令文件带入上下文。

### 4.2 环境相关确认（不改变核心机器契约）

- **外部 MySQL**：若 `[mysql].url` 的 host 不是脚本管理的 `yang-mysql`，数据预检会跳过并
  warn；此时需按 §4.1 的三条只读 SELECT 人工确认。当前默认部署使用脚本管理的容器，预检自动生效。
- **飞书事件 token/Encrypt Key**：预检只能确认 token 非空，无法读取飞书后台；需确认应用配置与
  飞书开发者后台逐字一致、Encrypt Key 与飞书加密推送匹配。若事件订阅一直正常，token 已一致。
- **公开注册验证码端点**：已按 OCR 安全修复删除（无消费方，且可被匿名用于向任意邮箱发信）；
  仓库内前端/测试已同步。若存在外部注册页直接调用该 URL，会收到 404，需要改用
  `username + password` 注册。这不影响审批派发/外部选项/门户。
- **passphrase 门户 suggest**：由匿名可用改为 403（安全修复）；前端会把 403 吞成空联想，
  页面行为不变；外部表单若直接调用 suggest 需改走 `/query` 或 public 门户。
- **门户 `query_field_ids`**：服务端重新接受但完全忽略，旧直连脚本不再 400；响应仍不返回该字段。
- **行为性变化**：同步派发 42901/通知时序、删除配置/数据源级联、撤销权限不再踢下线、
  非管理员 lookup 不回显 email；均需调用方/运维知情，但不改变审批派发/外部选项的请求响应结构。

### 4.3 建议的预发验证

在预发/灰度用真实飞书订阅分别验证四条链路：challenge 应答、加密 `event_callback`（直接 URL 与共用 challenge URL 各一次）、单条派发（含 42901 与通知时序）、外部选项取数。验证通过后再切生产。

## 5. 残留 / 暂缓

- **0.1**：历史请求记录明文 Token、凭据轮换未处理。
- **P0-2**：Step-up 未接线，仅持 Access Token 仍可执行高危账号操作。
- **P0-9**：仓库内真实 Encrypt Key/生产密文未清理、未轮换。
- **事件恢复 UI**：API-only，无控制台页面。
- **外部 MySQL 部署**：发布前数据预检需人工执行三条 SELECT（脚本管理 MySQL 时自动执行）。
- **事件 token/Encrypt Key 一致性**：部署前需与飞书后台核对（预检只能确认非空）。

## 6. 结论

- OCR 非暂缓缺陷已全部闭环；5 条部分闭环已推进完成；父仓 `.dockerignore` 已 push。
- 本地 `quick`/`full`/`integration` 与 GitHub Actions CI 均全绿（最终代码 `10449f0` 的运行见第 2 节）。
- 对外业务口径：审批派发与外部数据源取选项的请求/响应结构不变；门户公开查询继续工作；
  删除/告警类行为变化与两个明确的安全移除（注册验证码端点、passphrase suggest）已列出。
- **重部署判定：`direct_redeploy_safe`（自动 fail-closed）**。当前部署（脚本管理 MySQL、
  已有线上容器）可以直接重部署：绑定自动沿用、数据/事件配置预检在动线上前兜底；
  外部 MySQL 或事件 token 一致性需按 §4.2 人工确认。
