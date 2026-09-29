/**
 * xlsx 数据源的「重新导入」：选一批文件 → 直接调导入端点。
 *
 * # 它为什么**不走建源**
 *
 * 绑定（勾了哪些列、每列的源标识与父列）在建源那一刻就落定了，此后不会变。所以更新
 * 数据只要再喂一批**表头一致**的文件——服务端按启用绑定的列名去文件里取列，多出来的
 * 列忽略、缺列即拒（严格口径能成立的前提正是「绑定已配好、列名不会再变」）。
 *
 * 走建源那条路重来一遍的代价不是多两步：`create_datasource_table` 没有幂等键，
 * 同名会撞重名，而且会把已经发给飞书控件的 `source_key` 整批换掉——那些控件当场
 * 取不到选项。
 *
 * # 与建源向导的分工
 *
 * 这里**不问名称、不问列、不问源标识**，只问文件；回执也只说「读了什么、写了多少」。
 * 配置类的事实属于详情页上面那块（字段绑定表与凭据清单），两者不要在同一屏里重复。
 *
 * # 数据从哪来
 *
 * 与两个向导同一个注入范式：只经 `client.importFiles` 访问数据（真实实现在
 * `api.ts::useXlsxImportClient`），组件不摸会话与目录。
 */

import { useId, useState } from "react";

import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";

import type { XlsxImportClient, XlsxImportReport } from "../api";
import { describeImportProgress, useImportProgress } from "../import-progress";

export type XlsxReimportDialogProps = {
  open: boolean;
  /// 要导入到哪条数据源（表级主键）。
  datasourceId: number;
  /// 只要导入与查进度两个入口——建源与探表头都不该出现在这条路上（类型这么写，
  /// 传进来的 `useXlsxImportClient()` 也只被用到这两个方法）。
  client: Pick<XlsxImportClient, "importFiles" | "importProgress">;
  /// 导入成功：调用方据此回读选项、给回执并关掉对话框。
  ///
  /// 交回整份回执（而不是一个布尔）是因为回执里那几行（读了 N 行 → 派生 M 个选项、
  /// 跳过、异常行数）就是「这一轮到底发生了什么」的唯一记录。
  onImported: (report: XlsxImportReport) => void;
  onCancel: () => void;
};

export function XlsxReimportDialog({
  open,
  datasourceId,
  client,
  onImported,
  onCancel,
}: XlsxReimportDialogProps) {
  const idPrefix = useId();
  const [files, setFiles] = useState<File[]>([]);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /// 导入期间的实时进度。**15 万行那一档真正走的入口**（建源向导只走一次，此后每次
  /// 更新数据都经过这里）：那一跑是几分钟，除这行字之外没有任何东西能说明「它还在动」。
  /// pending 之外不轮询——这条源平时没有导入在跑，每秒问一次只是白打服务端。
  const progress = useImportProgress(client, pending ? datasourceId : null);

  function close() {
    setFiles([]);
    setError(null);
    onCancel();
  }

  async function submit() {
    setPending(true);
    setError(null);
    try {
      onImported(await client.importFiles(datasourceId, files));
      setFiles([]);
    } catch (cause) {
      // 服务端拒掉的原文留在对话框里（缺列、表头不一致、这条源已停用都是它说的），
      // 换一批文件就能重试——不需要重新走建源。
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !pending) close();
      }}
    >
      <DialogContent showCloseButton={!pending}>
        <DialogHeader>
          <DialogTitle>重新导入</DialogTitle>
          <DialogDescription>
            上传一批新的
            xlsx，整份替换这条数据源的选项。绑定（勾了哪些列、源标识与父列）
            保持不动——所以文件的表头必须与已落定的那几列完全一致：缺列会被整批拒掉，
            多出来的列忽略。
          </DialogDescription>
        </DialogHeader>

        {error === null ? null : (
          <p
            role="alert"
            className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
          >
            {error}
          </p>
        )}

        <div className="space-y-1.5">
          <Label htmlFor={`${idPrefix}-files`}>xlsx 文件</Label>
          <Input
            id={`${idPrefix}-files`}
            type="file"
            multiple
            accept=".xlsx"
            onChange={(event) => setFiles(Array.from(event.target.files ?? []))}
          />
          <p className="text-xs text-muted-foreground">
            可以一次选多份：它们的表头必须完全一致。文件只留在浏览器里，直接上传。
          </p>
        </div>

        {pending ? (
          <p role="status" className="text-xs text-muted-foreground">
            {describeImportProgress(progress, files)}
          </p>
        ) : null}

        <DialogFooter>
          <Button variant="ghost" onClick={close} disabled={pending}>
            取消
          </Button>
          <Button
            disabled={pending || files.length === 0}
            onClick={() => void submit()}
          >
            {pending ? "正在导入…" : "开始导入"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
