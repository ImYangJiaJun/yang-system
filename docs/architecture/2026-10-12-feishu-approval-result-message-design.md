# 飞书审批派发结果反馈：HTTP 正文与后端直接消息

> 2026-10-12 完成方案与工作流只读查证；2026-10-13 在独立工作树实施；2026-10-14 按用户选择的“精简内容，突出结论、原因和下一步”调整富文本，应用发送的精简示例已由用户确认“符合预期”，随后补充消息末尾的表格、原记录及已确认审批实例入口。本文件说明实现、官方契约、配置方法与验证边界；代码尚未部署。
>
> 用户本期范围：完成除接收人 ID 获取以外的能力，并使用用户提供的 open_id 验证应用身份发送。工作树为 `C:/Users/16040/.codex/worktrees/approval-result-feedback/yang-system`，分支 `codex/approval-result-feedback`，基点 `a3515c5`（包含主分支的用户 OAuth、官方 GET 搜索和凭证读取修复）。

## 1. 现在可以怎样使用

审批派发接口返回可读中文正文；顶层 `message` 与 `data.message` 完全相同。请求明确带接收人的 ID 或邮箱时，后端将同一结果精简为飞书机器人富文本消息：结论作标题，正文突出记录、原因和下一步，并保留涉及的控件、字段与数据源名称；末尾补充带业务名称的表格、原记录及已确认审批实例入口。完整定位信息在 HTTP 响应与派发记录中查看。无需增加工作流消息节点，也无需为直接发送配置 HTTP 响应变量。未带接收人时，仍可使用 HTTP 反馈。

**每次按钮触发时自动通知本人，仍需调用方提供本人 ID 或邮箱。** 本期不获取通知接收人 ID、不建立通讯录映射，也不从申请人员列猜接收人。用户已提供杨佳俊的 open_id；旧版 text 和首版 post 已确认送达，2026-10-14 的精简 post 示例已确认符合预期。这些验证不证明工作流能够动态取得每一位触发人的身份。

本次“结果”是创建或恢复审批实例、获取编号、回填编号的结果。单条请求同步反馈本轮结论；批量请求只反馈已受理。审批后续通过、拒绝、撤回，以及批量最终汇总，属于后续异步能力。

```mermaid
flowchart LR
    A[按钮或调用方] --> B[现有 HTTP 派发端点]
    B --> C[单条处理 / 批量受理]
    C --> D[生成中文正文]
    D --> E[HTTP 返回相同正文]
    D --> F{显式接收人且配置有效}
    F -->|是| G[应用身份调用飞书 IM]
    G --> H[独立通知回执]
    F -->|否| I[仅 HTTP 反馈]
    H --> E
```

## 2. 官方契约与身份结论

2026-10-13 读取 IM 和用户搜索文档，2026-10-14 补读记录链接及 AppLink 的以下官方 Markdown，均为 HTTP 200。官方 API 能力与当前应用的权限状态是两项证据，读取文档不等于应用已获授权。

