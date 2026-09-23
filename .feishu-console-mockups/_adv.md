总发现 15，确认 11，驳回 1

## 确认（要修）
1. [major] 「当前页被清空」被当成「没有匹配的结果 / 一个数据源都没有」：整页给出与事实相反的结论，同时分页控件整个消失
   loc: D:\code\lib_yang\project\yang-system\frontend\src\features\feishu\views\DatasourceListPage.tsx:109-118（分支链 393-396，分页渲染 424-433，停用/启用未回第 1 页 287-306）
2. [minor] 从「有结果」切到「空结果」时会整屏闪出四步建档空态（把 stale 的空数组与当前筛选状态拼在一起判断）
   loc: D:\code\lib_yang\project\yang-system\frontend\src\features\feishu\views\DatasourceListPage.tsx:112-118
3. [minor] 预检回执把「第一页条数」当成选项总数：>100 条时显示 100，且忽略响应里的 hasMore
   loc: D:\code\lib_yang\project\yang-system\frontend\src\features\feishu\api.ts:466-471（消费处 components\TokenPrecheckNotice.tsx:96）
4. [minor] 预检失败文案把「Token 其实是对的」的两种失败也说成「没有通过验证」
   loc: D:\code\lib_yang\project\yang-system\frontend\src\features\feishu\components\TokenPrecheckNotice.tsx:69-73（码含义见 types.ts:146-165）
5. [minor] 预检回执永不清除：删除该数据源后仍写着「数据源已创建」，其「重新填写 Token」会把用户送进死路
   loc: D:\code\lib_yang\project\yang-system\frontend\src\features\feishu\views\DatasourceListPage.tsx:100-101、182-189、325-338（对照 291-303 的停用/启用与 262-285 的删除都不碰 precheck）
6. [minor] 「编辑数据源」对话框不给改「加密返回 / 默认语言」，但 update_datasource 收这两个字段——默认语言选错后控制台没有任何修复入口
   loc: D:\code\lib_yang\project\yang-system\frontend\src\features\feishu\components\DatasourceFormDialog.tsx:280-322（对照 api.ts:364-369 与后端 src/addon/feishu/datasource/actions/update_datasource.rs:34-40）
7. [major] 停用/启用（走 update 的那条确认分支）改完不把页码归位：结果集变小后停在空页，页面给出「没有匹配的数据源」这个假结论，分页控件同时消失
   loc: frontend/src/features/feishu/views/DatasourceListPage.tsx:294（对照同文件 274 行的删除分支）
8. [minor] 详情页把「数据源不存在」当成「0 选项」，并断言「这个数据源本身是好的」——把不存在的数据源说成健康、还把用户引向多维表格自动化
   loc: frontend/src/features/feishu/views/DatasourceDetailPage.tsx:246（文案在 277 行）
9. [minor] 详情页只认 option.read、完全不看 datasource.read：既让无数据源读权限的身份进入页面，又在 403 文案里断言「当前身份可以看数据源本身」
   loc: frontend/src/features/feishu/views/DatasourceDetailPage.tsx:193
10. [major] 删除确认对话框不指名被删对象：正文之外连 source_key 也不显示，用户无法确认删的是哪一条
   loc: D:/code/lib_yang/project/yang-system/frontend/src/features/feishu/components/ConfirmDialog.tsx:96-101（页面在 views/DatasourceListPage.tsx:453 明明把 sourceKey 传了进来）
11. [minor] 详情页 0 选项空态断言「这个数据源本身是好的」，而该页无法证明这个数据源存在
   loc: D:/code/lib_yang/project/yang-system/frontend/src/features/feishu/views/DatasourceDetailPage.tsx:276（OptionEmptyState 整体在 270-287）

## 驳回（不要改）
1. 清除筛选（或把状态筛选切回「全部」）后会闪出「还没有数据源」四步建档指引，工具栏一起消失——把「筛选没结果」误报成「一个数据源都没有」
   为何驳回: 按它自己给的复现路径（列表里已有 25 条 → 输入 zzz → 点「清除筛选」），这一屏不会出现，因此这条发现站不住；但底层确实存在一个更窄、触发条件与它所述不同的真问题。

一、机制里「代码确实这么写」的部分成立
DatasourceListPage.tsx:112-113 的 filtering 取的是去抖后的 controller.query（list-query.ts:183-194，搜
