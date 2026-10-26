# XLSX 累计资源与阻塞任务生命周期修复

范围：P09 / N6。对应审核 `2026-10-25-infrastructure-review-feishu-outbound.md` 的 N6。
本地实现与验证记录，不表示生产验收或多实例同源写入互斥已完成。

## 边界

- 单文件上传仍为 16 MiB，multipart 总体仍为 17 MiB；未读取或修改真实配置。
- 既有每文件 256 MiB ZIP 声明解压上限增加整批累计限制；不会因拆成多个文件重置。
- 整批复制进表头/快照的文本与列名累计上限为 256 MiB。共享字符串先借用、先计费，再复制；不在超限后返回部分成功。
- 整批处理量最多 4,194,304 个单元格、1,048,576 行；包括未选列、空单元格及表头探测的实际遍历，稀疏行按物理行跨度计费。表头探测与快照遍历重复经过的少量行/格也计入处理量。
- 表头探测触及的文本与选中列单格最多 32,767 个 UTF-16 单元，匹配 Excel 单格文本能力。未选列的数据不复制，仍计入遍历格数/行数。现有 255 字符业务文案截断/停用异常行语义保持；需读取但超过解析能力的单格整批拒绝。
- 一个 `FeishuContext` 的克隆共享 1 个解析名额，probe 与导入共用，忙时返回既有 429 限流错误。名额在读上传文件前取得，导入解析成功后继续持有至持久化结束。

## 所有权与取消

probe 的同步解析迁入 `spawn_blocking`。导入把 permit 和 `ImportGuard` 一起移进同一阻塞闭包，成功后随快照交回异步写入阶段。请求 future 取消只设置原子取消标志；已运行的阻塞工作退出前不会释放名额或同源占用，也不会提前显示 idle。

解析在调用 calamine 前、遍历单元格及进度回调后检查取消。calamine 急切加载 sharedStrings 的单次调用内部没有协作检查入口，因此取消可能延迟至该调用返回；资源凭证在这段时间仍由实际工作持有。取消不回滚此前已经提交的逐绑定事务。

## 验证记录

- 首个红测实际失败：约 KiB 级 ZIP 中 1 MiB sharedString 被 3 行引用，旧解析接受并复制，预期拒绝。`target/infrastructure-remediation/xlsx/red-shared-string.log`，退出 101，失败来自回归断言。
- 只读检查本地两份既有银行文件：压缩字节 4,803,614 / 2,594,902；ZIP 声明解压字节 50,138,565 / 27,003,183；物理行 100,001 / 54,387；单元格 800,008 / 435,096。所设行/格上限保留这一已知规模的余量。日志 `target/infrastructure-remediation/xlsx/local-file-capacity.log`；这只是文件容量检查，不是导入成功或 RSS 证明。
- `cargo test --lib xlsx --locked -j 1`：49 passed、0 failed、0 ignored，退出 0；初次日志 `target/infrastructure-remediation/xlsx/green-unit.log`，九项变异恢复后的复验日志 `green-unit-final.log`。复验包含第二个文件第 17 行立即拒绝的停止位置断言。覆盖跨文件文本/ZIP/格数/行数预算、精确字节边界、共享字符串、遍历取消、连续两次请求取消时仍由阻塞任务持有 permit 与 ImportGuard、解析成功后异步写入期保留资源。
- 三个资源夹具以生成器重新构建后逐字节一致；日志 `target/infrastructure-remediation/xlsx/fixture-reproducibility.log`。
- `cargo test --test feishu_xlsx_import_integration --locked -j 1 -- --ignored --test-threads=1`：真实隔离 MySQL（64533）与 Redis（64536，DB 15），19 passed、0 failed、0 ignored，退出 0，测试 7.53 秒。日志 `target/infrastructure-remediation/xlsx/integration.log`；经 Registry 派发，不等于真实 HTTP multipart 传输验证。
- 九项摘除式验证全部取得真实断言失败（每项退出 101，均包含 `test result: FAILED`，无编译失败），随后恢复代码并重新通过上述 49 项。汇总 `target/infrastructure-remediation/xlsx/mutation-results.json`，各项原始日志 `mutation-<名称>.log`。

| 要求 | 回归入口 | 当前证据 |
| --- | --- | --- |
| 共享字符串不先复制再检查 | `rejects_a_shared_string_before_copying_it_into_snapshot_rows`、`the_text_budget_accepts_its_exact_boundary_and_rejects_one_byte_more` | 红测复现；单测绿 |
| 展开文本跨文件累计 | `repeated_shared_strings_are_charged_before_each_copy_and_across_files` | 小预算真实 XLSX，单测绿 |
| 未选列仍计工作量，行/格跨文件累计 | `cell_and_row_budgets_include_unselected_data_and_accumulate_across_files` | 单测绿 |
| ZIP 声明解压量跨文件累计 | `zip_expansion_budget_is_shared_by_all_files` | 真实 129 MiB 部件的单文件通过、两文件拒绝；单测绿 |
| 遍历期间观察取消 | `cancellation_during_traversal_stops_before_retaining_the_next_row` | 第二行进度回调取消，未复制第二行；单测绿 |
| 请求取消不提前释放名额/同源占用 | `cancelled_blocking_work_keeps_the_slot_and_import_guard_until_it_exits` | 连续两次受控阻塞工作，取消后仍互斥，实际退出后释放；单测绿 |
| 解析不阻塞单线程 Tokio，成功后仍持有资源 | 上项 current_thread 回归、`successful_parse_keeps_resources_owned_through_the_async_write_phase` | 异步心跳及写入阶段持有验证，单测绿 |
| 超限不改变持久快照 | `resource_rejection_preserves_the_snapshot_and_releases_import_ownership` | 真实 MySQL/Redis 通过：旧快照保持、任务回 idle、随后 probe 成功 |
| 两个 Action 共用容量，覆盖写库等待 | `a_database_blocked_import_keeps_the_shared_probe_capacity` | 真实 MySQL 行锁屏障通过：写入等待时 probe 返回限流，释放行锁后恢复 |

已完成的关键摘除式共九项：展开字节（`expanded-bytes`）、格数（`cell-count`）、行数（`row-count`）、整批 ZIP（`zip-batch`）、单格长度（`cell-length`）、解析取消检查（`parse-cancel`）、请求取消信号（`request-cancel`）、permit 所有权（`permit-owner`）、ImportGuard 所有权（`guard-owner`）。最后两项分别摘除，分别被“取消不能提前释放全局容量”与“取消不能提前放行同源导入”断言抓住。每项恢复后才进入下一项；编译失败不计作有效变异。

## 证据边界

ZIP 预算仍以中央目录声明为预检依据；本修复没有声称验证过伪造大小字段对底层 ZIP 解压器的所有影响。实际快照文本、格数、行数有独立计数，不依赖 ZIP 压缩比。字节和对象计数不等于 RSS 上限；calamine 临时对象、HashMap/Vec 容量和写库派生对象也占内存，未以源码计算冒充峰值测量。

名额与导入占用是本应用实例内的边界。不同进程、不同 `BuiltApp`、蓝绿实例之间的同源写者约束属于独立事项，不由这次修改保证。
