/**
 * 导入进度的**观测侧**：轮询 hook + 一句话文案。
 *
 * # 为什么是一个独立的非组件模块
 *
 * `describeImportProgress` 是纯函数、要被单测直接引用；放进任何 `.tsx` 里都会让那份文件
 * 同时导出组件与非组件，撞上 react-refresh 的 `only-export-components`
 * （lint 是 `--max-warnings 0`）。所以两者都放这里，组件只 import。
 *
 * # 为什么是 1 秒轮询，而不是 SSE / WebSocket
 *
 * 框架全仓没有流式能力（见设计文档 §5.8.1），而进度本身是**进程内的旁路观测**：
 * 一次导入几十秒到几分钟，秒级粒度足够，断了也只是少看几拍——轮询把「断线」这件事
 * 降级成「下一次请求」，不需要重连状态机。
 *
 * # 三条口径（**不许顺手统一**，它们与回执刻意不同）
 *
 * - `rowsDone` 是**物理**行序号（含空行），回执里的 `rowsRead` 是**非空**数据行数；
 * - `filesDone` 是「读完**并校验过表头**」的文件数，不是「上传完成」的文件数；
 * - `bindingsDone` 是「已开始处理」的绑定数，所以它**到不了** `bindingsTotal`
 *   ——显示成 `k/n` 时别把它读成「还剩 n-k 条没写」。
 */

import { useEffect, useState } from "react";

import type { ImportProgress, XlsxImportClient } from "./api";

/// 轮询间隔。与详情页的 `PULL_POLL_MS` 同量级（秒级观测，不是实时通道）。
export const IMPORT_PROGRESS_POLL_MS = 1_000;

/// 每 1 秒问一次这条数据源的导入进度；`datasourceId === null` 时**一次都不问**。
///
/// 传 `null` 是调用方的「现在没有导入在跑」：向导在提交前、对话框在 pending 之外都是
/// 这一态。拿它当开关（而不是在组件里自己 `if`）是为了让「什么时候开始/停止轮询」
/// 只有一处判断，卸载与置 null 走的是同一个 cleanup。
///
/// 失败**一律吞掉**：目录还没加载完、滚动发布把请求打到另一色实例、网络抖动，
/// 这三件事都不该让正在跑的导入失败，也不该把上一拍的好数据抹掉——保留旧值比闪回一个
/// 「不确定」态更接近事实（它下一秒大概率就回来了）。
export function useImportProgress(
  client: Pick<XlsxImportClient, "importProgress">,
  datasourceId: number | null,
): ImportProgress | null {
  const [progress, setProgress] = useState<ImportProgress | null>(null);

  useEffect(() => {
    if (datasourceId === null) return;
    let alive = true;
    // **先清掉上一轮的快照**：向导重试会复用同一个 datasourceId（`created` 非空就不再
    // 建源），于是 id 走的是 null → **同一个**非空值——不清的话，新一轮首拍落地前
    // （≤1s）界面显示的是上一轮失败前那一拍（例如「正在写入选项：已完成 3/5 条绑定…」），
    // 而服务端此刻答的是 loading。不确定态是允许的，上一轮的旧快照不是。
    setProgress(null);
    const tick = () => {
      client
        .importProgress(datasourceId)
        .then((next) => {
          // 卸载（或 datasourceId 变 null）之后落地的响应一律丢弃：否则会写一份
          // 已经不属于当前这次导入的快照。
          if (alive) setProgress(next);
        })
        .catch(() => undefined);
    };
    // **先立刻问一次**再挂定时器：否则第一秒里界面只能显示「正在上传」那种不确定态。
    tick();
    const timer = setInterval(tick, IMPORT_PROGRESS_POLL_MS);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [client, datasourceId]);

  return datasourceId === null ? null : progress;
}

/// 进度快照 + 本地那批文件 → 一句人话。
///
/// `idle`（或还没问到）显示的是**不确定态**：整批文件已经发出去了，服务端此刻在
/// transport 层收 body，进程内的阶段还没开始上报，只有上传动作本身是我们知道的。
/// **绝不能**在这种时候说「没有导入在跑」——蓝绿双实例下进度轮询可能落到另一色，
/// 它恒答 idle（见设计 §5.8.1）。
///
/// 数字一律 `String(n)` / `toFixed(1)`：`toLocaleString` / `Intl` 会把千分位、小数点
/// 按浏览器语言格式化，撞单语言产品合同（`verify:locale-contract`）。
export function describeImportProgress(
  progress: ImportProgress | null,
  files: readonly File[],
): string {
  const fileCount = files.length;
  const megabytes = (
    files.reduce((total, item) => total + item.size, 0) /
    (1024 * 1024)
  ).toFixed(1);
  const uploading = `正在上传文件（共 ${fileCount} 个，${megabytes} MB）…`;
  if (progress === null) return uploading;

  switch (progress.stage) {
    case "loading": {
      // 服务端刚建好条目、还没走到第一次上报时两个计数都是 null（阶段却是 loading）：
      // 这时拿**本地**的文件数当分母是诚实的（那正是我们发过去的个数），
      // 显示「0/0 个文件」才是假话。
      const done = progress.filesDone ?? 0;
      const total = progress.filesTotal ?? fileCount;
      return `正在读取并校验表头（已完成 ${done}/${total} 个文件）…`;
    }
    case "parsing": {
      const index = progress.fileIndex ?? 1;
      const totalFiles = progress.filesTotal ?? fileCount;
      const done = progress.rowsDone ?? 0;
      const rowsTotal = progress.rowsTotal;
      const prefix = `正在解析数据：第 ${index}/${totalFiles} 个文件，已读 ${done}`;
      // 分母来自文件里的 `<dimension>`：读不到就是 `null`，此时**不画假分母**。
      // `done > rowsTotal` 也走这一支——它说明分母是陈旧的（比如文件在导出过程中
      // 被追加过），画出来就是一句「已读 5 / 共 3 行」的假话。
      return rowsTotal === null || done > rowsTotal
        ? `${prefix} 行…`
        : `${prefix} / 共 ${rowsTotal} 行…`;
    }
    case "writing": {
      const done = progress.bindingsDone ?? 0;
      const total = progress.bindingsTotal ?? 0;
      return `正在写入选项：已完成 ${done}/${total} 条绑定…`;
    }
    // 认不出的阶段（服务端加了新阶段而这份前端还没跟上）与 idle 同待遇：
    // 退回不确定态，而不是把新阶段的字段按旧阶段读成一句假话。
    default:
      return uploading;
  }
}
