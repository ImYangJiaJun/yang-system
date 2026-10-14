# API 创建审批实例时，关联外部数据源的单选/多选控件回读为空白

**缺陷报告 · 审批开放接口**

同一份审批定义、同一组选项取值，客户端提单能把选项文案写进实例，API 提单不能。外部数据源接口经核实从未失败。

| | |
|---|---|
| 审批定义 | `D0557DA6-CC9A-4B6B-BDF2-8DC675D2DD6E`（往来付款类型 / Current Payment Type_测试） |
| 相关接口 | `POST /open-apis/approval/v4/instances`、`GET /open-apis/approval/v4/instances/:instance_id` |
| 涉及控件 | `widget17902293404030001` 办公地点（单选，关联外部数据源）、`widget17902293451560001` 公司名称（单选，关联外部数据源） |
| 当场时间 | 2026-09-28 16:40–16:47（北京时间，UTC+8） |
| 根因 | **V2 外部数据源**（`externalDataSearchable=true`，即开启了模糊/分页搜索）暂未对开放平台做完整支持。V1 数据源正常。 |
| 影响 | 付款类审批的审批人在详情页看不到收款主体，无法据以判断 |

## 现象

通过 `POST /open-apis/approval/v4/instances` 创建实例时，对关联了外部选项的单选控件，按其返回值中的 `options.id` 传值（这是《审批实例表单控件参数》明文要求的传法）。

接口返回 `code: 0`，建单成功。但随后用 `instances get` 回读，这两个控件的 `value` 与 `option.text` **均为空串**，审批详情页显示为未填写；`option.key` 则完整保留了我们传入的 id。

## 关键对照

下面两单来自**同一份审批定义**、**完全相同的选项 id**，相隔 3 分 40 秒，唯一差别是提交路径。第二单是把第一单回读出的表单**照抄回投**的。

### 一、客户端提单（正常）—— 我们的「源」

`GET /open-apis/approval/v4/instances/F811683C-9FAC-4836-B9F6-CEEF1EBEC891`
（serial `202609280123`，16:43:13）回读出的 `form` 里，两个外部数据源单选控件的原始形态：

```json
{"id":"widget17902293404030001","name":"办公地点(Office location)",
 "type":"radioV2","ext":null,
 "value":"成都",
 "option":{"key":"fldeysrdna:d22258711825","text":"成都"}}

{"id":"widget17902293451560001","name":"公司名称/Company name",
 "type":"radioV2","ext":null,
 "value":"成都市易威行科技有限公司",
 "option":{"key":"fldq7lcb6y:dc6b027d9457","text":"成都市易威行科技有限公司"}}
```

### 二、我们怎么照抄的

把上面这份 `form` 逐项回投。回读形态有三处**不能直接当输入用**，被迫做了还原 ——
每一处都是接口自己报错逼出来的：

| 还原 | 为什么 | 原样回投的报错 |
|---|---|---|
| 去掉 `type:"text"`（说明控件） | API 不支持该控件 | — |
| 去掉明细里的 `attachmentV2` 子控件 | 回读是**下载链接**，提交要的是 **file code** | `1395006 控件值不合法或者为空。控件= widget17800356620670001` |
| 静态单选 `收款方类型` 的 `value` 由文案还原成选项值 | 回读给的是 `"个人"`，API 要的是 `mpuvnw0h-fe2sqa3x3xk-0` | `1390001 控件的值不存在单选框的选项中，请检查你的输入。控件= widget17802983364650001, 值= 个人, 选项= [{"value":"mpuvnw0h-fe2sqa3x3xk-0","text":"个人"}, …]` |

**两个外部数据源控件没有做任何还原** —— 回读给的就是 `option.key`，我们原样发回。

### 三、API 提单（空白）—— 我们实际发出的请求体

`POST /open-apis/approval/v4/instances` → 实例 `B45FBF8C-1204-4570-855A-1BB2758ECCAC`
（serial `202609280127`，16:46:53）：

```json
{
  "approval_code": "D0557DA6-CC9A-4B6B-BDF2-8DC675D2DD6E",
  "open_id": "ou_****（见文末附表）",
  "uuid": "e2e00000-0000-4000-8000-0000000000de",
  "form": [
    {"id":"widget17902293404030001",
     "name":"办公地点(Office location)","type":"radioV2","ext":null,
     "value":"fldeysrdna:d22258711825",
     "option":{"key":"fldeysrdna:d22258711825","text":"成都"}},

    {"id":"widget17902293451560001",
     "name":"公司名称/Company name","type":"radioV2","ext":null,
     "value":"fldq7lcb6y:dc6b027d9457",
     "option":{"key":"fldq7lcb6y:dc6b027d9457","text":"成都市易威行科技有限公司"}},

    {"id":"widget17796881483690001","name":"付款明细/Payment Details","type":"fieldList",
     "value":[[{"id":"widget17796935635570001","type":"textarea","value":"手动测试"},
               {"id":"widget17802950665910001","type":"number","value":1234.56}]]},

    {"id":"widget17802983364650001","type":"radioV2","value":"mpuvnw0h-fe2sqa3x3xk-0"},
    …其余文本控件按定义传值
  ]
}
```

