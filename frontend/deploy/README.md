# 前端生产部署契约

`nginx.conf` 是 YANG System SPA 的可审计完整 Nginx 配置，可直接安装为
`/etc/nginx/nginx.conf`。`deployment-contract.mjs` 是生产构建 E2E 使用的
同一响应合同。发布时必须把 `dist/spa` 挂载到 `/usr/share/nginx/html`，
Rust 服务监听 `127.0.0.1:8080`，Nginx 只监听 `127.0.0.1:8081`。

公网入口必须由受信任的 TLS 终止层提供 HTTPS，并满足以下前置条件：

- 只允许 TLS 1.2/1.3，HTTP 永久重定向到 HTTPS；
- 覆盖客户端传入的 `Forwarded`/`X-Forwarded-*`，再传给此应用边缘；
- 保留本配置的 CSP、HSTS 和其他安全响应头；
- 对 `/api`、`/.well-known`、`/health` 不做 SPA fallback 或 CDN HTML 缓存；
- 发布后以真实域名执行生产 E2E 的深链接、安全头、缓存和 404 smoke。

本配置将所有 HTML/非哈希路径设为 `no-store`，将 Vite 生成的 `/assets/`
命名空间设为一年 `immutable`。`/assets` 缺失文件严格返回 404；只有非后端、
非资产的前端路由才 fallback 到 `index.html`。

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
