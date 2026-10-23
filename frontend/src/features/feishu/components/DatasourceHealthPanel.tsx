/**
 * 数据源体检面板：勾选的列还在不在（`health_check`）。
 *
 * # 结论必须分开说
 *
 * | 情形 | 界面 |
 * |---|---|
 * | 字段被**改名** | **不进这里**：每轮按 `field_id` 解析当前名字，改名能自愈 |
 * | 字段被**删除** | 逐列点名，**字段名优先**，同时给 `field_id` 与 `source_key` |
 * | 字段名**重复** / 类型**不支持** | 各自一段，逐条给名字、原因与标识 |
 * | 表 / 视图没了 | 各自一句，不和缺字段混为一谈 |
 * | 这次**没查成** | 单列一段并压住「通过」——「查不了」不是「没问题」 |
 * | 远端有、本地未绑定 | 「可新增」清单，**不是故障**，不压 `ok` |
 *
 * 只报 `field_id` 业务人员看不懂，只报 `source_key` 又定位不到列，所以两个都给；
 * 但**主标识是字段名**——`fieldName` 缺失时才退回一句明确的「未知字段名」，
 * 绝不拿 `field_id` 冒充名字。
 */

import { Button } from "@/shared/ui/button";
import { Skeleton } from "@/shared/ui/skeleton";

import type { HealthReport } from "../types";
import { fieldTypeLabel } from "../types";

export type DatasourceHealthPanelProps = {
  /// `null` = 还没体检过（或这个部署没有体检端点）。那时**不渲染一个空结论**：
  /// 把「不知道」画成绿字是最容易犯的错。
  report: HealthReport | null;
  pending?: boolean;
  /// 体检请求本身失败（网络 / 权限 / 出站未启用）时的原文。
  error?: string | null;
  onRecheck?: () => void;
};

function typeLabel(fieldType: number | null): string {
  return fieldType === null ? "类型未知" : fieldTypeLabel(fieldType);
}

