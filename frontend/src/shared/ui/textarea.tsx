import * as React from "react";

import { cn } from "@/shared/lib/utils";

/**
 * 多行文本输入。与 [`Input`](./input.tsx) 同一套视觉规格，只是高度由内容决定。
 *
 * 单独加这个 primitive 是为了 JSON 录入：级联映射是
 * `{"控件代码":{"parent_source_key":…,"parent_field":…,"cascade_field":…}}`，
 * 塞进单行输入里既看不全也改不动，而看错一个字段名不会报错——它只会让该数据源
 * 静默退化成「无级联」。
 *
 * 高度用 `min-h` 而不是固定 `h-9`：内容少时不该留一大片空白，内容多时不该出现
 * 内部滚动条把后续字段挤下去。
 */
function Textarea({ className, ...props }: React.ComponentProps<"textarea">) {
  return (
    <textarea
      data-slot="textarea"
      className={cn(
        "placeholder:text-muted-foreground dark:bg-input/30 border-input flex min-h-[4.5rem] w-full min-w-0 rounded-md border bg-transparent px-3 py-2 text-base shadow-xs transition-[color,box-shadow] outline-none disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50 md:text-sm",
        "focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-[3px]",
        "aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40 aria-invalid:border-destructive",
        className,
      )}
      {...props}
    />
  );
}

export { Textarea };
