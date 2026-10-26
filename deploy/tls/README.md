# TLS 上线前置与验收

`nginx.host.conf` 是**宿主机 nginx** 的完整配置模板，代理宿主 loopback `127.0.0.1:18654`。它默认拒绝 HTTP，并覆盖客户端携带的代理头；访问日志只记录 path，不记录 query。模板不申请证书、不修改防火墙，也不代表当前生产已启用 TLS。

独立 bridge 网络中的 `shared-gateway` 不能把 `127.0.0.1:18654` 当宿主地址，`host-gateway` 也无法自动访问只绑定宿主 loopback 的入口。选择宿主 nginx（或经审查的 Linux host-network 网关）才可直接使用此模板。若保留 bridge 网关，必须另行设计受控入口网络；不要为图省事直接接入同时能访问 MySQL/Redis 的应用网络。

部署前逐项完成：

1. 确认公网入口、证书 SAN（域名或 IP）、客户端信任链、端口和真实网关 netns。证书与私钥从独立密钥托管恢复到模板指定路径；私钥仅网关管理者可读。域名、443 并非 TLS 协议硬前置，修改端口后必须同步 Origin/OAuth 回调。
2. 使用网关**实际版本**运行 `nginx -t -c /path/nginx.host.conf`。模板会与既有虚拟主机冲突时，由运维合入现有配置，不能覆盖整份共享网关配置。
3. 应用边缘设为 loopback，管理面继续独立 loopback。切换后使用 production 配置，核对 Secure Cookie、密码重置链接、OAuth redirect、Host/Origin；旧 HTTP 链接全部更新。HTTP 重定向无法保护客户端第一次已发送的密码或 token。
4. 从真实网关 netns 请求应用主页、受保护 API、飞书 callback/dispatch（只用无效测试凭据观察预期鉴权信封）；再从外网完成 TLS 信任链与业务行为验收。宿主 curl 成功不能代替网关 netns 验证。伪造 Forwarded/XFF 必须被最外层覆盖；下游可信 CIDR 只涵盖实际代理。
5. 配置证书续期任务及失败告警。续期产物先 `nginx -t`，成功后才 reload；至少每日用 `openssl x509 -checkend 2592000 -noout -in /etc/yang-system/tls/fullchain.pem` 检查 30 天窗口，并用 `openssl s_client -connect <入口>:443 -servername <SAN> -verify_return_error` 检查在线证书链。告警投递与续期权限必须在现场验证。
6. 回滚优先保留 TLS，只回滚应用版本和匹配配置。不得为恢复应用而自动重新开放明文入口。网关配置、证书恢复资料与发布 manifest 一并纳入独立备份。

本地验证可以生成一次性自签证书，仅运行 `nginx -t`；这只证明语法和文件读取，不证明受信证书、续期、公网路由、真实代理链或飞书兼容性。
