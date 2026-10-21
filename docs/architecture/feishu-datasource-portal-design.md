# 飞书数据源查询门户设计文档

> 状态：草案 | 日期：2026-10-13 | 依赖：feishu datasource 模块已交付

## 1. 背景与目标

### 1.1 问题

飞书多维表格存储了大量信用卡、店铺等敏感信息，已通过 datasource 模块同步到系统。现在需要把其中某些表的能力**对外暴露为一个查询页面**，让用户通过一个精确值（如完整信用卡号）查到管理员指定的若干列。

### 1.2 核心约束

- 查询键**只接受精确匹配**，禁止模糊/范围/前缀搜索
- 返回哪些列由管理员白名单控制
- 门户的访问方式由管理员在创建时选配：公开 / 口令 / 登录
- 一个门户绑定一张已同步的表（`feishu_datasource` 的一行）
- 一个数据源可以有多个门户（不同查询键 / 不同开放列面向不同受众）

### 1.3 非目标

- 不做增量同步（门户数据 = datasource 已同步的数据，直接查本地表）
- 不做门户之间的组合查询
- 不做门户模板或批量创建
- 不替代现有的审批派发（`feishu.approval`）

---

## 2. 术语

| 术语 | 含义 |
|---|---|
| **门户（Portal）** | 一个可对外暴露的查询端点，绑定一个数据源 + 一个查询键 + 一组开放列 |
| **查询键（Query Key）** | 门户用来做精确匹配的字段，对应某个 `feishu_datasource_field` 绑定 |
| **开放列（Exposed Column）** | 管理员勾选的、允许在查询结果中展示的字段白名单 |
| **访问模式（Access Mode）** | `public`（任何人）/ `passphrase`（口令）/ `login`（系统登录） |

---

## 3. 数据模型

### 3.1 新表：`feishu_datasource_portal`

```
feishu_datasource_portal
├── id                  BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY
├── datasource_id       BIGINT UNSIGNED NOT NULL          -- FK → feishu_datasource.id
├── title               VARCHAR(100) NOT NULL             -- 门户名称（管理员用）
├── slug                VARCHAR(36) NOT NULL UNIQUE       -- URL 标识，UUID v4 无连字符
├── query_field_id      VARCHAR(64) NOT NULL              -- 查询键，指向 feishu_datasource_field.field_id
├── query_field_source_key VARCHAR(64) NOT NULL           -- 冗余存 source_key，方便查询
├── access_mode         VARCHAR(16) NOT NULL DEFAULT 'public'  -- public / passphrase / login
├── passphrase_hash     VARCHAR(255) NULL                 -- access_mode=passphrase 时存 argon2 哈希
├── exposed_columns     TEXT NOT NULL                     -- JSON 数组：开放列的 field_id 列表
├── result_labels       TEXT NULL                         -- JSON 对象：field_id → 展示名（管理员可自定义列标题）
├── empty_message       VARCHAR(500) NULL                 -- 查无结果时的提示文案（默认"未找到匹配记录"）
├── rate_limit_per_min  INT NOT NULL DEFAULT 30           -- 每分钟最大查询次数（按 IP）
├── enabled             TINYINT(1) NOT NULL DEFAULT 1     -- 启用/停用
├── created_at          DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
├── updated_at          DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP
└── INDEX idx_portal_datasource (datasource_id)
```

**设计决策**：

1. `slug` 用 UUID v4（32 字符十六进制），不可枚举。URL 形态 `/portal/<slug>`，不暴露自增 ID。
2. `query_field_id` 指向 `feishu_datasource_field.field_id`（改名不断链），同时冗余 `query_field_source_key` 避免查询时 JOIN。
3. `exposed_columns` 存 JSON 数组而非关联表。原因：列数少（通常 <20）、总是整组读写、不需要按列查询。
4. `passphrase_hash` 用 argon2，与项目已有密码哈希一致。
5. `rate_limit_per_min` 默认 30，可管理员调整。限流在 Redis 做滑动窗口。

