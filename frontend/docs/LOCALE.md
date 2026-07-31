# 语言与地区产品合同

<!-- locale-contract: supported zh-CN -->
<!-- locale-contract: runtime-switch disabled -->
<!-- locale-contract: reopen-trigger second-locale -->

## 当前发布范围

yang-system 首发只支持简体中文 `zh-CN`。这是明确的产品范围，不是已经实现国际化后的
默认语言：

- HTML 文档语言、Quasar 组件语言、日期/数字格式和本仓库业务文案统一为 `zh-CN`；
- 不读取浏览器首选语言，不提供运行时语言选择器，也不宣称支持 `Accept-Language`；
- 后端 Catalog 的 title/description、错误展示文案和前端静态文案都按简体中文交付；
- `operation_id`、路由、错误码、日志字段、指标名和存储枚举是机器合同，不参与本地化。

在目标用户、合同、采购和法规均只要求简体中文时，未引入翻译框架不是发布阻塞项。
`pnpm check` 会运行 `verify:locale-contract`，防止 HTML、Quasar 和 `Intl` 格式化在不同
机器上悄然漂移。

## 自动重新开门条件

出现以下任一事实时，i18n 立即成为 P0 发布门槛，不能继续沿用“中文即可”的结论：

1. 产品、客户合同、采购或法规要求第二种语言；
2. 同一语言需要第二个地区格式、时区或计量/货币规则；
3. 服务端需要按请求语言生成 Catalog、通知、导出或错误展示文案；
4. 无障碍测试要求覆盖另一种语言的可访问名称、阅读顺序或文本扩张。

## 第二语言上线前的验收

重新开门后至少要同时完成：

- 建立静态文案、Catalog 文案和后端错误码到展示文案的唯一所有权，禁止混合半翻译；
- 定义 locale 协商、合法白名单和失败回退，并让 Catalog revision/ETag/Vary 按 locale
  隔离，避免跨语言缓存污染；
- 日期、数字、时区、货币、复数和排序统一通过显式 locale/zone 格式化；
- 路由、会话恢复、导出/通知和服务端渲染（若引入）保持同一 locale；
- 用伪本地化发现截断和硬编码，并为每个支持语言运行单元、生产构建、Playwright、axe
  与人工屏幕阅读器检查；
- 记录缺失翻译门禁、回退观测指标、译文审批和回滚方案。

在这些条件完成前，文档只能写“单语言 `zh-CN` 产品”，不能写“支持 i18n”。