> 线上 `form` 是**压缩转义后的 JSON 字符串**（上面为便于阅读展开成数组）；`open_id` 为脱敏展示，可另行提供。

两处要点：外部选项控件的 `value` 传的是 `options.id`；**连 `option` 对象（含已解析的 `text`）也一并提交了**。

### 四、结果

| | 客户端提单 `F811683C-…` | API 提单 `B45FBF8C-…` |
|---|---|---|
| 提交的 `option.key` | `fldeysrdna:d22258711825` | **一字不差** |
| 提交时附带的 `text` | （客户端生成） | **我们主动带了 `"成都"`** |
| 回读 `value` | `"成都"` | `""` |
| 回读 `option.text` | `"成都"` | `""` |
| 建单结果 | 成功 | `code: 0` **成功** |
| 审批页显示 | 成都 | **未填写** |

**`option` 对象被整个忽略**：飞书只读 `value`，把它原样存进 `option.key`，**不据我们提供的 `text` 回填**。审批页因此显示「未填写」。

> 注：后续对照测试确认，此问题仅出现在 V2 外部数据源（`externalDataSearchable=true`，即开启了模糊/分页搜索），V1 数据源正常。详见「V1 与 V2 外部数据源对照测试」一节。

## 已排除的可能

| 假设 | 验证方式 | 结论 |
|---|---|---|
| 传值形态不对 | 穷举 11 种形态提交（见下节） | 已排除 |
| 定义侧外部选项配置有误 | 同一份定义下客户端提单正常显示 | 已排除 |
| 外部数据源接口报错 / 超时 | 端点 12 小时内被调用 38 次，失败 0 次，服务端 ERROR/WARN 0 行 | 已排除 |
| 取数时机（提交前是否拉过选项） | 提交前 4 秒内确有带完整用户上下文的取数，该单仍为空白 | 已排除 |
| 发起人身份（跨应用 open_id） | 改用租户内稳定的 user_id 提交，发起人解析一致，结果不变 | 已排除 |
| 联动参数未完整支持 | 已删除定义侧联动配置并重试 | 已排除（根因是 V2 数据源，非联动） |
| V2 外部数据源（`externalDataSearchable=true`） | 将所有控件从 V2 改为 V1 后重新提单 | **已确认为根因**：V2 控件空白，V1 控件正常（见下节对照） |

## 提交形态穷举

每单最多在两个外部选项控件上各放一种形态，其余控件保持不变。全部返回 `code: 0`，除表中标注被拒者。

| # | 提交的 value | 回读结果 |
|---|---|---|
| 1 | `fldeysrdna:d22258711825`（`options.id`，文档要求的传法） | 原样存为 `option.key`，`text` 空 |
| 2 | `@i18n@fldeysrdna:d22258711825`（`options.value`） | 原样存，`@i18n@` 前缀未被剥离 |
| 3 | `成都`（选项文案） | 文案被当作 key 原样存 |
| 4 | `d22258711825`（id 去掉前缀） | 原样存为 key |
| 5 | `["fldq7lcb6y:bc6c077547c5"]`（数组） | **1395006 拒绝** |
| 6 | `{"id": …}` / `{"value": …}`（对象） | **1395006 拒绝** |
| 7 | 形态 1 再加 `option{key,text}` | `option` 被整个忽略（纯输出字段） |
| 8 | 形态 2 再加 `i18n_resources` 映射 | 建单成功，文案仍空 |
| 9 | `type` 改为 `radio`（v1） | **1390001 拒绝**（须与定义一致） |
| 10 | 改用 `user_id` 替代 open_id 提交 | 发起人解析一致，文案仍空 |
| 11 | 删除定义侧联动参数后重试 | 文案仍空 |

## V1 与 V2 外部数据源对照测试

为定位根因，我们将审批定义 `D0557DA6-CC9A-4B6B-BDF2-8DC675D2DD6E` 中的 5 个外部数据源控件分别标记为 V1（`externalDataSearchable=false`）或 V2（`externalDataSearchable=true`），用相同的 `options.id` 提交两次实例：