export function DatasourceHealthPanel({
  report,
  pending = false,
  error = null,
  onRecheck,
}: DatasourceHealthPanelProps) {
  if (error !== null) {
    return (
      <div className="space-y-3">
        <p
          role="alert"
          className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          {error}
        </p>
        {onRecheck ? (
          <Button variant="outline" size="sm" onClick={onRecheck}>
            重新体检
          </Button>
        ) : null}
      </div>
    );
  }

  if (report === null) {
    // 还没有结论：给「正在查」或一个入口，**不给任何判断**。
    // 连入口都没有（调用方没给 `onRecheck`）时整个不渲染——
    // 一块写着「还没体检过」的空面板只是占地方。
    if (pending) return <Skeleton className="h-16 w-full" />;
    if (!onRecheck) return null;
    return (
      <div className="flex flex-wrap items-center gap-3">
        <p className="text-sm text-muted-foreground">还没体检过。</p>
        <Button variant="outline" size="sm" onClick={onRecheck}>
          重新体检
        </Button>
      </div>
    );
  }

  const problems: string[] = [];
  if (report.tableMissing) {
    problems.push(
      "数据表已不存在（或本应用已没有权限读到它）。勾选的字段一个都拉不到了，需要改坐标或把应用加为文档协作者。",
    );
  }
  if (report.viewMissing) {
    problems.push(
      "视图已不存在。拉取会按视图筛行，视图没了就只能改成「整表拉取」或换一个视图。",
    );
  }

  return (
    <div className="space-y-4">
      {report.ok ? (
        <p className="text-sm text-muted-foreground">
          体检通过：勾选的字段在表里都还在，数据表与视图也都在。
        </p>
      ) : (
        <p className="text-sm">
          体检发现需要处理的地方（改名不算问题——那能自愈，所以不在下面）。
        </p>
      )}

      {report.missingFields.length > 0 ? (
        <div className="space-y-2">
          <h3 className="text-xs font-medium text-muted-foreground">
            表里已经没有这些列了（勾了但拉不到）
          </h3>
          <ul className="space-y-2">
            {report.missingFields.map((missing) => (
              <li
                key={`${missing.fieldId}/${missing.sourceKey}`}
                data-slot="missing-field"
                className="space-y-1 rounded-md border border-border px-3 py-2"
              >
                <div className="flex flex-wrap items-baseline gap-2 text-sm">
                  <span className="font-medium">
                    {missing.fieldName ?? "（未知字段名）"}
                  </span>
                  <span className="text-xs text-muted-foreground">
                    已被删除：到「维护字段」停用它，或改绑到另一列。
                  </span>
                </div>
                <div className="flex flex-wrap items-center gap-2">
                  <code className="rounded-sm bg-muted/60 px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
                    {missing.fieldId}
                  </code>
                  <code className="rounded-sm bg-muted/60 px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
                    {missing.sourceKey}
                  </code>
                </div>
                {missing.affectedChildren.length > 0 ? (
                  <p className="text-xs text-tone-warning">
                    会连带暂停：
                    {missing.affectedChildren
                      .map((child) => child.fieldName ?? child.fieldId)
                      .join("、")}
                  </p>
                ) : null}
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {report.duplicateNameFields.length > 0 ? (
        <div className="space-y-2">
          <h3 className="text-xs font-medium text-muted-foreground">
            字段名重复（按名字取数会取到不确定的列）
          </h3>
          <ul className="space-y-1">
            {report.duplicateNameFields.map((field) => (
              <li
                key={`${field.fieldId}/${field.sourceKey}`}
                data-slot="duplicate-name-field"
                className="flex flex-wrap items-baseline gap-2 text-sm"
              >
                <span className="font-medium">{field.fieldName}</span>
                <span className="text-xs text-muted-foreground">
                  表内不止一列叫这个名字，请到「维护字段」改绑到唯一的那一列。
                </span>
                <code className="rounded-sm bg-muted/60 px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
                  {field.fieldId}
                </code>
                <code className="rounded-sm bg-muted/60 px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
                  {field.sourceKey}
                </code>
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {report.unsupportedTypeFields.length > 0 ? (
        <div className="space-y-2">
          <h3 className="text-xs font-medium text-muted-foreground">
            字段类型不支持作为选项值
          </h3>
          <ul className="space-y-1">
            {report.unsupportedTypeFields.map((field) => (
              <li
                key={`${field.fieldId}/${field.sourceKey}`}
                data-slot="unsupported-type-field"
                className="flex flex-wrap items-baseline gap-2 text-sm"
              >
                <span className="font-medium">{field.fieldName}</span>
                <span className="text-xs text-muted-foreground">
                  当前类型 {typeLabel(field.fieldType)}
                  {field.uiType === null ? "" : `（${field.uiType}）`}
                  ，这一列取不到可枚举的选项；请改绑到单选/文本一类的列。
                </span>
                <code className="rounded-sm bg-muted/60 px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
                  {field.fieldId}
                </code>
                <code className="rounded-sm bg-muted/60 px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
                  {field.sourceKey}
                </code>
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {report.unboundFields.length > 0 ? (
        <div className="space-y-2">
          <h3 className="text-xs font-medium text-muted-foreground">
            远端新增、尚未绑定
          </h3>
          <p className="text-xs text-muted-foreground">
            可在「维护字段」中添加；这不是故障。
          </p>
          <ul className="space-y-1">
            {report.unboundFields.map((field) => (
              <li
                key={field.fieldId}
                data-slot="unbound-field"
                className="flex flex-wrap items-baseline gap-2 text-sm"
              >
                <span className="font-medium">
                  {field.fieldName || field.fieldId}
                </span>
                <span className="text-xs text-muted-foreground">
                  {typeLabel(field.fieldType)}
                  {field.uiType === null ? "" : `（${field.uiType}）`}
                </span>
                <code className="rounded-sm bg-muted/60 px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
                  {field.fieldId}
                </code>
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {problems.map((problem) => (
        <p key={problem} className="text-sm text-muted-foreground">
          {problem}
        </p>
      ))}

      {report.unchecked.length > 0 ? (
        <div className="space-y-1">
          <h3 className="text-xs font-medium text-muted-foreground">
            这次没查成（不算通过）
          </h3>
          <ul className="space-y-1">
            {report.unchecked.map((line) => (
              <li key={line} className="text-xs text-muted-foreground">
                {line}
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {onRecheck ? (
        <Button variant="outline" size="sm" onClick={onRecheck}>
          重新体检
        </Button>
      ) : null}
    </div>
  );
}