### 3.2 新表：`feishu_datasource_portal_log`

```
feishu_datasource_portal_log
├── id                  BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY
├── portal_id           BIGINT UNSIGNED NOT NULL          -- FK → feishu_datasource_portal.id
├── query_value_hash    VARCHAR(64) NOT NULL              -- 查询值的 SHA-256（不存明文！）
├── result_count        INT NOT NULL DEFAULT 0            -- 返回行数
├── ip_address          VARCHAR(45) NULL                  -- 来源 IP（IPv6 兼容）
├── user_agent          VARCHAR(500) NULL
├── access_mode_used    VARCHAR(16) NOT NULL              -- 本次用的访问模式
├── status              VARCHAR(16) NOT NULL              -- hit / miss / rate_limited / forbidden
├── created_at          DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
└── INDEX idx_portal_log_portal (portal_id, created_at)
```

**设计决策**：

1. `query_value_hash` 存 SHA-256 而非明文。信用卡号等高敏感值不落日志明文，但可以事后审计"某个卡号是否被查过"。
2. 日志表无更新操作，只有 INSERT，append-only。

---

## 4. 后端 API

### 4.1 管理员侧（需认证 + 权限）

权限键：`feishu.portal.read` / `feishu.portal.write`（纳入管理员等价清单，因为门户可以暴露任意数据）。

| Action | 方法 / 路径 | 说明 |
|---|---|---|
| `list_portals` | POST `/api/v1/feishu/portals/query` | 列出门户（支持按 datasource_id 过滤） |
| `create_portal` | POST `/api/v1/feishu/portals` | 创建门户 |
| `update_portal` | PUT `/api/v1/feishu/portals` | 更新门户配置 |
| `delete_portal` | DELETE `/api/v1/feishu/portals` | 删除门户（日志保留） |
| `toggle_portal` | PUT `/api/v1/feishu/portals/toggle` | 启用/停用 |
| `portal_access_log` | POST `/api/v1/feishu/portals/log` | 查看访问日志 |
| `set_passphrase` | PUT `/api/v1/feishu/portals/passphrase` | 设置/更换口令（仅 access_mode=passphrase） |

**创建门户的输入契约**：

```rust
struct CreatePortalInput {
    datasource_id: i64,
    title: String,                              // ≤100
    query_field_id: String,                     // 必须是该数据源已绑定的 field_id
    exposed_columns: Vec<String>,               // field_id 列表，必须都是该数据源已绑定的
    access_mode: AccessMode,                    // public / passphrase / login
    passphrase: Option<String>,                 // access_mode=passphrase 时必填，≥8 字符
    result_labels: Option<HashMap<String, String>>,  // field_id → 展示名
    empty_message: Option<String>,              // ≤500
    rate_limit_per_min: Option<u32>,            // 默认 30，上限 120
}
```

**校验规则**：
- `datasource_id` 必须存在且 `status = active`
- `query_field_id` 必须是该数据源的已绑定字段（存在于 `feishu_datasource_field`）
- `exposed_columns` 每一项都必须是该数据源的已绑定字段
- `query_field_id` 不能出现在 `exposed_columns` 中（查询键本身不展示——如卡号只用来查，不展示）
- `access_mode = passphrase` 时 `passphrase` 必填且 ≥8 字符

### 4.2 用户侧（公开端点）

| 端点 | 方法 / 路径 | 认证 | 说明 |
|---|---|---|---|
| `portal_query` | POST `/api/v1/feishu/portal/:slug/query` | **public** | 执行查询 |

**输入**：

```rust
struct PortalQueryInput {
    value: String,           // 查询值（精确匹配）
    passphrase: Option<String>,  // access_mode=passphrase 时必填
}
```

**输出**：

