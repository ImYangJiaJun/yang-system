# 前端基础设施修复与验证

日期：2026-10-25。分支：`codex/infrastructure-remediation-20261025`。本文只记录前端代理负责的源码和本地验证，不表示部署或生产验收完成。

## 条目与触点

| ID | 本包处理 | 触点与验收 |
| --- | --- | --- |
| R-02 | 已修复本地会话代次竞态 | `session-controller.ts`、`auth-session.ts`、`client.ts`。开始/清除会话递增代次；旧 restore/refresh 成功与终态失败不能覆盖新状态；旧单飞完成不能清除新单飞。受保护 Action 的响应体解析也位于代次检查内，迟到的凭据变更回执不能清除新登录。登出/停用的迟到 finally 同样受保护。覆盖退出、重新登录、凭据变更、跨标签结束的本地边界。 |
| R-03 | 已修复等待预算 | 刷新的 10 秒预算在跨标签锁前开始，Web Locks 接收 signal；存储租约等待可取消、超时不绕过活租约执行，心跳不会覆盖新 owner。覆盖持锁 stall、等待取消、无 Web Locks、旧响应及单飞。 |
| D8-1 | 已修复并真实验证 | `frontend/deploy/nginx.conf` 开启文本静态资源 gzip 与 Vary。新 `verify-nginx-runtime.mjs` 运行固定摘要生产镜像，以原始 HTTP 响应验证 gzip 解压结果、identity、Vary、PNG 不压缩、已压缩上游不二次压缩。 |
| D8-2 | 已修复当前公共请求入口 | 新 `engine/http/deadline.ts` 的有限预算覆盖排队、刷新、请求及响应体；不用较新的 `AbortSignal.any/timeout`。普通操作 60 秒，刷新与登出/停用整个流程 10 秒。包含 Catalog、通用 Action、生命周期、账户/管理请求、公开认证请求和门户请求。超时文案保留“结果尚未确认”，不自动重发结果未知的写操作。门户建议输入变更/卸载取消旧请求，查询也在会话/页面边界取消。 |
| D8-3 | 已修复 | 账户共用写入口、头像、会话列表和管理用户列表/签发重置凭证接入现有 401 续期。只重放鉴权先于副作用合同下的首次 401，第二次业务 401 原样返回，不误清会话。保留调用方取消。 |
| D8-4 | 已修复元数据丢失；保持既有信封合同 | 无效 JSON 使用 `ApiError` 保留状态码/request ID，不暴露网关 HTML/解析器摘录。204 空体的 JSON Action 继续拒绝，已有当前合同没有声明允许无体的操作。 |
| D8-5 | 已修复 | 瞬态恢复失败进入 `failed`，原路径显示明确的“重试恢复会话”按钮；终态 Cookie 失效才进入 anonymous。实际 UI 回归验证再次恢复。演示 E2E 明确 mock 无 Cookie 的 401，不再依赖无认证路由的 404。 |
| D8-6 | 已修复确定失败重试；保留身份隔离 | 默认 Query 不自动重试，由用户明确重试；mutation 原本无默认重试。保持含 token 的现有查询键，不冒险移除身份边界或统一业务 staleTime。实际 App 的 403 Catalog 请求只发一次。 |
| D8-7 | 条件性需求不扩展 | 现行生产无 redirect Action，不新增 JSON target 协议。opaqueredirect 继续明确展示浏览器未暴露目标且不导航；可见 Location 增加同源检查，拒绝外站/javascript。未手改生成契约。 |
| D8-8 | 边缘已修复并验证，后端由总协调验收 | 统一政策为业务单文件 16 MiB、multipart/HTTP 总体 17 MiB（1 MiB 封装预算），普通 JSON 保持 16 MiB。Nginx 和部署合同改为 17m，部署说明同步。真实固定镜像先复现 16m 对 16 MiB 文件加封装返回 413；修复后该文件请求与恰好 17 MiB 总体请求通过，17 MiB+1 字节返回 413，Content-Length/chunked 都通过。人工上游确认收到完整字节，未替代真实后端解析器/单文件/JSON 限额验证。 |
| D6-3 / S53 | 前端与边缘部分已修复 | 重置页优先读取 `#token=`，兼容旧 `?token=`，读取后 replace 清理凭据并保留其他 query。后端邮件 URL 改动由总协调处理。Nginx 安全日志只记 method/status/request ID/bytes/duration/upstream_status/upstream_response_time，不记录原始 URI/query/Referer；内建 error 日志没有字段脱敏能力，降到 crit。真实容器以 200/404/断连 502 验证上游状态和耗时，多次尝试的逗号分隔值保留；路径/query/Referer 哨兵均未出现在 stdout+stderr。 |
| 全量测试报告：关闭按钮 | 已修复 | 浏览器先复现 sticky header 截获 X 的点击，再为关闭按钮补 `z-20`；明暗主题均实际点击关闭。 |
| 全量测试报告：React render 更新 | 已修复 | DataGrid 不再在 React state updater 内调用父级 setter；先复现浏览器警告，再验证批量删除功能及 console 零该警告。 |
| 全量测试报告：视觉 | 已校准一个过时基线 | 实际比图确认明色差异来自已有 sticky header/footer、当前品牌和确定的种子数据；关闭按钮修复并验证后只更新 `action-dialog-light-chromium-win32.png`，其余五张保留。随后以 `--update-snapshots=none` 完整通过。 |

