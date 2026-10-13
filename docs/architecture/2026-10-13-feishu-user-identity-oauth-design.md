# 飞书用户身份（OAuth）集成设计

> 状态：已实施（2026-10-13）。代码入口：`src/addon/feishu/domain/user_token.rs`、
> `src/addon/feishu/approval/actions/oauth_*.rs`、`src/infrastructure/feishu_oauth_reminder.rs`。

## 1. 目标与非目标

**目标**：让审批派发的 `applicant=姓名` 参数真正可用——姓名 → open_id 走的
`contact/v3/users/search` **只接受 `user_access_token`**，而本系统此前只有 tenant 身份。
为此集成飞书 OAuth：管理员本人授权一次，系统持有并自动续期用户凭证；授权满 350 天后
每天给系统管理员发提醒，直到重新授权。

**非目标**：

- 不做多用户/多份凭证（系统只持一份身份，重新授权是**整体替换**）；
- 不保留 tenant token 回退路径（实测必败 99991663，留着只会把故障掩盖成一次多余的出站）；
- 不做 350 天以外的告警（提前撤销、刷新失败早于 350 天只记日志，由派发路径的可行动文案暴露）。

## 2. 事实基础（真机核实）

| 事实 | 出处 |
|---|---|
| `contact/v3/users/search` 对 tenant token 返回 **99991663**（token 类型不支持） | 真机实测 |
| 授权页 `accounts.feishu.cn/open-apis/authen/v1/authorize`；令牌端点 `accounts.feishu.cn/oauth/v3/token`（RFC 6749，`client_id`/`client_secret` 在请求体） | 官方文档 |
| `offline_access` **必须**出现在授权链接的 scope 里，否则令牌响应没有 `refresh_token` | 官方文档 |
| `refresh_token` **一次性**：刷新即换新，旧值立即失效；`access_token` 约 2 小时且 `expires_in` **非固定值** | 官方文档 |
| 用户授权满 **365 天**后必须重新授权，继续刷新报 `20037`；**刷新不重置**这条时钟 | 官方文档 |
| 授权页的 scope 必须是开发者后台已开通权限的子集，否则 **20027** | 官方文档 |
| 本部署是裸 IP + 端口（安全组只放行 18xxx），回调只能是 `http://<ip>:<port>/...` | 部署现状 |
| 框架的 `ResponseBody::Raw` 把 `content_type` 限死为 `application/json` | `yang-base/src/action/response.rs:446` |

## 3. 决策记录

| # | 决策 | 理由 |
|---|---|---|
| D1 | 新表 `feishu_user_token`，**单例行**（至多一行） | 只有管理员一个身份；重新授权是替换而不是新增 |
| D2 | `authorized_at` 是**普通列**而非 `created_at` | 重授权必须重置 350/365 天时钟，`created_at` 只在首插时写 |
| D3 | **每日无条件保活刷新**，不只惰性刷新 | `refresh_token` 一次性且寿命短（文档示例 7 天）；只做「用到才刷」的话，静默期一长整条链就断了，而断链只能靠重新授权补救 |
| D4 | 刷新走**进程内串行**（`static LazyLock<tokio::sync::Mutex<()>>` + 锁后复查） | 并发刷新会撞 `20073`，还可能把刚换到的凭证写丢；单实例假设与仓库三个 worker 一致（多实例需换 Redis 锁，照 `TenantTokenCache`） |
| D5 | OAuth `state` 存**进程内内存表**（10 分钟 TTL、一次性），不引 Redis、不加列 | 单实例、一次性，且失败闭合（重启丢 state → 重新点一次「去授权」）；少一个依赖面 |
| D6 | 用户凭证的取用写成**自由函数**（`user_token::user_access_token`），不新增结构体/trait | 与 `resolve_applicant`/`alert_*` 的 domain 层函数式风格一致 |
| D7 | **不动 `Outbound`/`outbound_setup`**，在派发的调用点就地取凭证 | `Outbound` 描述的是 tenant 身份出站，混入 user 身份会模糊语义；接线量也最小 |
| D8 | 提醒 worker **独立成文件**（86400s），不加 `last_reminder_at` 冷却列 | 独立 worker 天然一天一封；与 `alert.rs` 明确的「不冷却、持续提醒」哲学一致（没收到邮件会被读成「已经好了」） |
| D9 | 提醒邮件**不含链接**，正文给出操作路径 | 不新增配置项，也不复用密码重置专用的 `email.password_reset.link_base_url` |
| D10 | 未配置 OAuth 时**不回退** tenant 路径，直接给可行动错误 | 回退必然失败且掩盖配置缺口 |
| D11 | 授权 `scope` 用常量 `contact:contact.base:readonly offline_access` | 两者都必须在开发者后台开通；真机若报 20027 / 99991672 按返回的 `permission_violations` 校正 |
| D12 | 回调返回 **JSON 信封**而不是漂亮的 HTML 页 | 框架刻意把 `ResponseBody::Raw` 限死为 JSON，不绕；文案改成「飞书授权成功，可关闭本页」 |
| D13 | 收件人走**表 DSL** 读 `users`，不用原生 SQL | `users.email` 标了 `readable_by([SYSTEM_ROLE])` 而 `Repository` 正以该角色查询；原生 SQL 在本仓是要登记的逃生口，读有表声明的表够不上那个门槛 |

## 4. 数据模型：`feishu_user_token`

