/**
 * 编辑一条表级数据源：改名称。
 *
 * # 为什么只有「名称」一项
 *
 * 一条表级数据源身上有三样东西，但它们的编辑入口各不相同：
 * - **字段绑定**（哪几列、各自进 URL 的 `source_key`、级联父列）归配置向导——
 *   它是创建时定下来的，而且后端更新端点是**整份替换**语义，拆一半出来改只会漂移；
 * - **坐标**（Base Token / 数据表 / 视图）同样在向导里第一次配好，列表页只读展示；
 * - **名称**是唯一一项「就地改一下」有意义的，也正是列表上唯一一眼能看见的东西。
 *
 * # 为什么提交物里还要带上绑定集合
 *
 * 后端 `update_datasource_table` 的 `fields` 是**必填**（整份替换）。所以这里把这一行
 * 当前的绑定原样带回——先过 [`enabledBindingInputs`]，只送启用中的那几条：
 * 服务端对「集合里出现」的已有绑定会写 `enabled = true`，把停用的塞回去等于
 * 悄悄把它重新启用，而用户这次只是改了个名字。
 */

import { useEffect, useId, useState } from "react";

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

const TITLE_ERROR = "名称必须在 1..=100 字符。";

const EMPTY_BINDING_NOTE =
  "这条数据源没有启用中的字段绑定，而服务端的更新接口要求至少带一条绑定。" +
  "先用配置向导补一条，再回来改名称。";

export type DatasourceEditSubmission = {
  datasourceId: number;
  title: string;
};

export type DatasourceEditDialogProps = {
  open: boolean;
  /// 目标数据源的主键。
  datasourceId: number;
  /// 名称的现值。**必须来自那一行**，不要拿控件的缺省值顶上。
  initialTitle: string;
  /// 这条数据源当前**启用中**的绑定条数。
  ///
  /// 只为 0 时给出「改不了」的说明：后端要求 `fields` 至少一条，此刻提交必然被拒。
  enabledBindingCount: number;
  pending?: boolean;
  serverError?: string | null;
  onSubmit: (submission: DatasourceEditSubmission) => void;
  onCancel: () => void;
};

export function DatasourceEditDialog({
  open,
  datasourceId,
  initialTitle,
  enabledBindingCount,
  pending = false,
  serverError = null,
  onSubmit,
  onCancel,
}: DatasourceEditDialogProps) {
  const fieldId = useId();
  const [title, setTitle] = useState(initialTitle);
  const [localError, setLocalError] = useState<string | null>(null);

  // 每次打开都从那一行的现值重新开始：留着上一次的输入会让人把 A 的名字
  // 存到 B 上（列表页的行会随翻页/回读整批换掉）。
  useEffect(() => {
    if (!open) return;
    setTitle(initialTitle);
    setLocalError(null);
  }, [open, initialTitle, datasourceId]);

  const trimmed = title.trim();
  const titleValid = trimmed.length >= 1 && trimmed.length <= 100;
  const canSubmit = titleValid && enabledBindingCount > 0 && !pending;

  function submit() {
    if (!titleValid) {
      setLocalError(TITLE_ERROR);
      return;
    }
    setLocalError(null);
    onSubmit({ datasourceId, title: trimmed });
  }

  const error = localError ?? serverError;

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !pending) onCancel();
      }}
    >
      <DialogContent showCloseButton={!pending}>
        <DialogHeader>
          <DialogTitle>编辑数据源</DialogTitle>
          <DialogDescription>
            这里改的是名称。字段绑定与坐标由「添加数据源」的配置向导定下，
            改绑定请重新跑一次向导。
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

        {enabledBindingCount === 0 ? (
          <p className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm">
            {EMPTY_BINDING_NOTE}
          </p>
        ) : null}

        <div className="space-y-1.5">
          <Label htmlFor={`${fieldId}-title`}>名称</Label>
          <Input
            id={`${fieldId}-title`}
            value={title}
            onChange={(event) => setTitle(event.target.value)}
            autoComplete="off"
          />
          <p className="text-xs text-muted-foreground">
            1..=100 字符。它只是展示用名称，不进任何接口地址。
          </p>
        </div>

        <DialogFooter>
          <Button variant="ghost" disabled={pending} onClick={onCancel}>
            取消
          </Button>
          <Button disabled={!canSubmit} onClick={submit}>
            {pending ? "提交中…" : "保存"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
