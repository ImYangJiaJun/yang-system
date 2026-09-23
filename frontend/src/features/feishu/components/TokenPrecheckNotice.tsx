/**
 * 创建 / 轮换之后的 Token 连通性预检回执。
 *
 * 这是控制台唯一能「具体」的指引时刻：此刻它知道真实的 `source_key`，也知道用户
 * 手里正握着明文 Token——而服务端永远回不出明文（`token_hash` 只存 SHA-256 摘要），
 * 所以**事后无法再校验任何一次**。预检因此必须排在创建/轮换成功之后（数据源行得先存在，
 * `approval_options` 才能按 `source_key` 查库并比对哈希）。
 *
 * 预检失败**不阻塞**创建结果：数据源已经建好了，这一屏只回答「链路通没通」，
 * 并且必须说清「建好了、但 Token 没验证过」，而不是让人以为创建失败了。
 */

import { Button } from "@/shared/ui/button";

import { APPROVAL_OPTION_CODES, type TokenPrecheckResult } from "../types";
import { CopyField } from "./CopyField";

export type TokenPrecheckMode = "create" | "rotate";

export type TokenPrecheckNoticeProps = {
  /// 真实的数据源标识：回执里要粘回飞书审批后台的那一段就是它。
  sourceKey: string;
  /// null 表示还没有结果（不渲染任何东西）。
  result: TokenPrecheckResult | null;
  mode?: TokenPrecheckMode;
  pending?: boolean;
  /// 预检失败时的「就地重填 Token」入口：不必删掉数据源重建。
  onRotate?: () => void;
};

const NEUTRAL_BAR =
  "rounded-md border border-border bg-muted/50 px-3 py-2 text-sm";
const ERROR_BAR =
  "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive";

export function TokenPrecheckNotice({
  sourceKey,
  result,
  mode = "create",
  pending = false,
  onRotate,
}: TokenPrecheckNoticeProps) {
  if (pending) {
    return (
      <p className={NEUTRAL_BAR} aria-live="polite">
        正在用刚填的 Token 试拉一次选项…
      </p>
    );
  }

  if (!result) return null;

  if (result.status === "failed") {
    // 只有「数据源查不到」这一种失败能推翻「数据源已经就位」——服务端的判定顺序是
    // 先按 source_key 查数据源（40401），再比对 Token（40101/40102），最后看状态（40301）。
    // 其余失败码都说明那一行是存在的，可以照旧说上面那次操作已经生效。
    const sourceMissing = result.code === APPROVAL_OPTION_CODES.sourceNotFound;

    return (
      <div className="space-y-3">
        <p className={ERROR_BAR} role="alert">
          {mode === "create" ? "数据源已创建" : "Token 已更新"}
          ，但这次预检没走通：{result.message}
          {result.code === null ? "" : `（错误码 ${result.code}）`}
        </p>
        <p className={NEUTRAL_BAR} aria-live="polite">
          {result.verdict}
        </p>
        <p className="text-sm text-muted-foreground">{result.hint}</p>
        <p className="text-xs text-muted-foreground">
          {sourceMissing
            ? "服务端说它查不到这个数据源——上面那次操作到底有没有落库，这一屏确认不了，请回列表页核对它是否还在。"
            : "预检失败不影响上面这次操作的结果——数据源本身已经就位，只是这条链路还没验通。"}
        </p>
        {
          // 「重新填写 Token」只在 Token 可能是原因时才给：40401 里服务端根本没查到那一行，
          // 拿新 Token 去更新它只会再收到一次「数据源不存在」——那就是把用户送进死路。
        }
        {onRotate && !sourceMissing ? (
          <Button variant="outline" size="sm" onClick={onRotate}>
            重新填写 Token
          </Button>
        ) : null}
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <p className={NEUTRAL_BAR} aria-live="polite">
        {mode === "create" ? "数据源已创建" : "Token 已更新"}
        ，链路是通的：
        {result.encrypted
          ? "服务端返回了加密内容（该数据源开了「加密返回」），内容读不了，但这次取数已经成功。"
          : result.hasMore
            ? // 这一页拉满了（单页上限 100），服务端还说有下一页：
              // 那就只能说「还有更多」，不能说个精确的条数——那个数只属于这一页。
              `服务端这次拉到了 ${result.optionCount} 个选项，而且它说还有更多——这个接口一次只返回一页，所以这里给不出总数。`
            : `服务端这次拉到了 ${result.optionCount} 个选项。`}
      </p>
      <CopyField
        value={sourceKey}
        label="把这一段粘回飞书审批后台的外部选项配置里："
        hint="接口地址与 Token 由飞书那边填写；在这里点「校验数据」是飞书后台自己的动作。"
      />
    </div>
  );
}
