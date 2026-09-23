# 勘误：CONTRACT.md 的四处事实错误 + 三个必须补的状态

这些是**后续核实**推翻 `CONTRACT.md` 的地方，锚点都在后端源码里。
四个原型都是照旧契约生成的，因此**全部带有这些错误**，必须逐条修。

---

## E1 · `默认语言` 的取值域是 `zh_cn / en_us / ja_jp`，不是 `zh-CN / en-US`

- `src/addon/feishu/datasource/table.rs:45` 默认值是 `zh_cn`
- `src/addon/feishu/domain/i18n.rs:13` `DEFAULT_LOCALE = "zh_cn"`
- `src/addon/feishu/domain/protocol.rs:50-52` 明写这三项词表

**为什么必须改**：这不是显示问题。`i18n.rs:59-72` 的 `build_result_body` 把
`default_locale` **原样当作回给飞书的 `locale`**，而 `create_datasource` /
`update_datasource` 对它**没有任何校验**（只校验 source_key / title / token / status）。
前端写成 `zh-CN` 会把这个非法值写进库，并在飞行链路里原样回给飞书。

**怎么改**：
- 所有展示处 `zh-CN` → `zh_cn`，`en-US` → `en_us`（有 `ja_jp` 同理）。
- 表单里做成**三选一**，不是自由文本。下拉的 option value 用 `zh_cn` / `en_us` / `ja_jp`，
  显示文字用「简体中文 / English / 日本語」。
- 计数（生成后自查）：A 21+14、B 26+18、C 21+14、D 19+9 处。

## E2 · 「加密存储」这个名字是错的，真名是「加密返回」

- `src/addon/feishu/datasource/table.rs:42` 字段 title 就是「加密返回」
- 它加密的是**返回给飞书的选项信封**（`approval_options.rs` 的 `FeishuEnvelope::encrypted`），
  需要服务端配置 `feishu.encryption_key`
- Token 无论如何**只存 SHA-256 摘要**（`table.rs:33-39`），跟这个开关毫无关系

**为什么必须改**：现在的文案（如「开启后 Token 以密文保存」）会教给评审一个
**与安全相关的错误结论**——让人以为关掉它 Token 就明文存储。

**怎么改**：
- 标签改为「加密返回」。
- 说明改为「返回给飞书的选项内容加密传输；需服务端已配置加密密钥」。
- **并且**：开关开启但服务端没配密钥时，飞书侧会直接拿到 `50002 服务端未配置加密密钥`。
  把这个真实代价写成一行提示文字（不是 tooltip），因为它是这个开关的核心取舍。

## E3 · `数据源标识` 不是「1–64 字符」

`src/addon/feishu/datasource/actions/create_datasource.rs:52-58`：

- 必须**小写字母开头**
- 只含 `[a-z0-9_]`
- ≤ 64 **字节**

**怎么改**：表单里该字段的帮助文字改成「小写字母开头，只能包含小写字母、数字与下划线，最长 64 字节」，
并在输入框上体现（例如输入时把非法字符标红，或直接在 `pattern` 里体现）。
因为它会进接口 URL，这一点必须让人看见。

## E4 · 错误文案里有两句是编的

真实原文（其余保留）：

| 场景 | 真实文案 | 出处 |
|---|---|---|
| 名称长度 | `名称必须在 1..=100 字符` | `create_datasource.rs:66` |
| 状态取值 | `status 只能是 active 或 disabled` | `update_datasource.rs:80` |

契约里原来的「title 长度必须在 1..=100」「status 只接受 "active" 或 "disabled"」是编的。
错误态是最能暴露信息架构差别的一屏，用假文案等于让四个方案在同一段假话上比。

---

## E5 · 状态筛选（方案 B / D 的核心卖点）当前服务端做不到

`src/addon/feishu/datasource/table.rs:46-51` 的 `status` 字段是
`Radio::new().title().require().varchar().options().default()`——**没有 `.filterable(true)`**。

框架对筛选是 fail-closed（`crates/yang-base/src/table/table_query/validation.rs:135-143`）：
字段没声明 filterable 就直接 `FieldPermissionDenied("字段不允许筛选")`。

**怎么改**（保持 UI 不变，但把代价说清楚）：
- 保留那个「全部 / 启用 / 停用」分段控件（它的 UX 是对的）。
- 在原型底部的「已知实现约束」清单里写明：
  「列表按状态筛选需后端给 `status` 加 `.filterable(true)`（设计文档三处后端改动之外的第四处）。」
