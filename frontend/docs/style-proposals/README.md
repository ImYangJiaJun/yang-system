# 整体样式优化方案

本目录包含整体样式方向的提案与配套 token 示例文件，供对比选型。
打开 `preview.html` 可在同一套模拟界面上直接切换各方案查看效果（AB 方案还支持浅色/深色模式切换）。
每个 `scheme-*.css` 是对应方案的设计 token 层，选定后替换/合并进 `src/css/app.css` 的 `:root` 段即可预览效果。

## 现状问题

- 全部样式集中在 `src/css/app.css`（约 2100 行），维护成本高。
- 品牌色有两份：`quasar.config.ts` 的 `brand.primary: #004976`（Quasar 组件使用）
  与 `app.css` 的 `--ys-primary: #075d66`（自定义样式使用），界面实际为混色。
- 存在两套头部渐变（`.app-header` 与 `.formal-header`），视觉语言不统一。

## 方案总览

| 方案                           | 风格                                    | 参考实例                                  | 改动量                           | 适用场景                   |
| ------------------------------ | --------------------------------------- | ----------------------------------------- | -------------------------------- | -------------------------- |
| AB 精修主题（浅色/深色可切换） | 中性灰 + 细边框 + 微阴影，亮/暗双 token | Linear、Stripe Dashboard、Vercel、Grafana | 中（token + Quasar Dark plugin） | 默认推荐，一套语言两种模式 |
| C 企业蓝高密度                 | #1677ff 企业蓝 + 紧凑表格               | Ant Design Pro、阿里云控制台              | 中（token + 表格密度）           | 企业内部高频录入           |
| D 强化渐变科技风               | 保留并统一渐变、玻璃拟态                | Stripe 官网、Raycast                      | 小（统一现有语言）               | 对外演示、品牌展示         |

## 各方案要点

### AB — scheme-ab-adaptive.css（由原 A、B 合并）

- 浅色：中性灰底 + 细边框 + 单层微阴影，圆角收窄（10/14/20 → 6/8/12），头部去渐变。
- 深色：纯黑底 + 高对比 teal 点缀，组件用描边而非阴影分层。
- 切换：Quasar `Dark` plugin（`$q.dark.toggle()`），token 用 `body.body--dark` 覆盖，
  首次进入可 `$q.dark.set("auto")` 跟随系统；文件底部附切换按钮示例代码。
- 表单控件交互（preview.html 有完整可点演示）：
  hover 边框加深、focus 主色光环（3px）、校验错误抖动 + 错误色光环、
  开关弹性滑动、按钮 hover 提亮 / active 按压 / focus-visible 光环、禁用态独立底色。
- 参考：https://linear.app 、Stripe Dashboard、https://ui.shadcn.com 、https://vercel.com 、Grafana。

### C — scheme-c-enterprise-blue.css

- 主色换 `#1677ff`，表格行高 56px → 40px，提高信息密度。
- 参考：https://preview.pro.ant.design 、阿里云/腾讯云控制台。

### D — scheme-d-gradient.css

- 不推翻现有设计，统一两套渐变与主色，加强玻璃拟态一致性。
- 参考：https://stripe.com 、https://www.raycast.com

## 无论选哪个都建议先做

1. 拆分 `app.css` 为 `tokens.css` / `layout.css` / `components.css`，后续改样式只动 `tokens.css`。
2. 统一品牌色来源：`quasar.config.ts` 的 brand 与 CSS 变量收敛为同一份 token。