**第一次提单（V2 数据源未改）**：实例 `9B0F4A3E-DC7F-4CF5-BE87-75EFD3D7DA67`

| 控件 | 版本 | 联动 | 提交 value | 回读 value | option.text | 结果 |
|---|---|---|---|---|---|---|
| 归属银行 | V2 (searchable=true) | 无 | `col_3:8e31c2e9055c` | `""` | `""` | **空白** |
| 开户行行名 | V2 (searchable=true) | 有(→归属银行) | `col_2:a6ccef054be7` | `""` | `""` | **空白** |
| 联行号 | V1 (searchable=false) | 有(→开户行行名) | `col_5:7a2ed0503d65` | `"313428350502"` | `"313428350502"` | **正常** |
| 办公地点 | V1 (searchable=false) | 无 | `fldeysrdna:d22258711825` | `"成都"` | `"成都"` | **正常** |
| 付款公司名称 | V1 (searchable=false) | 有(→办公地点) | `fldq7lcb6y:dc6b027d9457` | `"成都市易威行科技有限公司"` | `"成都市易威行科技有限公司"` | **正常** |

**第二次提单（全部改为 V1 后）**：实例 `A56D1D1F-FBAA-45C7-B51B-4CC06D34B86C`

| 控件 | 提交 value | 回读 value | option.text | 结果 |
|---|---|---|---|---|
| 归属银行 | `col_3:8e31c2e9055c` | `"赣州银行股份有限公司"` | `"赣州银行股份有限公司"` | **正常** |
| 开户行行名 | `col_2:a6ccef054be7` | `"赣州银行股份有限公司南康横市支行"` | `"赣州银行股份有限公司南康横市支行"` | **正常** |
| 联行号 | `col_5:7a2ed0503d65` | `"313428350502"` | `"313428350502"` | **正常** |
| 办公地点 | `fldeysrdna:d22258711825` | `"成都"` | `"成都"` | **正常** |
| 付款公司名称 | `fldq7lcb6y:dc6b027d9457` | `"成都市易威行科技有限公司"` | `"成都市易威行科技有限公司"` | **正常** |

**结论：** 问题只出在 V2 外部数据源（`externalDataSearchable=true`，即开启了模糊/分页搜索）。V1 数据源的控件全部正常回填。这与《关联外部选项说明》中"使用了 V2 版本（对应 page_token，query 参数），暂未对开放平台做完整支持"的描述一致。

## 我方接口的实测响应

直接请求我方外部选项接口（`POST`，`Content-Type: application/json`），返回符合《关联外部选项》规范：

| | |
|---|---|
| `code` | `0` |
| `data.result.options` | 8 条，每条形如 `{"id":"fldeysrdna:d22258711825","value":"@i18n@fldeysrdna:d22258711825"}` |
| `data.result.i18nResources` | `[{"locale":"zh_cn","isDefault":true,"texts":{"@i18n@fldeysrdna:1bb5e3986433":"深圳", …}}]` |

`id`、`value`、`i18nResources` 三项均与规范一致，且 12 小时内 38 次调用全部成功（服务端无任何 ERROR/WARN 记录）。

## 依据的公开条款

> 《审批实例表单控件参数》单选一节：**「如果关联外部选项，则 value 需要传入外部选项的 options.id。」**

> 《审批常见问题》：「创建审批实例时，单选或多选控件关联了外部选项，如何传值？」——**「单选或多选控件的 value 参数传入关联外部选项说明中的 options.id 参数值。」**

我们的传法完全按上述两条执行。

## 希望得到确认或修复

1. **V2 外部数据源（`externalDataSearchable=true`）何时支持通过 API 创建实例并正确回填文案？** 当前 V2 控件通过 API 提单后 `option.text` 为空，审批页显示"未填写"。若短期内无法支持，请在文档中明确标注此限制，避免开发者踩坑。
2. 若 V2 支持有时间表，请告知预计版本或替代方案（例如：是否可以通过其他参数绕过，或是否有内部接口可用）。
3. 若非我方漏传参数，请告知缺失的字段名与所属接口。

另请一并确认：现行 `value` 传 `options.id` 是否仍为唯一正确传法。上面的对照表明客户端路径写入的 `option.key` 正是我方返回的 `options.id`，故我们认为该传法无误。

## 附带发现

排查过程中遇到的另外三处文档与接口不一致，一并反馈。

