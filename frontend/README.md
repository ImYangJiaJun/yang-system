# yang-system-frontend

YANG 生态的契约驱动前端。后端注册 Action 和 View 后，前端默认提供接口演示与通用业务页面；复杂场景通过静态 `view_id` 注册表覆盖。

## 目录边界

```text
src/
├── App.vue                 # 仅保留应用级 Router 出口
├── api/                    # HTTP 调用、响应通道和目录缓存
├── components/
│   ├── action/             # Action 默认演示器
│   ├── form/               # JSON Schema 与关系字段表单
│   └── table/              # 通用 TableView
├── contracts/              # 后端 UI Catalog 的运行时校验与类型
├── css/                    # 全局样式
├── custom/
│   ├── registry.ts         # view_id 静态白名单唯一入口
│   └── views/              # 自定义页面实现
├── layouts/                # Quasar 应用外壳、会话栏和目录导航
├── pages/                  # 路由页面与渲染器编排
├── router/                 # Vue Router 配置
└── stores/                 # Pinia 请求级目录与会话状态
```

依赖方向保持单向：`layouts/pages -> stores/components -> api/contracts`。`custom` 可以调用公开的 `api/contracts`，但后端返回的字符串不能直接拼接动态 import 路径。

## 本地开发

```powershell
pnpm install
pnpm dev
```

默认将 `/api`、`/.well-known` 和 `/health` 代理到 `http://127.0.0.1:8080`。需要连接其他后端时设置：

```powershell
$env:VITE_PROXY_TARGET = "http://127.0.0.1:18080"
pnpm dev
```

## 验证

```powershell
pnpm check
pnpm e2e
```

`pnpm e2e` 会同时启动 `../src/bin/frontend_demo.rs`，覆盖默认接口演示、表格、关系选择、上传下载、确认操作和自定义 View。