```rust
struct PortalQueryOutput {
    found: bool,
    columns: Vec<PortalColumn>,   // 开放列定义（标题 + 类型）
    rows: Vec<PortalRow>,         // 匹配行（仅开放列的值）
    message: Option<String>,      // 未找到时的提示
}

struct PortalColumn {
    field_id: String,
    label: String,                // 管理员配置的展示名或默认 field_name
    field_type: String,           // text / number / date / single_select / ...
}

struct PortalRow {
    values: HashMap<String, serde_json::Value>,  // field_id → value
}
```

**安全机制**：

1. **精确匹配强制**：SQL 查询用 `WHERE query_column = ?`，永远不生成 LIKE / BETWEEN / IN (子查询)。
2. **速率限制**：Redis 滑动窗口，key = `portal:query:{slug}:{ip}`，超限返回 429。
3. **口令验证**：argon2 比对，失败返回 403（与无权限统一，不泄露是否存在口令）。
4. **slug 不可枚举**：UUID v4，64 位熵以下暴力不可行。
5. **查询值不落明文**：日志只存 SHA-256。
6. **无分页**：精确匹配通常返回 0 或 1 行；如匹配多行，上限 100 行，超过截断并在 `message` 中提示。

---

## 5. 前端

### 5.1 公开门户页面

**路由**：`/portal/:slug`

**关键突破**：这是项目里第一个**不在 `RequireAuth` 门控内**的页面。在 `routes.tsx` 中新增一条**不在 `<RequireAuth>` 包裹的 `/` 路由下**的顶级路由：

```
├── /portal/:slug   → <PortalQueryPage/>    # 公开，不走 AppLayout
```

**页面布局**（极简）：

```
┌─────────────────────────────────┐
│  [门户标题]                      │
│                                 │
│  ┌─────────────────────────┐   │
│  │ 请输入完整卡号           │   │
│  └─────────────────────────┘   │
│  [    查  询    ]               │
│                                 │
│  ┌─────────────────────────────┐
│  │ 卡所属店铺 │ 额度 │ 到期日  │  ← 仅开放列
│  ├────────────┼──────┼─────────┤
│  │ XX旗舰店   │ 50000│ 12/28   │
│  └─────────────────────────────┘
│                                 │
│  未找到匹配记录                  │  ← 查无结果时
└─────────────────────────────────┘
```

**交互细节**：
- `access_mode = passphrase` 时，输入框上方多一个口令输入框
- `access_mode = login` 时，页面检测未登录则重定向到 `/login?redirect=/portal/:slug`
- 查询按钮在请求中 / 无输入时 disabled
- 结果表格列由后端 `columns` 动态决定（不硬编码）
- 底部显示门户的 `empty_message`（查无结果时）

### 5.2 管理侧门户配置

**入口**：在现有飞书数据源详情页（`DatasourceDetailPage`）新增一个 Tab ——"查询门户"。

**管理页面功能**：
- 门户列表（该数据源下的所有门户）
- 创建门户向导：选查询键 → 勾选开放列 → 选访问模式 → 预览 URL → 确认
- 编辑门户（改标题 / 开放列 / 访问模式 / 口令 / 限速）
- 启用 / 停用
- 访问日志查看（时间 / IP / 查询值哈希 / 结果数 / 状态）
- 复制门户 URL

**前端新增文件**：
- `frontend/src/features/feishu/views/PortalConfigPanel.tsx` — 管理面板
- `frontend/src/features/feishu/portal-api.ts` — 门户 API 调用
- `frontend/src/features/feishu/views/PortalQueryPage.tsx` — 公开查询页
- `frontend/src/features/feishu/types.ts` — 新增门户相关类型

**路由注册**：`routes.tsx` 新增 `/portal/:slug`，`registry.ts` 不需要注册（公开页不是自定义视图，是独立路由）。

---

## 6. 安全模型

### 6.1 威胁分析