- **顺带**：`title` 只声明了 `.searchable(true)`（能搜、**不能筛、不能排**）；
  `feishu_datasource` 里只有 `source_key` 是 `.sortable(true)` 的。
  所以列头**不要**画「按名称排序」的箭头。
- 还有一条更隐蔽的：`list_datasources` **没有默认排序兜底**
  （`list_datasources.rs:74-76` 只遍历传入的 `order_by`；对比 `list_options.rs:62-72` 有回退）。
  前端不发 `order_by` 就是**无序分页，翻页会重复/漏行**。这一条也要进约束清单。

> 注意不对称：详情页的选项列表**可以**按 `enabled` 筛（`option/table.rs:53-57` 声明了 filterable）。
> 所以「列表页筛选是假的、详情页筛选是真的」——两边画一样的筛选器会误导。

---

## E6 · 必须补的三个状态

### E6-a 详情页 **0 选项**（最优先）

这是最真实的首次体验：数据源刚建好、多维表格的自动化还没推过任何选项。
而它恰恰是「**链路到底通没通**」最需要回答的一屏。

四个原型现在都画了 8 条选项。请加一个方式到达「有数据源、但该数据源一个选项都没有」：
详情区显示一个明确的空态，说明「还没有选项推过来」，并指出下一步去哪做
（多维表格的自动化），**不要**只写「暂无数据」。

### E6-b 页面级 403

权限是**三粒独立位**：`feishu.datasource.read`、`feishu.datasource.write`、`feishu.option.read`
（分别在 `list_datasources.rs:41`、`create_datasource.rs:84`、`list_options.rs:24`）。

于是存在一个「两角色模型」盖掉的组合：**有 `datasource.read`、没有 `option.read`**
→ 卡片列表正常，点进详情**全部 403**。请画出这个状态：
详情区显示一条说明「你没有查看选项的权限」+ 重试，而**不是**让页面白屏或显示空列表。

### E6-c 停用确认对话框

§5.4 明确有「停用」这一步，但契约的必画清单只列了删除确认。
真实后果：数据源 `status != active` 时，飞书审批拉选项直接拿到
`40301 数据源已停用`（`approval_options.rs:64-76, 168`）——
也就是说**正在被人使用的审批控件会当场取不到选项**。

确认文案请写明这一点，例如：「停用后，飞书审批中正在使用该数据源的控件会立即取不到选项。」
而**删除**确认用后端原文（CONTRACT §6.4），不要改写。两者要说得出区别。

---

## E7 · 底部「原型控制」条里请增加一个「已知实现约束」折叠区

四个原型都要有，内容按各自方案裁剪，至少包含它自己踩到的：

1. 列表按状态筛选需后端给 `status` 加 `.filterable(true)`（目前服务端会拒绝）。
2. 列表必须由前端**显式传 `order_by`**，否则无序分页会重复/漏行。
3. 没有任何 Action 能按 `source_key` 取**单个**数据源 → 详情页头部要么再发一次
   带 `where` 的 `list_datasources`，要么靠导航态传递（刷新/分享链接会丢）。
4. 「校验数据」按钮**控制台做不到**：唯一能校验的是 `approval_options`，
   它要拿数据源 Token 自校验，而服务端永远回不出明文 Token。
5. 卡片上**不能**显示选项数量 / 最后同步时间（后端不返回）。
   若要显示，需给 `list_datasources` 加字段（或 `option` 表补 `updated_at` select）。
6. 角色判断走 `catalog.actions` 里有没有对应 operation_id（**目录本身已按身份投影**：
   `registry.rs:245,256` 的 `policy.allows(context)`），不需要前端解析 JWT。

---

## E8 · 措辞：两个层级的「停用」不是一回事

- `feishu_datasource.status`（数据源级）：整个控件取不到任何选项
- `feishu_option.enabled`（选项级）：只影响那一条，注释明写「禁用而非删除，
  避免历史审批单引用的选项彻底失联」（`option/table.rs:53-57`）

而 `list_options` **不过滤数据源状态** → 打开一个已停用的数据源，
会看到「已停用的数据源」配着一排「启用」的选项行。

**怎么改**：详情页顶部用一条说明同时讲清两级；或用不同的词
（数据源=「已停用」，选项=「已下线」）。四个原型现在都用同一个「启用/停用」词，
评审会以为它们是一回事。
