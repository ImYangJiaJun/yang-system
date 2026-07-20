# yang-system-frontend

YANG 生态的契约驱动正式前端。整体视觉与 BR 生态保持一致：使用主色顶栏、模块入口、窄侧栏、标准 Quasar 卡片/列表和头像账号菜单；后端注册 View 后自动进入正式业务导航。原有 Action 演示、租户切换和真实调用能力完整保留在独立开发工作台中，复杂场景继续通过静态 `view_id` 注册表覆盖。

## 运行模式

| 路径         | 模式         | 用途                                                 |
| ------------ | ------------ | ---------------------------------------------------- |
| `/`          | 正式控制台   | 系统总览、身份状态和业务入口                         |
| `/business`  | 正式业务页面 | 只渲染当前身份可访问的 View，不暴露接口调试信息      |
| `/workbench` | 开发工作台   | 保留 Action 演示、TableView 验收、租户切换和响应调试 |
| `/login`     | 登录         | 建立正式控制台与工作台共用的会话                     |

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
├── layouts/                # 正式控制台与开发工作台两套隔离壳层
├── pages/                  # 首页、正式业务页和开发渲染器编排
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

`pnpm e2e` 会同时启动 `../examples/frontend_demo/`，覆盖正式控制台与开发工作台隔离、正式业务入口、默认接口演示、表格、关系选择、上传下载、确认操作和自定义 View。