| 列 | 类型 | 说明 |
|---|---|---|
| `id` | `Key` | 主键 |
| `open_id` / `user_name` | `Str(64)` 必填 / `Str(128)` | 授权人身份与展示名 |
| `access_token` / `refresh_token` | `Text` 必填、`secret`、仅 `SYSTEM_ROLE` 可读写 | 照 `feishu_datasource_field.token_cipher` 的处置 |
| `access_token_expires_at` / `refresh_token_expires_at` | `Timestamp` 必填 | 以响应里的 `expires_in` / `refresh_token_expires_in` 实算 |
| `scope` | `Str(1024)` | 授权范围快照：报 99991672 时不必先重新授权就能区分「授权时没要」与「后台没开通」 |
| `authorized_at` | `Timestamp` 必填 | **最近一次完整授权时间**（350 天判据） |
| `created_at` / `updated_at` | 标准列 | |

**不变量**：至多一行。读写按「第一行」定位（`load_single`），不依赖「插入时能显式指定
自增主键」这一 DSL 行为。

## 5. 三条时钟与刷新语义

```
授权（完整 OAuth）          刷新（每日保活 / 用到就刷）
      │                            │
      ├─ authorized_at ────────────┼──────► +365 天：刷新报 20037，必须重新授权
      │                            │        （刷新**不**重置这条时钟）
      ├─ access_token_expires_at ──┘ +2h：派发路径按需刷新（留 300s 余量）
      └─ refresh_token_expires_at ── +N 天（文档示例 7 天）：每日保活把它一直推后
```

令牌端点**不重试**（`OutboundRequest.idempotent = false`）：`code` 与 `refresh_token`
都是一次性的，响应丢失时重发只会拿到「已使用」；刷新那条更糟——新凭证已经签发但响应丢了，
重发拿不回来。这条风险已在 §8 记账。

## 6. 端点

| 端点 | 方法 | 权限 | 注册门禁 |
|---|---|---|---|
| `/api/v1/feishu/oauth/status` | POST | `feishu.approval.read` | 无条件 |
| `/api/v1/feishu/oauth/authorize-url` | POST | `feishu.approval.write` | 无条件 |
| `/api/v1/feishu/oauth/callback` | GET（`public`） | — | 与派发端点同门禁（`can_pull()`） |

回调的 query 参数用框架的 `yang_base::params!` 声明（`#[param(source = query)]`），
**刻意不加 `deny_unknown_fields`**：浏览器与中间代理会经手这个地址，多一个无关参数不该 400。
回调**不回显任何入参**，失败详情只进日志。

## 7. 失败文案（可行动是唯一标准）

| 场景 | 文案要点 |
|---|---|
| 从未授权 | 「请在『飞书审批配置』页点『去授权』」 |
| 刷新凭证失效（20026/20037/20064/20073） | 「授权已失效，请重新授权（满 365 天飞书强制重新授权）」 |
| 应用级缺 scope（99991672） | 「通讯录权限未开通，等待审核」 |
| 用户凭证缺 scope（99991679） | 「用户授权缺少通讯录权限，请重新授权并在授权页补充权限」 |
| 凭证形态非法（99991668/99991671） | 「飞书用户凭证无效，请重新授权」 |
| 后台开关未开（20074） | 「开发者后台『刷新 user_access_token』开关未开启」 |

## 8. 已知缺口与风险

| 风险 | 处置 |
|---|---|
| 后台拒绝 IP 形式的 redirect URI | 备选：nginx 反代 + 域名；或先用 `http://localhost:18654/...` 联调 |
| `users/search` 实际所需 scope 与常量不符 | 按 20027 / 99991672 返回的 `permission_violations` 校正 `OAUTH_SCOPES` |
| 刷新成功但写库失败 → 凭证链断裂 | 记 error 日志；下次派发/提醒暴露为可行动文案，引导重新授权（无更好的幂等手段） |
| 进程重启丢掉未消费的 state | 失败闭合：重新点一次「去授权」 |
| 多实例部署 | 当前单实例假设（与仓库 worker 一致）；多实例需把 D4/D5 换成 Redis 锁与 Redis state |
| 授权后 `users` 表被清空/首个账号无邮箱 | 提醒降级为 `tracing::warn!`，不阻塞；派发仍能正常工作 |

## 9. 人工操作清单（部署方）

1. 开发者后台 → 安全设置 → 添加重定向 URL（须与 `feishu.oauth_redirect_uri` **逐字一致**）；
2. 权限管理 → 开通 `offline_access` 与 `contact:contact.base:readonly`；
3. 确认「安全设置 → 刷新 user_access_token」开关开启（默认开，改动需发版生效）；
4. 配置 `[feishu] oauth_redirect_uri = "http://<host>:<port>/api/v1/feishu/oauth/callback"`
   （不是密钥，只走配置文件：**不登记** secret 文件与环境变量入口）；
5. 部署后在「飞书审批配置」页点「去授权」完成首次授权。

## 10. 验收

- 控制台显示「已授权：<姓名> · <时间>」；未授权时卡片给出提示与入口；
- 派发带 `applicant=姓名` 的请求能解析出 open_id（此前必然失败）；
- 未授权 / 凭证失效 / 缺 scope 三类失败各自给出上表对应的文案；
- 授权满 350 天后每天一封提醒邮件（阈值由 `user_token::reauth_reminder_due` 单测钉住），
  重新授权后自动停止。
