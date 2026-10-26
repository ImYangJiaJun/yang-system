# 前端生产部署契约

`nginx.conf` 是 YANG System SPA 的可审计完整 Nginx 配置，可直接安装为
`/etc/nginx/nginx.conf`。`deployment-contract.mjs` 是生产构建 E2E 使用的
同一响应合同。发布时必须把 `dist` 挂载到 `/usr/share/nginx/html`，
Rust 服务监听 `127.0.0.1:8080`，Nginx 监听 `8081`（所有网卡）。

> **2026-09-22 起监听地址由 `127.0.0.1:8081` 改为 `8081`。**
>
> 原设计只绑 loopback，理由是「应用边缘必须躲在受信 TLS 边缘之后」。但容器编排用
> `docker -p` 发布端口时，DNAT 会把流量送到 netns 的 eth0，而 loopback 绑定收不到
> ——「只听 loopback」与「用 `-p` 发布端口」在技术上互斥。
>
> 「不暴露公网」的责任因此**移到编排层**，规则是：
> **发布端口必须写 `-p 127.0.0.1:<host_port>:8081` 或 `-p ${BIND_ADDR}:<host_port>:8081`
> （`BIND_ADDR` 默认 `127.0.0.1`），绝不写裸端口 `-p <host_port>:8081`、也绝不硬编码 `0.0.0.0`。**
> 这条由 `scripts/verify-deployment-contract.mjs` 机械校验（它会读
> `../../deploy/deploy-blue-green.sh` 并检查每一个发布 8081 的 `-p`）；监听 80/443 仍被禁止。
>
> ⚠️ **门禁的边界**：它只证明**源码里的默认值**是 loopback（即「默认不暴露」），
> **不检查运行时环境变量**——`BIND_ADDR=0.0.0.0` 是运维可显式使用的覆盖手段
> （`deploy.ps1 -EdgeBindAddr 0.0.0.0` 即此）。所以它不构成对暴露面的保证。
>
> 仍然必须与后端容器**共享网络命名空间**运行——nginx 的 upstream 写死 `127.0.0.1:8080`。

公网入口必须由受信任的 TLS 终止层提供 HTTPS，并满足以下前置条件：

- 只允许 TLS 1.2/1.3，HTTP 永久重定向到 HTTPS；
- 覆盖客户端传入的 `Forwarded`/`X-Forwarded-*`，再传给此应用边缘；
- 保留本配置的 CSP、HSTS 和其他安全响应头；
- 对 `/api`、`/.well-known`、`/health` 不做 SPA fallback 或 CDN HTML 缓存；
- 发布后以真实域名执行生产 E2E 的深链接、安全头、缓存和 404 smoke。

本配置将所有 HTML/非哈希路径设为 `no-store`，将 Vite 生成的 `/assets/`
命名空间设为一年 `immutable`。`/assets` 缺失文件严格返回 404；只有非后端、
非资产的前端路由才 fallback 到 `index.html`。

上传采用分层限额：业务单文件最多 **16 MiB**（16,777,216 字节），
multipart/HTTP 整体最多 **17 MiB**（17,825,792 字节），为封装与其他字段
预留最多 1 MiB；因此 Nginx 配置为 `client_max_body_size 17m`。
普通 JSON 仍由后端独立限制为 16 MiB。边缘只限制请求总字节，不能替代
后端的单文件与 JSON 检查。`node frontend/scripts/verify-nginx-runtime.mjs`
在固定摘要生产镜像验证 16 MiB 文件加封装、恰好 17 MiB 与超出 1 字节，
同时覆盖 Content-Length 与 chunked 传输；该脚本需 Docker，人工上游仅用于
确认边缘完整转发字节，不代表真实业务上传或后端解析器验收。

文本资源开启 gzip 与 `Vary: Accept-Encoding`，PNG 等已压缩格式不重复压缩。
访问日志仅记录 method、status、request ID、响应字节、请求耗时，以及
`upstream_status`、`upstream_response_time`，不记录路径、query 或 Referer。
上游状态与耗时支持定位 502 等故障；多次尝试时保留 Nginx 的逗号分隔值。
内建 error 日志无法按字段脱敏原始请求行，因此使用 `crit`，普通故障的
原始文本详情不再保留；诊断应结合安全访问日志和后端日志。

HSTS 只有经 HTTPS 返回时浏览器才会执行。本地 HTTP E2E 验证的是响应合同，
不等同于已证明某个公网域名的 TLS、证书、DNS 或边缘配置。首次真实发布仍须
取得目标环境 smoke 的终态证据。

仓库门禁同时执行合同变异测试、生产构建浏览器 smoke，并在 CI 中使用固定版本
的 Nginx 官方镜像真实加载此配置。真实环境必须让 TLS 边缘设置可信的
`X-Forwarded-Proto`；应用边缘只接受 `http`/`https` 两个值，其他输入按内部
连接协议降级。

配置语义以 Nginx 官方的
[`add_header`](https://nginx.org/en/docs/http/ngx_http_headers_module.html)、
[`proxy_set_header`](https://nginx.org/en/docs/http/ngx_http_proxy_module.html)
和 [`try_files`](https://nginx.org/en/docs/http/ngx_http_core_module.html#try_files)
文档为准。所有 `add_header` 都放在 `server` 层，子 `location` 不声明新的
`add_header`，从而保留官方定义的继承行为。