| 威胁 | 缓解 |
|---|---|
| 枚举卡号（暴力遍历） | 速率限制 + 精确匹配（不接受前缀） + 日志审计 |
| 门户 URL 被猜到 | UUID slug，不可枚举 |
| 查询值泄露（日志） | 日志只存 SHA-256，不存明文 |
| 过度暴露数据 | 白名单机制：只展示管理员勾选的列 |
| 未授权访问门户 | 三种访问模式可选；login 模式复用系统认证 |
| 口令泄露 | argon2 哈希存储；可随时更换 |
| 查询键也被暴露 | 强制：查询键不出现在结果列中 |

### 6.2 审计

- 每次查询（无论命中与否）都写 `feishu_datasource_portal_log`
- 管理操作（创建/修改/删除门户）走标准审计（`audit` 基础设施）
- 日志保留期：建议 90 天，可后续加清理 worker

### 6.3 与现有安全体系的关系

- 管理侧 Action 走框架 JWT 鉴权 + `feishu.portal.read/write` 权限
- 管理侧权限纳入管理员等价清单（`sensitive_permissions.rs`）
- 公开端点 `portal_query` 注册为 `public`，与 `approval_options` 同级别
- 公开端点额外走速率限制中间件（需新增或复用现有 `yang_base` 限流能力）

---

## 7. 实施分期

### Phase 1：核心闭环（建议先做）

- 数据模型 + Schema 声明
- 管理员 CRUD Action（list / create / update / delete / toggle）
- 公开查询端点（精确匹配 + 速率限制 + 日志）
- 前端：公开查询页（支持 public 模式）
- 前端：管理侧配置面板（在数据源详情页新增 Tab）

### Phase 2：访问控制补全

- 口令模式（passphrase）
- 登录模式（login）
- 管理侧访问日志查看页

### Phase 3：运维增强（可选）

- 查询统计仪表板（命中/未命中趋势）
- 日志自动清理 worker
- 门户模板（同一张表的不同门户快速复制）

---

## 8. 开放决策点

| 编号 | 问题 | 建议默认值 |
|---|---|---|
| D-1 | 查询键是否允许出现在结果列中？ | 不允许（安全优先） |
| D-2 | 门户 URL 是否需要自定义短链能力？ | 不需要（UUID slug 足够） |
| D-3 | 查询结果是否需要支持导出？ | Phase 1 不支持 |
| D-4 | 一个门户是否支持多查询键（AND 组合）？ | 当前只支持单查询键 |
| D-5 | `passphrase` 是否需要有效期？ | Phase 1 不需要，可随时更换 |

---

## 9. 文件清单预估

### 后端新增

```
src/addon/feishu/portal/
├── mod.rs              # module 装配
├── table.rs            # feishu_datasource_portal 表声明
├── log_table.rs        # feishu_datasource_portal_log 表声明
└── actions/
    ├── mod.rs
    ├── list_portals.rs
    ├── create_portal.rs
    ├── update_portal.rs
    ├── delete_portal.rs
    ├── toggle_portal.rs
    ├── portal_query.rs         # 公开端点
    ├── portal_access_log.rs
    └── set_passphrase.rs
```

### 前端新增

```
frontend/src/features/feishu/
├── portal-api.ts
├── views/
│   ├── PortalConfigPanel.tsx    # 管理面板
│   ├── PortalEditDialog.tsx     # 创建/编辑对话框
│   └── PortalQueryPage.tsx      # 公开查询页
```

### 需修改的现有文件

- `src/addon/feishu/mod.rs` — 注册 portal module
- `src/addon/feishu/domain/context.rs` — FeishuContext 增加 portal repository
- `src/addon/access/domain/sensitive_permissions.rs` — 新增管理员等价权限
- `frontend/src/shell/routes.tsx` — 新增 `/portal/:slug` 路由
- `frontend/src/features/feishu/views/DatasourceDetailPage.tsx` — 新增门户 Tab
- `src/infrastructure/schema.rs` — 新表声明（通过 table.rs 自动）
