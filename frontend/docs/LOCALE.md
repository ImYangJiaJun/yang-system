# 语言与地区产品合同

<!-- locale-contract: supported zh-CN -->
<!-- locale-contract: runtime-switch disabled -->
<!-- locale-contract: reopen-trigger second-locale -->

## 当前发布范围

yang-system 首发只支持简体中文 `zh-CN`。这是明确的产品范围，不是已经实现国际化后的
默认语言：

- HTML 文档语言、日期/数字格式和本仓库业务文案统一为 `zh-CN`；
- 不读取浏览器首选语言，不提供运行时语言选择器，也不宣称支持 `Accept-Language`；
- 后端 Catalog 的 title/description、错误展示文案和前端静态文案都按简体中文交付；
- `operation_id`、路由、错误码、日志字段、指标名和存储枚举是机器合同，不参与本地化。

`pnpm check` 会运行 `verify:locale-contract`，防止 HTML 语言声明与 `Intl` 格式化在
不同机器上悄然漂移。frontend-v2 的文案集中在组件内联中文 + `src/lib/product-locale.ts`
的 locale 常量；新增第二语言前必须满足重新开门条件（与旧前端同一份产品合同）。

## 自动重新开门条件

出现以下任一事实时，i18n 立即成为 P0 发布门槛：

1. 产品、客户合同、采购或法规要求第二种语言；
2. 同一语言需要第二个地区格式、时区或计量/货币规则；
3. 服务端需要按请求语言生成 Catalog、通知、导出或错误展示文案；
4. 无障碍测试要求覆盖另一种语言的可访问名称、阅读顺序或文本扩张。