## 验证

- `pnpm --dir frontend check`：退出 0；Prettier、ESLint、TypeScript、80 文件/938 Vitest、locale、build、production-build、bundle、deployment-contract 全通过。首屏 JS gzip 274.7 kB。既有 Vite `advancedChunks` deprecation warning 仍存在。
- 开发 Chromium E2E 与本地视觉：23 passed，0 skipped；5320/18320 隔离端口，`--update-snapshots=none`。
- 生产构建 Chromium E2E：4 passed；5321/18321 隔离端口。为避免其他代理的 Cargo 构建互相锁等待，使用现有预编译 `frontend_demo.exe` 与临时 Playwright 配置；该结果只覆盖前端与演示合同，不能证明本轮后端新代码。
- `node frontend/scripts/verify-nginx-runtime.mjs`：固定摘要 Nginx 容器，gzip/identity/Vary/PNG/预压缩/日志脱敏/上游诊断/16 MiB 文件/17 MiB 总体边界均通过，覆盖 Content-Length 与 chunked。测试上游只处理人工字节，在本机临时监听以供 Docker Desktop 访问；finally 关闭监听并删除容器与测试临时目录。冻结后复验退出 0，最终日志：`target/infrastructure-remediation/verification/nginx-runtime-final.log`。
- D8-8 追加后定向验证：两个改动脚本的 ESLint、Prettier 与部署合同（含 17m 改回 16m 的自检变异）通过；此前 938 Vitest 与两套 Chromium 结果不作为本轮上传边界证据。
- 摘除式验证：22 项实际失败后恢复。包括 auth 代次、单飞、restore 成功/失败、终止会话代次、请求超时、调用方取消、预先取消、Web Locks signal、租约心跳、redirect 同源、Query 重试、恢复 UI、URL 清理、门户取消、gzip、访问日志、错误日志，以及追加的 17m 改回 16m、放宽到 18m、摘除 upstream_status、摘除 upstream_response_time。原始结果在被忽略的 `frontend/test-results/remediation-mutations/`。单独删 `query.delete` 会制造无限导航，该实验已停止并恢复，未计入通过的 22 项；替代实验摘除完整 URL 清理效果，两个回归正常变红。
- `git diff --check -- frontend` 通过。只改前端及本报告，未提交/push/部署，未修改 OpenAPI/API-types 生成物；自建演示后台进程已按 PID+路径核对后停止。

## 边界

追加审批任务状态同步：`review` 显示为“待人工核查”，保留未知状态原值展示；含义为外部写入结果未知或重试耗尽，不能当作确定失败，不新增重试入口或筛选功能。仅同步 `ApprovalRequestsPage.tsx`、业务类型注释及任务夹具。先以新夹具取得真实红测，再运行派发记录/审批控制台两个测试文件，16 项全部通过（单 worker）；日志 `target/infrastructure-remediation/review-label-red.log` 与 `review-label-green.log`。此追加不沿用此前 938 项及 Chromium 结果作为新状态的浏览器证据，生成契约由总协调统一导出。

没有测试生产、真实 TLS/飞书、Firefox/WebKit、浏览器冻结场景或真实多标签调度。localStorage 租约仍是尽力互斥，不能替代后端重放防御。60 秒前端预算不保证服务端在断连后撤销已发生的副作用；用户必须先检查结果再重试。会话代次保护证明的是本地状态与事件，HTTP Cookie/服务器撤销事实仍由服务器决定。Nginx 降低内建 error 日志级别牺牲了普通上游错误的原始详细日志；安全访问日志保留请求及上游状态、耗时与请求编号。边缘 request ID 未被本包新增透传，不能据此声称与后端 request ID 完全关联。
