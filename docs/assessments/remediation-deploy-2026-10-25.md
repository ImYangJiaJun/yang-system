# 部署与恢复修复记录（2026-10-25）

范围：E01–E09 与 D2/S 中部署、恢复相关项。仅本机源码、命令替身和临时文件验证，不访问生产 SSH、配置或数据库。CodeGraph 未索引 shell，改用直接读取脚本及调用点。

## 实施前触点与验收

| 触点 | 预期验收 |
|---|---|
| `probe_code` / `wait_ready` / `health_check` 全部发布、回滚、refresh 调用 | curl rc 非零即失败；完整 readiness JSON 与边缘同时通过 |
| `stop_pair` / `stop_all_pairs` / `reset_database` | 停止失败或状态未知，不得进入 DROP；停止预算覆盖应用关闭预算 |
| `main` / `cmd_smoke` / `cmd_cutover` / `cmd_rollback` | 操作互斥；冒烟 ID 与配置版本不变；任一切流量步骤失败恢复旧容器；配置随版本回滚 |
| 配置上传、安装、备份 | 创建即私有，权限失败中止；容器实际 UID 可读，禁止 root 读取替代验证 |
| `load_mysql_env` 与 PowerShell 预检 | 假密码不得出现在错误输出 |
| 新备份入口与恢复手册 | dump/压缩/加密/异地上传任一步失败不得发布 last-success；恢复先隔离出网与 worker |
| TLS 资产与交付文档 | 明确宿主/容器网络拓扑，提供可验模板；现场证书、网关、资源容量标未知 |

不更改保留期、不猜资源容量、不自动签证书。Schema 锁内策略由 root 处理；只读 smoke 启动接口与生命周期修复另见 `remediation-runtime-2026-10-25.md`。

## 状态与验证

| ID / 步骤 | 当前状态与边界 |
|---|---|
| E01 | 上传创建即私有目录、最终安装受同一发布锁保护；正式配置 0600 + UID 10001 读 ACL，临时无网络同 UID 容器预检；chmod/ACL 失败即停。真实 userns/ACL 未验 |
| E02 | curl 非零失败、readiness JSON 语义验证及边缘双检查已修。readiness 与 edge 共享 60 秒总 deadline，curl 上限取 5 秒与剩余预算中的较小值；真实网络黑洞未演练 |
| E03 | stop 失败直接中止，DROP 前再次核对 live/green/blue 后端状态；无法证明所有外部写者均停，维护闸门仍属现场前置 |
| E04 / S56 / S75 | 操作与文件安装互斥；发布快照冻结两镜像 ID/config SHA；cutover 按 green ID；本机 build 拒绝脏两仓并在构建后复核，镜像标记两仓 revision/local-build。尚未绑定 CI 制品证明 |
| E05 | blue/live 配置快照、原容器 ID、回滚绑定预检与配置恢复已实现；切流量失败保留恢复材料，不跨未知 schema 自动启动旧版。人工兼容性声明不是锁内证明 |
| E06 | 可执行 backup-mysql.sh 显式转发凭据、pipefail、dump 完成/表定义检查、CMS 公钥加密、异机摘要确认后才更新 last-success。真实备份账号、off-host 和数据库恢复未验 |
| E07 / S70 / S71 / S74 | 恢复资产/密钥/镜像/网关清单、Redis 安全状态、旧 OAuth 凭据失效、隔离出网/worker 和 RPO 边界已更正；没有宣称具备生产恢复点 |
| E08 / D2-1 / D2-11 / S30–32 | 宿主 nginx TLS 模板与证书/网关 netns/Origin/续期验收已交付，本地 nginx -t 通过；shared-gateway bridge 与宿主 loopback 不可达明确阻断，现场拓扑仍待批准落实 |
| E09 | 非法 URL、占位行、DDL 驱动回显均不输出秘密；TLS 模板 access log 不含 query。前端 nginx 由另一代理负责 |
| D2-3 / S33 | 延用受控一次 frontend restart，已删除“恢复即可证明 netns 根因”的错误断言。长期 watchdog/现场 netns 故障演练未完成 |
| D2-5 | Docker create/stop 预算统一 310s，依据应用 total_timeout_seconds 验证上限 300s；未在真实长请求下演练 |
| D2-7 / S57 / S59 | compose Redis/资源与日志参数由 root 接手，未在此包擅定容量 |
| D2-8 | stop/rename/run 失败通过 EXIT 路径保留原容器身份、停止失败的新对并非零报告；完整自动安全回滚待 schema proof 接口 |
| D2-10 | production/缺省环境在修改前拒绝非 IPv4 loopback；覆盖 0.0.0.0、::、具体公网 IP。IPv6 发布尚未支持，拒绝并说明限制 |

本轮定向验证：`python scripts/test_deploy_safety.py` 32 项通过；`--mutations` 16 个关键故障变异全部杀死；`python scripts/check_deploy_syntax.py` 通过。变异仅作用于临时副本，未修改共享源码；尚未为每一个新增错误出口单独制作摘除变异，不能将 16 个样本表述为全分支变异覆盖。

临时自签证书 + 本地前端镜像 `sha256:8708bc4f01c7d97a2a16a4f8df2a4976e370b09b058d2890e69e7f0b5e7ab632`，`--network none` 的 nginx -t 通过；真实 OpenSSL CMS 公钥加密、格式验证、解密后字节一致。未使用真实凭据/生产网络。

root 接口：将 `scripts/test_deploy_safety.py` 纳入 full 门禁；`.releases/<generation>/schema-validation` 当前明确写 unknown，需后续锁内 schema policy 的实际执行证明接入。恢复初检可使用 `--smoke-check` 的只读 schema/依赖入口；它不提供业务 HTTP，正常模式的全部 worker 仍需网络隔离与独立业务验收。`ROLLBACK_SCHEMA_COMPATIBLE=yes` / PowerShell `-RollbackSchemaCompatible` 只是操作人声明，不能替代证明。

没有 commit、push、部署、生产 SSH、清库；未修改保留期或自动申请证书。