| 官方资料 | 本期采用的契约 |
|---|---|
| [发送消息](https://open.feishu.cn/document/server-docs/im-v1/message/create.md) | `POST /open-apis/im/v1/messages` 支持 tenant_access_token；接收人必须明确类型；应用机器人需启用并发布，用户需在可用范围内 |
| [发送消息内容](https://open.feishu.cn/document/server-docs/im-v1/message-content-description/create_json.md) | msg_type=post，content 为 zh_cn.title 与二维段落数组的 JSON 序列化字符串；text 节点支持 bold/italic，hr 节点为分割线 |
| [检索记录](https://open.feishu.cn/document/server-docs/docs/bitable-v1/app-table-record/get.md) | `GET .../records/:record_id?with_shared_url=true` 返回 `record_url`；使用应用身份，复用已有多维表格权限，只用于可选的记录入口补充 |
| [批量获取记录](https://open.feishu.cn/document/docs/bitable-v1/app-table-record/batch_get.md) | 请求参数声明 with_shared_url，但 record_url 的描述写“本接口不返回”；保留现有业务取数契约，链接单独按上述单条接口读取 |
| [打开小程序](https://open.feishu.cn/document/common-capabilities/applink-protocol/supported-protocol/open-a-gadget.md) | AppLink 区分 PC 的 path_pc 和手机的 path；复用现有编号链接回填的 instance_applink，不从审批定义或请求 UUID 猜实例 |
| [搜索用户](https://open.feishu.cn/document/server-docs/contact-v3/user/search-users.md) | `GET /open-apis/search/v1/user` 仅支持 user_access_token；不能通过增加应用权限将其变成应用身份接口 |

应用身份发送消息可申请官方列出的三种权限之一：`im:message`、`im:message:send_as_bot`、历史权限 `im:message:send`。本用途优先申请 `im:message:send_as_bot`。使用 user_id 还需字段权限 `contact:user.employee_id:readonly`。

官方 IM 支持 open_id、union_id、user_id、email、chat_id。本期只开放前四种个人标识。open_id 随应用变化，不能直接使用其他应用的 open_id；邮箱须是该飞书用户的真实邮箱。本期的语法校验不证明账号存在，也不能严格区分 ASCII 英文姓名与自定义 user_id。

发送接口限额为 1000 次/分钟、50 次/秒，同一用户 5 QPS。230020 可能伴随 HTTP 400，仍按频控重试。官方 UUID 最长 50 字符，相同 UUID 在一小时内至多成功发送一条消息。本期没有新增全局消息限速器，超限依赖有限退避与明确失败回执。

主分支已合入用户 OAuth 能力，现有审批发起人参数 applicant 的姓名搜索链路使用 user_access_token 和官方 `GET /open-apis/search/v1/user`，空姓名仍不取凭证。本工作树同步保留该改动；用户已在主分支成功取得上述用户信息，本期直接使用其提供的 ID 验证发送。requested_by 只作日志展示；通知接收人、审批申请人、日志请求人分别按各自契约处理。

## 3. 接入请求

**本章节描述 a3515c5 基线工作树的旧派发合同，不能直接用于当前主分支。** 本工作树端点为 `POST /api/v1/feishu/approval/dispatch`，沿用管理 Token 鉴权和启用配置的坐标校验。新增可选字段 notification_recipient，其余字段沿用该基线契约。

2026-10-14 只读核对主 checkout 时，主线已迁移为 `POST /api/v1/feishu/approval/dispatch/{config_key}`，请求体以按配置签发的 token 鉴权，不再接收旧合同的配置坐标。当前反馈实现尚未适配该迁移；合入主线或部署前，必须将反馈收尾及定位信息接到主线的配置查询结果，并按新合同更新测试、工作流接入说明。此处的旧请求示例仅用于本工作树验证。

以下均为占位示例，不能直接用来触发真实审批：

```json
{
  "base_token": "appExample",
  "table_id": "tblExample",
  "record_id": "recExample",
  "requested_by": "触发人展示名",
  "notification_recipient": {
    "receive_id_type": "email",
    "receive_id": "initiator@example.com"
  }
}
```

| 输入 | 规则 |
|---|---|
| notification_recipient | 省略即不发送；不影响 HTTP 正文，也不改变申请人 |
| receive_id_type | open_id / user_id / union_id / email，必须显式给出 |
| receive_id | trim 后非空、最长 320 字节、不能有空白或控制字符；open_id 为 ASCII ou_ 前缀，union_id 为 ASCII on_ 前缀，user_id 为 ASCII；邮箱检查基本结构 |
| record_id | 给出时处理单条；批量必须整个省略，传空白仍被拒绝 |
| requested_by | 展示与日志字段，不能当作通知接收人 |
| applicant | 原审批申请人通道，与通知接收人无关；未传时继续读取原人员列 |

notification_recipient 拒绝未知子字段，不接受 name 类型或姓名搜索参数。合法语法不等于飞书账号有效；收件人错误或权限不足时，通知回执为失败。

已有邮箱可直接使用 receive_id_type=email，无需先换取 open_id。在当前 HTTP 节点的请求体中增加这两个明确字段即可接上后端发送。**怎样从触发器稳定取得本人邮箱或 ID 是本期排除的工作，不能假设 `$.触发器.user.id` 或 user.email 已验证可用。**

## 4. 响应与兼容性

保留 code/message/data 信封及 data.accepted/message/serial_number；新增 data.status 和 data.notification。顶层 message 原先为 ok，本期改为可读正文；如果消费方硬编码比较 `message == "ok"`，应改为读取业务码和状态。

| data.status | accepted | 含义 |
|---|---:|---|
| succeeded | true | 实例已确认，编号已成功回填；可能是新建，也可能是 UUID 恢复 |
| waiting | true | 本轮未创建，等待补齐数据或修正映射；保持旧 accepted 语义，不写终态错误 |
| created_pending_backfill | false | 已确认实例存在，但详情、编号、链接或编号回填尚未完成 |
| retryable | false | 本轮未完成，审批是否创建尚未确认；不得提示“肯定没创建” |
| failed | false | 配置、参数或终态业务失败 |
| accepted | true | 批量已受理，后台尚未完成处理 |

accepted=true 不代表审批一定已创建；使用 status 判断业务结论。保持兼容：**data.serial_number 只在编号成功回填后出现**。已知编号但未回填时，可以在正文显示编号，仍省略该字段。

业务处理结果，包括 waiting、retryable 和 created_pending_backfill，沿用 HTTP 200、code=0 的正常结果通道。未配置或停用且没有首次配置三件套时，沿用 HTTP 200、code=40401；进入 Action 的参数错误返回 HTTP 400、原框架错误码。服务端异常保留对应 HTTP/业务码，正文屏蔽内部原文。鉴权失败、反序列化失败或请求未到达 Action，仍由框架处理，不保证带本期反馈结构。

批量、不指定接收人的响应示例：

```json
{
  "code": 0,
  "message": "【批量派发已受理】\n配置：付款申请\n多维表：财务协作\n数据表：付款申请表\n审批：付款审批\n说明：已受理，处理结果稍后回填至表格\n下一步：后台扫描所有启用配置中回填列为空的记录，结果稍后写入各表；此消息不代表处理完成。\n技术定位：Base=appExample；表=tblExample",
  "data": {
    "accepted": true,
    "status": "accepted",
    "message": "【批量派发已受理】\n配置：付款申请\n多维表：财务协作\n数据表：付款申请表\n审批：付款审批\n说明：已受理，处理结果稍后回填至表格\n下一步：后台扫描所有启用配置中回填列为空的记录，结果稍后写入各表；此消息不代表处理完成。\n技术定位：Base=appExample；表=tblExample",
    "notification": {
      "status": "not_requested",
      "message": "未指定接收人，完整反馈已通过 HTTP 返回。"
    }
  }
}
```

| notification.status | 含义 |
|---|---|
| not_requested | 未指定接收人，返回 HTTP 反馈 |
| skipped | 请求或启用配置没有通过通知前置校验，未发送 |
| sent | 飞书返回 code=0 和非空 message_id，已接受消息；不等于用户已读 |
| failed | 出站配置缺失、接口拒绝、未取得有效确认或超时；审批结论不变 |

通知成功的子对象示例：

```json
{
  "status": "sent",
  "message": "飞书已接受消息。",
  "message_id": "om_example",
  "uuid": "c33ba0b2-024e-4995-a223-b16a00c126e6"
}
```

通知超时也可能已经发出，所以 failed 文案会说明“送达情况未确认，请先检查消息”。通知状态不覆盖顶层业务码、accepted、status 或编号。

## 5. 正文怎样避免只显示 ID

HTTP 正文按结论、配置/表格/审批名称、记录摘要、原因、处理建议、定位信息组织；机器人消息只投影必要信息，不改变业务结论。配置的展示名称来自现有缓存，可能滞后于飞书改名；可沿用控制台的名称刷新能力。查不到、空白或查询失败时写“名称暂不可用”，不推断来源已删除。

外部选项匹配失败显示审批控件的明细路径、表格字段名、选项数据源标题与绑定字段名、当前文案、严格匹配失败原因及修正建议。以下来源名称仅为示意，**不代表已查明真实 fldq7lcb6y 的绑定**：

```text
【本轮未创建审批，需要修正数据】
配置：付款申请
多维表：财务协作
数据表：付款申请表
审批：付款审批
记录：成都市易威行科技有限公司
说明：本轮未处理：审批控件：明细 / 付款公司名称
来源字段：实际付款公司
选项来源：合作企业 / 企业名称
当前值：成都市易威行科技有限公司
原因：没有唯一且完全匹配的启用选项
处理：检查选项是否启用、文案是否完全一致，清理同名重复项。
技术定位：控件=widgetExample；字段=fldCompany；source_key=fldq7lcb6y
下一步：按原因补齐数据或修正映射后，再点击按钮。
技术定位：Base=appExample；表=tblExample；记录=recExample
```

停用来源的名称查询不筛掉停用行；首次配置时来源没有启用选项，也显示真实名称。**展示查询不放宽匹配权限**：业务仍要求 active 数据源、enabled 绑定和选项、正确 source_key、唯一且完全相等的文案。不存在或查不到的来源显示名称不可用并保留 source_key。

本期只对官方 is_primary=true 的主字段取安全摘要，支持允许的文本、数字、选择、公式或自动编号类型及其可读值，最多 80 字符；不把人员、附件、链接或未知对象 JSON 倒入正文。不是从任意首列猜主字段，也不声称具备敏感内容语义识别。

名称清理控制字符；单个名称上限 100 字符，失败选项当前值 160 字符，说明 950 字符，完整正文 2000 字符。原始飞书报文、Token、数据库错误原文不作为用户通知；已知错误码保留安全说明。

### 富文本展示

发送仍使用应用身份的同一个 IM 接口，msg_type=post；无需卡片模板、卡片回调、新权限或工作流节点。官方 post 支持标题、段落、文本样式、链接和图片。精简正文使用 title、text 和 bold 标签，消息末尾使用原生 a 节点提供跳转入口。

首行完整的 `【结论】` 转为原生标题，保留六种业务状态原有的确定性边界。正文按以下顺序生成，标签加粗，动态值使用普通文本：

1. 记录：摘要与数据表名称合在一段；批量没有记录时只显示数据表。
2. 原因：去除重复的“说明”“本轮未处理”“暂时失败，可重试”前缀；成功和批量受理使用“结果”标签，已确认实例待回填和创建情况未确认仍使用各自标题。
3. 涉及：将审批控件的明细路径、来源字段、选项数据源名称合成一段，便于定位问题。
4. 当前值：只在与记录摘要不完全相同时显示，避免重复公司名称；不能因精简而隐藏不同的失败值。
5. 下一步：优先显示具体“处理”建议；没有具体建议时保留该状态的通用下一步。

配置名称、空段落、分割线、专用技术定位行不放入消息。多维表和审批的业务名称在末尾的入口段显示。控件、字段、Base、表、记录和 source_key 的完整 ID 仍保留在 HTTP 正文与请求日志里。未识别的业务说明保留在“原因”中；没有结论标记时使用“审批派发结果”标题。正文通常为四至五段，末尾增加二至三段入口；实际长度受业务名称与说明影响，不承诺手机端固定行数。

例如精简正文序列化前为（末尾入口见下节）：

```json
{
  "zh_cn": {
    "title": "本轮未创建审批，需要修正数据",
    "content": [
      [
        {"tag": "text", "text": "记录：", "style": ["bold"]},
        {"tag": "text", "text": "成都市易威行科技有限公司（付款申请表）"}
      ],
      [
        {"tag": "text", "text": "原因：", "style": ["bold"]},
        {"tag": "text", "text": "没有唯一且完全匹配的启用选项"}
      ],
      [
        {"tag": "text", "text": "涉及：", "style": ["bold"]},
        {"tag": "text", "text": "审批控件：明细 / 付款公司名称；来源字段：实际付款公司；选项来源：合作企业 / 企业名称"}
      ],
      [
        {"tag": "text", "text": "下一步：", "style": ["bold"]},
        {"tag": "text", "text": "检查选项是否启用、文案是否完全一致，清理同名重复项。"}
      ]
    ]
  }
}
```

动态公司名称和错误摘要只写入 text 节点，再由 JSON 序列化器生成 content 字符串；不会被解释成 Markdown、@ 或超链接。中文、引号、Emoji 和文案中的第二个全角冒号按字面保留；换行按上述条目重新组织。官方说明 style 不支持自定义机器人 Webhook 和批量发送接口；本期调用的应用 IM 创建消息接口支持 bold。post 不提供本期可用的自定义字号或颜色。

### 消息末尾的跳转入口

| 入口 | 可读文字 | 地址与显示条件 |
|---|---|---|
| 多维表 | 多维表名称 / 数据表名称 | 复用控制台现有 `https://www.feishu.cn/base/{base_token}?table={table_id}` 形式；发送前已复核配置坐标和 enabled，两个坐标再按路径段白名单校验 |
| 原记录 | 本次记录的主字段摘要 | 仅单条请求出现；以应用身份调用官方单条记录接口取 record_url，核对返回 record_id 与请求一致，且地址为飞书/Lark HTTPS 链接 |
| 审批 | 审批名称 / 审批编号 | 仅 Backfilled 或 CreatedPendingBackfill 保留已确认 instance_code 并生成现有审批 AppLink；实例已确认但编号未知时写“编号待生成” |

Waiting、Terminal、Retryable 和批量 Accepted 没有确认实例，所以“审批”只显示审批名称，不能跳转到一条不存在或未确认的实例。原记录摘要不可用时写“本次触发的记录（摘要暂不可用）”；record_url 缺失、错记录、异常或超时均降级为普通文字，多维表入口仍可用于查找。名称仍按既有清理和截断规则处理，不把裸 ID 当作入口标题。

链接补充发生在派发结束后，只有明确接收人、有效启用配置和可用出站设施时才读取；总等待预算两秒，与消息发送的十秒预算分别计算。不会修改分享设置或记录，也不会重新创建审批；原记录链接读取失败不改审批状态。链接不会额外赋予查看权限，接收人能否查看仍由飞书判断。HTTP 正文保留原有完整定位文本，不额外拼入网址；IM 将同一结果增加可点击的展示入口。

官方已将单记录 GET 标记为历史接口、不推荐用于新的业务取数。新版 batch_get 虽提供 with_shared_url 参数，其 record_url 字段说明却写明该接口不返回此字段；当前只用契约明确的旧 GET 尽力补充消息入口，不改变业务取数路径。接口不可用时退回普通文字与多维表入口。待新版契约和真实返回确认支持记录链接后再替换此可选读取。

已创建但尚未回填时，消息审批入口可显示已确认的编号；HTTP 的 data.serial_number 仍只在编号成功回填后出现，保留原有兼容契约。

单条成功的消息示例（方括号表示原生可点击文字，示例编号不代表真实审批）：

```text
审批实例已确认，编号已回填
记录：成都市易威行科技有限公司（付款申请表）
结果：已创建审批实例，编号 202610140001
下一步：请到表格查看审批编号；可选扩展字段以表格实际值为准。
多维表：[财务协作 / 付款申请表]
原记录：[成都市易威行科技有限公司]
审批：[付款审批 / 202610140001]
```

没有创建实例的反馈保留上述表格和原记录入口，“审批：付款审批”作为普通文字；原因与涉及字段仍使用已验收的精简正文。

### 当前批量成功与失败反馈

**批量成功当前仅指受理成功，status=accepted。后台处理完毕后没有再发成功/失败汇总消息，也没有逐条通知。** 受理消息为：

```text
批量派发已受理
数据表：付款申请表
结果：已受理，处理结果稍后回填至表格
下一步：后台扫描所有启用配置中回填列为空的记录，结果稍后写入各表；此消息不代表处理完成。
多维表：[财务协作 / 付款申请表]
审批：付款审批
```

这里的多维表入口标识本次按钮请求的配置；实际 worker 仍扫描所有启用配置。消息没有“成功 N 条 / 失败 M 条”，没有单条实例入口，也不能把受理当作所有记录成功。后续结果通过各表回填列及控制台任务、派发记录查看。

| 批量失败发生位置 | 当前行为 |
|---|---|
| 入口处理失败，但已识别有效启用配置 | 指定接收人且通知可用时，可发送“本轮处理失败”、原因、下一步、触发表格入口和审批名称 |
| 入口前置参数/鉴权错误，或配置未配置、已停用、发送前复核失败 | 不直接发送消息；由 HTTP 返回失败及 notification=skipped（鉴权中间件提前拒绝时仍走既有鉴权响应） |
| 已受理后，某条记录在 worker 中成功或失败 | 写任务状态和既有回填；不会追加本期消息。等待修正与可重试结果保留既有重试资格 |
| 受理成功但消息调用失败 | 审批 status=accepted 保持不变，notification=failed；不能因此重跑整批 |

若后续需要最终批量汇总，需要单独定义批次范围、归属、结束条件和持久发送记录；当前 worker 并没有按一次按钮请求归集批次，不能从全局任务表临时计数当作该触发人的批次结果。

## 6. 发送、重试与持久化

复用现有 OutboundTransport、TenantTokenProvider 与出站重试，不新增依赖或发送服务。消息接口只由派发结果收尾调用，不反向调用审批创建。

通知前在两秒展示查询预算内重新核对配置主键、base_token、table_id、enabled。找不到启用配置或查询异常时跳过发送，HTTP 保留派发结论与名称降级提示。这是发送前复核，不是与飞书请求原子提交；复核之后配置再变化的极短窗口仍存在。

通知本身总预算十秒；每条 IM 请求两秒。每个 Token 的发送序列最多三次尝试，退避 250 ms 起，最多一秒，并限制 Retry-After 等待。Token 过期最多刷新一次，直接使用刷新返回的 Token；刷新后可开始第二个最多三次的序列，仍受十秒总预算限制。永久错误不重试。

同一 HTTP 调用内，消息重试始终复用一个随机 UUID；审批 UUID 仍按原记录坐标派生，两者用途不同。消息 content 用 JSON 序列化器生成，保留中文、换行、引号和 Emoji，不手写转义。

**本期只有当前调用内的有限重试，没有持久通知队列。** 进程中止不会在重启后自动补发；再次调用 HTTP 会生成新消息 UUID，可能重复通知。官方一小时去重不能代替长期发送记录。请求日志写失败仍只降级日志，不撤销已发生的审批或通知。

现有请求日志摘要列最长 255 字符，本期按 Unicode 字符截断；response_body 保存完整 data JSON，包含业务正文、状态和独立通知回执。无需新增数据库列或 SQL 迁移。通知邮箱/ID 会进入现有请求原文日志，沿用审批控制台权限管理与日志运维纪律。

后台任务遇到 created_pending_backfill 时保持 pending、清租约，并保留已确认 instance_code 及可选已知编号，供原 UUID 续跑。既有 succeeded/waiting/failed/accepted 请求日志四桶、worker retryable 指标桶继续兼容；细分状态在完整响应中查看。六个可选扩展字段沿用原行为，succeeded 只保证编号回填，扩展字段实际结果以表格为准。

增加发送等待可能延长单条 HTTP 时长。十秒只是通知预算，不是整个派发请求预算；审批创建、详情获取、表格写入及日志落库还有各自耗时，不能承诺始终低于多维表格工作流超时。

## 7. 既有工作流证据与本期影响

以下是 **2026-10-12 的只读快照**，本期没有重新读取或修改云端工作流：

| 项目 | 当时读取的事实 |
|---|---|
| [用户给出的入口](https://bvijptdn9rx.feishu.cn/base/Lxf4bG4R3agS3QsMWGycPzXDnCg?table=wkfoFIKAbMcAZjUz) | Base 为“技术测试专用”；链接的 wkf... 是工作流 ID，不是派发 API 的数据表 ID |
| 实际目标表 | 技术测试专用（财税数据源），tblWZfpeWI9rglXs |
| 工作流 | wkfoFIKAbMcAZjUz，已启用，HTTP 带 record_id |
| 工作流 副本 | wkf46xRaK9ixfNGB，已启用，HTTP 不带 record_id |
| HTTP 节点 | JSON 响应，response_value 为 `{}`，读取定义中没有后续消息节点 |
| 请求人 | 将 `$.trigbcbHHBAb.user` 引用拼入 requested_by；当时文本槽实际渲染为姓名 |

截图中把整个 HTTP body 放进消息正文会展示 JSON。若使用原生消息节点，应引用正文字符串；如果采用本期后端直发，只需给现有 HTTP 请求增加明确接收人，不增加该消息节点。身份字段不能从上述姓名引用直接推出。

批量 worker 仍扫描**所有启用配置中回填列为空的待处理记录**，不只处理本次按钮所在表。旧批量工作流在 HTTP 前最多修改 100 条“申请状态为空”的记录，这与后台判据和扫描范围不同；本期受理正文不承诺数量或最终成功。

## 8. 与另一任务的共存关系

关联设计：[审批回填、发起人参数与追踪](2026-10-12-feishu-approval-backfill-initiator-tracking-design.md)、[用户身份 OAuth](2026-10-13-feishu-user-identity-oauth-design.md)。两项基线实现已在本工作树，反馈功能基于它们增量实施，未改主 checkout、另一任务文档或其用户凭证模块。

| 共享位置 | 本期处理 |
|---|---|
| DispatchResult 与 worker | 保留 Backfilled 的 instance_code；增加创建后待回填状态，并更新所有消费者 |
| finish_backfill | 保留编号成功后的扩展回填；只在编号本身未完成时使用新状态 |
| dispatch 输入 | 保留 applicant、requested_by 和原配置字段；notification_recipient 单独可选 |
| 申请人解析 | 保留基线用户 Token、空姓名短路与已知授权错误提示；通知不复用姓名搜索。Action 收尾前屏蔽凭证及搜索错误原文，保留错误码与重新授权建议 |
| 表与 Schema | 不新增表或列，日志长度适配既有列；不引入迁移文件 |
| 工作流 | 不更新云端定义；接收人来源后续接入时只补显式通知参数 |

功能目标可以共存。此前衔接 OAuth 基线时，dispatch.rs 的姓名搜索错误提示发生一处文本冲突，已同时保留 OAuth 的可行动提示和反馈的原文屏蔽。2026-10-13 再同步至 a3515c5，凭证读取、流程单选回填、OAuth 搜索权限、官方 GET 搜索和探针文案五项主分支改动与本任务恢复无冲突。后续若另一分支继续修改共享函数，合并时仍需复核结果类型和编排接线；独立工作树只隔离编辑，不保证未来没有文本合并冲突。

主线后续的 per-config token 迁移已改变 dispatch 输入、路由和配置定位；本工作树尚未衔接，属于明确的合入前置项，不能据此前的 OAuth 共存验证声称已兼容当前主线。

## 9. 本地验证与真实联调边界

本期实现文件：

- [HTTP 收尾与输入输出](../../src/addon/feishu/approval/actions/dispatch.rs)
- [中文正文与安全摘要](../../src/addon/feishu/domain/approval_feedback.rs)
- [消息契约、预算、重试与确认](../../src/addon/feishu/domain/approval_notification.rs)
- [原记录链接读取与地址校验](../../src/addon/feishu/domain/bitable.rs)
- [创建后待回填结论](../../src/addon/feishu/domain/approval_dispatch.rs)
- [外部选项名称与严格匹配](../../src/addon/feishu/domain/approval_provision.rs)
- [请求日志](../../src/addon/feishu/approval/domain/request_log_writer.rs)、[worker](../../src/infrastructure/feishu_approval_worker.rs)
- [真实 HTTP 集成回归](../../tests/feishu_approval_dispatch_integration.rs)

| 验证层 | 精简排版阶段证据（新增链接的验证另列） |
|---|---|
| 官方文档 | 2026-10-13 再次读取 IM 创建、消息内容、用户搜索三份官方 Markdown；确认身份、权限、UUID、频控及 post 标题、段落、bold/italic、hr 契约 |
| 领域及消息替身测试 | 精简富文本完成并恢复变异后，full 的 Rust 库测试 1043 条通过、9 条忽略；包括领域五类结果、正文六状态、创建/恢复/回填、消息四种接收类型、确认失败、Token 单次刷新、限流、UUID、十秒预算，以及用户凭证与姓名搜索错误的可行动提示和原文屏蔽；富文本覆盖六状态标题、加粗标签、分段、技术定位省略、重复值去重、不同值保留、具体处理建议优先与动态文案字面保留 |
| 真实 MySQL | 来源重名/不存在/停用、无启用选项、真实名称、配置 ID/坐标/启用状态复核均通过；测试库实际读写，不用 mock 数据库替代 |
| 真实 localhost HTTP、MySQL/Redis | 六个请求场景通过，见下表；核对 HTTP/业务码、共享正文、单次受理、申请人不变、完整响应日志 |
| 摘除式变异 | 精简排版阶段 115 项均触发运行时测试失败，其中 38 项针对最终精简富文本：标题、前缀、名称、技术定位省略、重复值去重、不同值保留、具体处理建议优先等。恢复后核对 8 个源码文件哈希一致，并重跑消息模块测试（10 条通过、2 条忽略）及完整门禁。汇总为 target/feedback-mutation-results/results.json，逐项日志同目录留档，精简版执行日志为 target/feedback-mutations-compact.log 与 target/feedback-mutations-compact-resume.log；编译失败不算有效变红 |
| OpenAPI 与 TypeScript 生成物 | scripts/dump_openapi.py 在新基线执行成功，日志为 target/feedback-openapi-official-search.log；生成物差异同时补齐已合入的六个可选回填列和 OAuth 基线契约。当前导出工具不装配要求 Feishu 出站配置的条件机器路由，因此快照不包含本派发端点。运行时 Action 自动登记 DispatchInput 的输入 Schema，返回仍为通用 ApiResponse，不能声称生成的 TypeScript 已包含本期六状态和通知参数 |
| full | 最终精简富文本完整门禁退出码 0，日志为 target/feedback-ci-full-compact-recheck.log；架构/自检、格式、Rust 全目标测试与 Clippy、生产依赖审计、前端完整检查均通过。Rust 库测试 1043 条通过、9 条忽略；Vitest 72 个文件、778 条测试通过；开发构建 E2E 21 条通过、2 条视觉测试按既有配置跳过，生产构建 E2E 4 条通过 |
| integration | 精简排版调整前的真实 MySQL/Redis 集成门禁退出码 0，22 组测试运行、147 条通过，其中审批派发 10 条全部通过；日志为 target/feedback-ci-integration-post.log。使用专用 _test MySQL 与 Redis DB 15，单线程执行。最终调整仅涉及消息正文投影与对应测试；没有再次运行整套 integration |

首次精简版 full 在未改动的前端模块页用例中发生 5 秒超时，日志为 target/feedback-ci-full-compact.log；随后单独复跑同文件 3 条测试全部通过，日志为 target/feedback-frontend-timeout-recheck.log。没有修改该用例、超时阈值或门禁配置；完整重跑全部通过。文档最终自检还核对了 4 个 JSON 示例、18 个本地链接、8 个变异前后源码哈希及临时兼容配置已删除。

2026-10-14 新增消息末尾链接后的验证单独记录如下，不以历史发送回执代替本次验证：

| 验证层 | 链接增量的实际结果 |
|---|---|
| 官方文档 | 实际补读 post 原生 a 节点、单记录 GET、新版 batch_get 和 AppLink 契约；原文保存在 target/feedback-link-docs/。确认单 GET 为历史接口、新版 record_url 描述存在矛盾，采用及降级边界见第 5 节 |
| 定向测试 | feedback_links 的 4 条用例通过，覆盖六种单条结果的实例与编号传递、批量入口、链接降级、记录 ID 一致性、GET 参数及两秒总预算；补充富文本末尾原生链接、普通文字降级和发送接线用例，均由下述完整门禁再次执行 |
| 摘除式变异 | 28 项均触发运行时测试失败，编译失败不计有效变红；覆盖 URL 守卫、记录身份、坐标、查询参数、批量分支、确认实例、已知编号、HTTP 编号兼容、两秒预算、原生 a/text 节点及发送参数接线。初次检查暴露反斜杠样例只覆盖域名的测试缺口，补入路径反斜杠后该项有效变红；汇总 target/feedback-link-mutation-results/results.json，执行日志 target/feedback-link-mutations.log 与 target/feedback-link-mutations-resume.log。恢复后三个源码文件哈希与变异前一致，并复核 HTTPS 守卫存在 |
| full | python scripts/run_ci.py full 退出码 0；target/feedback-ci-full-links.log。架构/自检、格式、Rust 全目标测试及 Clippy、生产依赖审计、前端完整检查均通过；Rust 库测试 1049 条通过、9 条忽略，Vitest 72 个文件/778 条通过，开发 E2E 21 条通过/2 条既有视觉测试跳过，生产 E2E 4 条通过。依赖审计首次网络 ECONNRESET，按既有策略重试成功；浏览器日志仍有既有 TableView/DataGrid 渲染期状态更新提示，本次未修改该前端逻辑 |
| 真实 localhost HTTP、MySQL/Redis | 定向复跑 dispatch_feedback_http_contract_and_notification_failure_are_independent，1 条测试、内部 6 个请求场景通过；target/feedback-link-http-integration.log。专用 _test 库和 Redis DB 15，验证业务码/HTTP 状态、正文同源、通知失败隔离、受理只唤醒一次、完整响应落库；该应用不装配出站设施，所以不覆盖真实链接读取或外发。未重跑整套 integration |
| 只读评审 | 复核批量扫描所有启用配置、缺少最终汇总、已知编号仅用于通知且不提前返回 HTTP 字段、HTTPS 守卫恢复，以及主线 per-config token 的待适配说明；没有修改主 checkout、工作流或发送新消息 |

2026-10-14 按用户提供的表格与审批链接，使用本工作树的 `send_feedback`、`HttpClientTransport` 和 `TenantTokenProvider` 向此前的杨佳俊 open_id 发送一次原生 post。测试退出码 0、1 条通过，飞书仅返回一条消息响应；HTTP 200、code=0、message_id=`om_x100b63db2ddf74a8cf992fd637fe0e0`，sender.id=`cli_aa21e85b6e381cba`、id_type=app_id、sender_type=app、msg_type=post。回执为 [target/feedback-live-post-links-receipt.json](../../target/feedback-live-post-links-receipt.json)，日志为 [target/feedback-live-post-links-smoke.log](../../target/feedback-live-post-links-smoke.log)。逐段核对标题、文字、样式、原生 a 节点及完整 href 均通过。

| 验证层 | 本次真实结果 |
|---|---|
| 消息内容 | 标题“验证消息｜表格与审批跳转”；末尾“多维表：打开本次审批来源表”和“审批：202610130151”均为原生可点击文字 |
| 表格目标 | [用户指定的表与视图](https://bvijptdn9rx.feishu.cn/base/Lxf4bG4R3agS3QsMWGycPzXDnCg?table=tblWZfpeWI9rglXs&view=vewmDPvC9M)，回显 href 与输入逐字一致 |
| 审批目标 | instance_applink 生成的 PC/手机路径均指向实例 20971264-278F-4501-8743-5B7E2DA25C46，完整链接与用户提供值相同；可读文字使用编号 202610130151 |
| 本人收件 | 用户回复“收到消息了，跳转正常”，确认真实收到 |
| 电脑点击 | 用户进一步明确确认“电脑和手机的两个链接都正常”，电脑表格与审批入口通过 |
| 手机点击 | 同一明确回复确认手机表格与审批入口通过；未记录具体操作系统或客户端版本 |
| 业务影响 | 本次仅发送验证消息，没有创建审批、修改记录或调整工作流 |

本次证明两个指定链接通过应用富文本送达，且本人在电脑、手机均可正常跳转。表格使用用户提供的租户域名并保留 view；生产代码自动生成的通用域名表链接不带 view，尚未单独实测。未提供 record_id，因此没有调用官方记录接口或发送“原记录”链接，不能将本次结果视作记录 URL 读取或完整派发链路通过。

首次运行因 `--exact` 配合短测试名没有匹配用例，结果为 0 条测试、未发送；日志保留为 target/feedback-live-post-links-no-test.log。改用完整测试名后执行一次发送。主分支配置仍仅在临时副本中移除旧工作树不认识的两个事件字段，其余 TOML 值与应用 ID 均核对不变，测试结束即删除副本。历史回执未覆盖，未因断言或客户端验证重复外发。

发送后只读核对首版、精简版及链接版三份 post 回执，1 条测试通过；日志为 [target/feedback-live-post-links-receipt-check.log](../../target/feedback-live-post-links-receipt-check.log)。通知模块定向回归 11 条通过、2 条显式外部验证忽略，日志为 [target/feedback-live-post-links-unit.log](../../target/feedback-live-post-links-unit.log)，没有重复发送。此次仅更新显式发送样例、回执断言和文档，生产发送与链接逻辑未改变，未重复整套 full/integration。

真实 localhost HTTP 集成测试没有出站 HTTP Tools，通知确定失败；接收端只观察 worker 信号，不执行真实审批或消息。Token 刷新和频控由严格的飞书响应替身验证。另有独立真实发送测试使用本工作树的 send_feedback、HttpClientTransport 和 TenantTokenProvider，发送后保存并核对官方回执；旧版 text、首版 post 和精简 post 的本人收件均已确认，精简样式已验收。

首次真实文本测试 `real_app_sends_feedback_and_confirms_sender_and_content` 在 a3515c5 上执行，退出码 0、1 条通过。历史回执为 [target/feedback-live-im-receipt.json](../../target/feedback-live-im-receipt.json)，执行日志为 [target/feedback-live-im-smoke.log](../../target/feedback-live-im-smoke.log)：

| 核对项 | 实际结果 |
|---|---|
| 身份 | tenant_access_token；sender.id_type=app_id、sender.sender_type=app |
| 发送应用 | yang-system，cli_aa21e85b6e381cba；未使用另一应用的 CLI 凭证 |
| 接收人 | 用户提供的杨佳俊 open_id；2026-10-13 本人确认成功收到 |
| 官方确认 | HTTP 200、code=0；message_id=om_x100b63d4c64b80b0b1755ae62d55b32 |
| 正文 | text 消息，官方回执正文与生成正文完全相同；包含公司、审批控件、来源名称、原因与处理建议 |
| 业务影响 | 消息明确标注“验证消息｜格式示例”；本次没有创建审批或修改多维表格 |

新版真实富文本只发送一次，接收人和应用与上次相同：message_id=om_x100b63d5d607f8acb34a52deb511167，HTTP 200、code=0、sender.sender_type=app、msg_type=post。回执为 [target/feedback-live-post-receipt.json](../../target/feedback-live-post-receipt.json)，原执行日志为 [target/feedback-live-post-smoke.log](../../target/feedback-live-post-smoke.log)。原发送测试的严格 JSON 相等断言失败：飞书会给普通文本节点补上 style=[]，并增加 content_v2；发送本身已成功，不能将测试失败当作未发送而直接重试。

改为逐段核对标题、节点类型、文字与样式后，使用只读测试 `saved_rich_feedback_receipt_preserves_title_content_styles_and_sender` 核对保存回执，退出码 0、1 条通过，不重复外发。核对日志为 [target/feedback-live-post-receipt-check.log](../../target/feedback-live-post-receipt-check.log)。标题、业务名称、段落、bold/italic 和 hr 均与输入语义一致；仅允许可选空样式和服务端附加字段。用户随后回复“已收到，需调整样式”，确认送达但尚未通过样式验收；未调用已读状态接口。

2026-10-14 按用户选择精简为原生结论标题与四段正文，应用发送一次，真实发送测试退出码 0、1 条通过：

| 核对项 | 精简版结果 |
|---|---|
| 官方确认 | HTTP 200、code=0；message_id=om_x100b63d2276618acb0b070e82ae513f |
| 身份和类型 | sender.id=cli_aa21e85b6e381cba、id_type=app_id、sender_type=app、msg_type=post |
| 标题 | 验证示例｜未创建审批，需修正选项 |
| 记录 | 成都市易威行科技有限公司（示例付款申请表） |
| 原因 | 没有唯一且完全匹配的启用选项 |
| 涉及 | 审批控件：明细 / 付款公司名称；来源字段：实际付款公司；选项来源：示例合作企业 / 企业名称 |
| 下一步 | 检查选项是否启用、文案是否完全一致，清理同名重复项。 |
| 样式与收件 | 标题、四段正文和加粗标签与请求逐项一致；用户回复“符合预期” |
| 业务影响 | 来源名称明确标为示例；没有创建审批或修改多维表格 |

精简版回执为 [target/feedback-live-post-compact-receipt.json](../../target/feedback-live-post-compact-receipt.json)，成功执行日志为 [target/feedback-live-post-compact-smoke.log](../../target/feedback-live-post-compact-smoke.log)。发送前主分支新增的 event_verification_token、event_encrypt_key 被旧工作树的配置校验拒绝；这些失败尝试没有调用 IM。验证改用临时兼容配置，仅删这两项，并断言其余 TOML 值和应用 ID 不变，发送后立即删除副本；主分支配置未修改，未引入另一任务的事件订阅能力。

当前真实发送测试名为 `real_app_sends_rich_feedback_and_confirms_sender_and_content`，默认 ignore，普通 full/integration 不会外发；显式运行需提供配置路径、期望应用 ID、open_id 与测试 Redis DB 15。测试现已改为本次链接样例，已有 feedback-live-post-links-receipt.json 时拒绝再次发送。四份历史回执均应保留，避免重复外发；只读核对比较保存的当时请求与官方响应，不用最新渲染器改写历史证据。

最终源码恢复后，再执行只读测试 `saved_rich_feedback_receipt_preserves_title_content_styles_and_sender`，同时核对首版 post 和精简 post 的保存回执：退出码 0、1 条通过，未发送新消息。日志为 [target/feedback-live-post-compact-receipt-check.log](../../target/feedback-live-post-compact-receipt-check.log)。标题、逐段文字、节点类型、加粗标签和应用发送者均与当时请求一致；普通文本节点的服务端空 style 与附加字段不影响语义核对。

| HTTP 请求场景 | HTTP / code | status | notification | worker 信号 |
|---|---|---|---|---:|
| 批量，不带接收人 | 200 / 0 | accepted | not_requested | 1 |
| 批量，已知邮箱 | 200 / 0 | accepted | failed（无出站设施） | 1 |
| 停用配置，open_id | 200 / 40401 | failed | skipped | 0 |
| 未配置，open_id | 200 / 40401 | failed | skipped | 0 |
| 空白 record_id，open_id | 400 / 700005 | failed | skipped | 0 |
| 姓名误作 open_id | 400 / 700005 | failed | skipped | 0 |

本地复现入口（专用 _test MySQL 与 Redis DB 15）：

```powershell
$env:YANG_SYSTEM_TEST_DATABASE_URL = "mysql://root:yang-local@127.0.0.1:3306/yang_system_test"
$env:YANG_SYSTEM_TEST_REDIS_URL = "redis://127.0.0.1:6379/15"
$env:PYTHONUTF8 = "1"
$env:PYTHONIOENCODING = "utf-8"
$env:CARGO_BUILD_JOBS = "1"
$env:npm_config_registry = "https://registry.npmjs.org"
python scripts/dump_openapi.py
python scripts/run_ci.py quick
python scripts/run_ci.py full
python scripts/run_ci.py integration
```

本工作树的 Cargo 相对依赖由本机 junction 指向主仓库 crates；早期验证临时加的空 `[workspace]` 已恢复，通过工作树父目录的本机 workspace exclude 保持可运行，产品 Cargo.toml 没有变更。Windows 上以 CARGO_BUILD_JOBS=1 限制编译内存占用，不改变测试选择或门禁内容。前端依赖按 frozen lockfile 安装，没有依赖清单变更。

**未完成的真实验收**：未部署，未运行云端两个工作流，未创建真实审批；从按钮动态取得触发人身份，以及审批创建、表格回填、HTTP 响应与消息的真实完整链路仍未验证。当前应用对指定用户的 text、首版 post、精简 post 和链接 post 发送、官方回执、语义核对及本人收件已验证，精简样式已验收；本次指定表格与审批链接在电脑、手机均由本人确认跳转正常。真实记录接口返回、“原记录”跳转、运行时自动生成的通用表链接及其他接收人的可用范围仍需核实。批量汇总、审批最终结论通知、重启后补发与跨调用严格去重不在本期。
