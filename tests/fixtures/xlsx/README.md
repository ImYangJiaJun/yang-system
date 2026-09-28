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

见 `docs/architecture/feishu-bank-branch-datasource-tasklist.md` Task 1 的表格。
**改夹具必须重跑生成脚本**，不要手改二进制。