| 项 | 说明 |
|---|---|
| `i18n_resources.texts` 的示例值有误 | 《创建审批实例》中 `texts` 的类型是 `i18n_resource_text[]`（`{key,value}` 数组），但**示例值写成了 JSON map**。照示例提交会返回 `9499 Invalid parameter type in json: texts`。 |
| `instances get` 的 `form` 无法直接回投 | 三处不对称：API 不支持 `text`（说明）控件；明细内的 `attachmentV2` 回读是**下载链接**而非提交所需的 file code（回投报 1395006）；单选/多选的 `value` 回读是**展示文案**，而 API 要求的是 `option.key` —— 回投「个人」会得到 `1390001 控件的值不存在单选框的选项中… 值= 个人`。 |
| `approvals get` 的 `externalDataLinkage` 不可作判据 | 控件确实关联了外部数据源时，该字段仍可能返回 `false`，且 `externalData` 整块（含 `token`、`externalUrl`、`key`）为空。不宜据此判断某控件是否使用外部选项。 |

## 复现步骤

1. 准备一个单选控件已关联**V2 外部数据源**（`externalDataSearchable=true`，即开启了模糊/分页搜索）的审批定义，外部接口按《关联外部选项》返回 `options`（含 `id`）与 `i18nResources`。
2. 调用 `POST /open-apis/approval/v4/instances`，该控件的 `value` 传对应的 `options.id`。
3. 调用 `GET /open-apis/approval/v4/instances/:instance_id` 回读该实例：`option.key` 为传入的 id，`value` 与 `option.text` 为空。
4. 在同一份定义下从飞书客户端提交同样两个选项，回读可见 `value` 与 `option.text` 均有文案。
5. 将控件的外部数据源改为 **V1**（关闭模糊/分页搜索，`externalDataSearchable=false`），重新通过 API 提单，回读可见 `value` 与 `option.text` 均正常回填。

## 附：外部选项接口与 Token（请填入后一并交给对方）

对方要核对「我们这边到底有没有正常返回选项」，最快的办法是直接调这两个接口。地址就是审批定义里
`externalData.externalUrl` 登记的那两个，Token 请填入：

| 控件 | 请求地址 | Token |
|---|---|---|
| 办公地点(Office location) | `http://47.109.148.207:18654/api/v1/feishu/approval/options/fldeysrdna` | `157d7429c461219a1f616e3b58c3bc04eff0291eb23214a4f2a80c1c636fc876` |
| 公司名称/Company name | `http://47.109.148.207:18654/api/v1/feishu/approval/options/fldq7lcb6y` | `d05eed03775157c41bba23996d45b158e4ff644bc892df920e91e9db6d5d3a11` |

调用方式与《关联外部选项》规范一致：`POST <上表地址>` + `Content-Type: application/json`，
`token` 取上表对应那一行的值。两个控件的请求体只差 **`linkage_params`**：

**办公地点**（无联动）：

```
POST http://47.109.148.207:18654/api/v1/feishu/approval/options/fldeysrdna
Content-Type: application/json

{"token": "157d7429c461219a1f616e3b58c3bc04eff0291eb23214a4f2a80c1c636fc876", "locale": "zh_cn"}
```

**公司名称**（联动父控件「办公地点」）：

```
POST http://47.109.148.207:18654/api/v1/feishu/approval/options/fldq7lcb6y
Content-Type: application/json

{"token": "d05eed03775157c41bba23996d45b158e4ff644bc892df920e91e9db6d5d3a11", "locale": "zh_cn", "linkage_params": {"fldeysrdna": "成都"}}
```

- 联动 key = **`fldeysrdna`**，取自**审批定义**里该控件的
  `externalData.linkageConfigs[].key`（同时也是父控件「办公地点」的字段代码）。
- 联动 value = 父控件选中的**文案**（`"成都"`），**不是** `@i18n@<option_id>` ——
  这是飞书回传过来的真实形态，我们按这个形态匹配。

（其余可选字段：`page_token` / `query` 用于分页与模糊搜索，本例用不到。）

> 补充：**去掉联动也试过**。把「公司名称」的联动配置删掉后重新提单，`linkage_params`
> 不再出现，外部选项仍然是空白。所以本问题与联动无关，两个控件都复现。

返回结构同规范：`data.result.options[].id` 是我们的选项 id（如 `fldeysrdna:d22258711825`），
`data.result.i18nResources[].texts` 里是 `@i18n@<id>` → 文案的映射。上文「三、」里提交的
`options.id` 正是这里返回的 `id`。

如需其它信息（应用 ID、发起人 `open_id`、原始请求日志），也可以填在这里：

| 项 | 值 |
|---|---|
| 应用 ID | `<待填>` |
| 发起人 open_id | `<待填>` |

---

为安全起见，本文档不含应用密钥。
