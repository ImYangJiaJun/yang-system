# xlsx 解析测试夹具

由 `python scripts/make_xlsx_fixtures.py` 生成，**产物提交进 git**。

## 为什么自造而不用真实文件

仓库里有真实数据 `docs/境内银行网点信息管理-{1,2}.xlsx`（7.4 MB，15.4 万行），
但它们被 `.gitignore` 的 `docs/*.xlsx` 忽略，**CI 上不存在**，
不能做夹具。真实文件只在本地做人工验证用。

## 为什么手写 XML 而不引依赖

xlsx 是装着若干 XML 的 zip。用标准库 `zipfile` 手写最小结构，
夹具就能在任何有 Python 的环境里复现，不引入新的构建依赖。

## 每个夹具的意义

主清单见 `docs/architecture/feishu-bank-branch-datasource-tasklist.md` Task 1 的表格。
下列夹具未收录在该表格中：

- `shared_strings_missing_data_row.xlsx`：表头用 inlineStr、数据行首个单元格为数值且
  共享字符串部件缺失，触发 `read_header` 换行与 `read_snapshot` 的缺表守卫。
- `blank_row_between.xlsx`：数据行之间夹空行，验证 `rows_read` 只数非空行、空快照守卫生效。
- `date_cells.xlsx`：数值序列号日期（带日期格式）与 ISO `t="d"` 日期共存，验证两种形态
  都能读成可读日期。
- `overlong_padded_value.xlsx`：305 字符、首尾带空格的超长值，验证截断到 255 后与 trim
  后文案不一致时的 key 派生边界。

**改夹具必须重跑生成脚本**，不要手改二进制。

资源边界回归另含：`shared_string_oversized.xlsx`（1 MiB 共享字符串、3 次引用）、
`shared_string_repeated.xlsx`（4 KiB 字符串、32 次引用，用小预算验证累计展开），以及
`zip_batch_budget.xlsx`（真实 129 MiB 可压缩扩展部件，验证两文件累计 ZIP 预算）。
最后一个部件不交给 calamine 解压，文件本身约 130 KiB；生成器分块写入。
