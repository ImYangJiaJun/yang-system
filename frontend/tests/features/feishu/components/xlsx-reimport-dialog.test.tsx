/**
 * xlsx 数据源的「重新导入」对话框。
 *
 * **15 万行那一档真正走的入口**：建源向导只走一次，此后每次更新数据都要经过这里。
 * 这一份钉三件事：
 * - 只有 pending 期间才轮询这条数据源的进度（平时打开着一次都不问）；
 * - pending 期间 `role="status"` 那行文案来自服务端的阶段快照；
 * - 失败仍然把服务端的原文留在对话框里，且不留下一条转圈的进度。
 */

import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { XlsxReimportDialog } from "@/features/feishu/components/XlsxReimportDialog";
import type {
  ImportProgress,
  XlsxImportClient,
  XlsxImportReport,
} from "@/features/feishu/api";

const IDLE: ImportProgress = {
  stage: "idle",
  filesTotal: null,
  filesDone: null,
  fileIndex: null,
  rowsDone: null,
  rowsTotal: null,
  bindingsTotal: null,
  bindingsDone: null,
};

const REPORT: XlsxImportReport = {
  datasourceId: 12,
  elapsedMs: 900,
  files: [{ name: "bank_1.xlsx", rowsRead: 5 }],
  bindings: [],
};

function stubClient(
  overrides: Partial<
    Pick<XlsxImportClient, "importFiles" | "importProgress">
  > = {},
): Pick<XlsxImportClient, "importFiles" | "importProgress"> {
  return {
    importFiles: vi.fn().mockResolvedValue(REPORT),
    importProgress: vi.fn().mockResolvedValue(IDLE),
    ...overrides,
  };
}

function file(name = "bank_1.xlsx"): File {
  return new File(["PK\x03\x04"], name, {
    type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  });
}

function renderDialog(
  client: Pick<XlsxImportClient, "importFiles" | "importProgress">,
  onImported = vi.fn(),
) {
  render(
    <XlsxReimportDialog
      open
      datasourceId={12}
      client={client}
      onImported={onImported}
      onCancel={vi.fn()}
    />,
  );
  return onImported;
}

async function pickFiles() {
  await userEvent.upload(screen.getByLabelText("xlsx 文件"), file());
  await userEvent.click(screen.getByRole("button", { name: "开始导入" }));
}

describe("XlsxReimportDialog · 导入进度", () => {
  it("pending 期间显示进度、并轮询**这条**数据源；导入完成后状态行消失", async () => {
    let resolveImport: ((report: XlsxImportReport) => void) | undefined;
    const client = stubClient({
      importFiles: vi.fn(
        () =>
          new Promise<XlsxImportReport>((resolve) => {
            resolveImport = resolve;
          }),
      ),
      importProgress: vi.fn().mockResolvedValue({
        ...IDLE,
        stage: "writing",
        bindingsTotal: 4,
        bindingsDone: 1,
      }),
    });
    renderDialog(client);

    // 对话框开着但还没提交：**一次都不问**（这条源平时没有导入在跑，
    // 每秒问一次只是白打服务端）。
    expect(client.importProgress).not.toHaveBeenCalled();

    await pickFiles();

    await waitFor(() => {
      expect(screen.getByRole("status")).toHaveTextContent(
        "正在写入选项：已完成 1/4 条绑定…",
      );
    });
    expect(client.importProgress).toHaveBeenCalledWith(12);

    await act(async () => {
      resolveImport?.(REPORT);
    });

    await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
  });

  it("导入成功后把整份回执交回调用方，且不再显示进度行", async () => {
    const client = stubClient();
    const onImported = renderDialog(client);

    await pickFiles();

    await waitFor(() => expect(onImported).toHaveBeenCalledWith(REPORT));
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("失败仍把服务端原文留在对话框里，且不留下一条转圈的进度", async () => {
    const client = stubClient({
      importFiles: vi.fn().mockRejectedValue(new Error("文件缺列：联行号")),
      importProgress: vi.fn().mockResolvedValue({ ...IDLE, stage: "parsing" }),
    });
    renderDialog(client);

    await pickFiles();

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "文件缺列：联行号",
    );
    // 失败之后 pending 归 false：进度行必须一起消失——留着它就是一句
    // 「还在导」的假话。
    await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
    expect(screen.getByRole("button", { name: "开始导入" })).toBeEnabled();
  });
});
