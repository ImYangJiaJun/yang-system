/**
 * 导入进度的观测侧：轮询 hook 的启停，与那句进度文案的每个分支。
 *
 * 这一份钉的是「写错了只有真跑一次 15 万行导入才看得出来」的几件事：
 * - `datasourceId === null` 时**一次都不问**（没有导入在跑的时候问它毫无意义，
 *   而且导入页面是常驻挂载的）；
 * - 挂上之后**立刻问一次**，否则第一秒里只剩「正在上传文件」；
 * - 客户端失败**一律吞掉**，不抛、也不把上一拍的好数据抹掉；
 * - 文案里 `rows_done` 是**物理**行序号、分母可能缺失、`R > T` 时要回落成纯计数。
 *
 * 数字一律 `String` / `toFixed`（禁 `toLocaleString` / `Intl`，见单语言产品合同）。
 */

import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ImportProgress } from "@/features/feishu/api";
import {
  IMPORT_PROGRESS_POLL_MS,
  describeImportProgress,
  useImportProgress,
} from "@/features/feishu/import-progress";

function snapshot(overrides: Partial<ImportProgress> = {}): ImportProgress {
  return {
    stage: "idle",
    filesTotal: null,
    filesDone: null,
    fileIndex: null,
    rowsDone: null,
    rowsTotal: null,
    bindingsTotal: null,
    bindingsDone: null,
    ...overrides,
  };
}

function file(name: string, megabytes: number): File {
  return new File([new Uint8Array(megabytes * 1024 * 1024)], name);
}

describe("describeImportProgress", () => {
  const files = [file("bank_1.xlsx", 1), file("bank_2.xlsx", 0.5)];

  it("null 与 idle 同为不确定态：只说得出本地这批文件的个数与总字节", () => {
    // 上传阶段服务端**结构上报不出来**（multipart body 在 transport 层读完才 dispatch），
    // 所以这里只能说不确定态。**绝不能**说「没有导入在跑」——蓝绿双实例下进度轮询
    // 可能落到另一色，它恒答 idle。
    const expected = "正在上传文件（共 2 个，1.5 MB）…";
    expect(describeImportProgress(null, files)).toBe(expected);
    expect(describeImportProgress(snapshot(), files)).toBe(expected);
  });

  it("loading：读表头读到了第几个文件", () => {
    expect(
      describeImportProgress(
        snapshot({ stage: "loading", filesTotal: 2, filesDone: 1 }),
        files,
      ),
    ).toBe("正在读取并校验表头（已完成 1/2 个文件）…");
  });

  it("loading 而服务端还没上报计数：分母用本地文件数，不画「0/0」", () => {
    // 条目刚建好那一拍（stage 已是 loading）两个计数都还是 null。显示「0/0 个文件」
    // 是假话——我们明明发了 2 个过去。
    expect(describeImportProgress(snapshot({ stage: "loading" }), files)).toBe(
      "正在读取并校验表头（已完成 0/2 个文件）…",
    );
  });

  it("parsing 有分母：第 i/n 个文件，已读 R / 共 T 行", () => {
    expect(
      describeImportProgress(
        snapshot({
          stage: "parsing",
          filesTotal: 1,
          filesDone: 1,
          fileIndex: 1,
          rowsDone: 154386,
          rowsTotal: 154386,
        }),
        files,
      ),
    ).toBe("正在解析数据：第 1/1 个文件，已读 154386 / 共 154386 行…");
  });

  it("parsing 无分母：文件里没有 <dimension> 时只说已读多少行，不画假分母", () => {
    expect(
      describeImportProgress(
        snapshot({
          stage: "parsing",
          filesTotal: 3,
          fileIndex: 2,
          rowsDone: 41,
          rowsTotal: null,
        }),
        files,
      ),
    ).toBe("正在解析数据：第 2/3 个文件，已读 41 行…");
  });

  it("parsing 的 R > T：分母陈旧，回落成纯计数（否则是「已读 5 / 共 3 行」）", () => {
    expect(
      describeImportProgress(
        snapshot({
          stage: "parsing",
          filesTotal: 1,
          fileIndex: 1,
          rowsDone: 5,
          rowsTotal: 3,
        }),
        files,
      ),
    ).toBe("正在解析数据：第 1/1 个文件，已读 5 行…");
  });

  it("writing：已开始处理的绑定数 / 总绑定数", () => {
    expect(
      describeImportProgress(
        snapshot({
          stage: "writing",
          filesTotal: 1,
          filesDone: 1,
          fileIndex: 1,
          rowsDone: 154386,
          rowsTotal: 154386,
          bindingsTotal: 4,
          bindingsDone: 0,
        }),
        files,
      ),
    ).toBe("正在写入选项：已完成 0/4 条绑定…");
  });

  it("认不出的阶段退回不确定态，而不是按别的阶段读出一句假话", () => {
    const unknown = {
      ...snapshot(),
      stage: "finalizing",
    } as unknown as ImportProgress;
    expect(describeImportProgress(unknown, files)).toBe(
      "正在上传文件（共 2 个，1.5 MB）…",
    );
  });
});

describe("useImportProgress", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("挂上立刻问一次，之后每秒一拍", async () => {
    vi.useFakeTimers();
    const importProgress = vi
      .fn()
      .mockResolvedValue(snapshot({ stage: "loading" }));
    // client 必须是**稳定引用**（真实实现在 useMemo 里），否则每次 setState 触发的
    // 重渲染都会换掉 effect 依赖、把 effect 重新跑一遍。
    const client = { importProgress };

    const { result } = renderHook(() => useImportProgress(client, 7));

    // 第一拍**不用等时钟**：否则第一秒里界面只剩「正在上传文件」那种不确定态。
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(importProgress).toHaveBeenCalledTimes(1);
    expect(importProgress).toHaveBeenCalledWith(7);
    expect(result.current?.stage).toBe("loading");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(IMPORT_PROGRESS_POLL_MS);
    });
    expect(importProgress).toHaveBeenCalledTimes(2);
  });

  it("datasourceId 为 null：一次都不问，返回 null", async () => {
    vi.useFakeTimers();
    const importProgress = vi.fn().mockResolvedValue(snapshot());
    const client = { importProgress };

    const { result } = renderHook(() => useImportProgress(client, null));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3 * IMPORT_PROGRESS_POLL_MS);
    });

    expect(importProgress).not.toHaveBeenCalled();
    expect(result.current).toBeNull();
  });

  it("变回 null 之后不再问（cleanup 清掉定时器与在途响应）", async () => {
    vi.useFakeTimers();
    const importProgress = vi.fn().mockResolvedValue(snapshot());
    const client = { importProgress };

    const { result, rerender } = renderHook(
      ({ id }: { id: number | null }) => useImportProgress(client, id),
      { initialProps: { id: 7 as number | null } },
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(IMPORT_PROGRESS_POLL_MS);
    });
    const asked = importProgress.mock.calls.length;
    expect(asked).toBeGreaterThan(1);

    rerender({ id: null });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5 * IMPORT_PROGRESS_POLL_MS);
    });

    expect(importProgress).toHaveBeenCalledTimes(asked);
    expect(result.current).toBeNull();
  });

  it("客户端失败一律吞掉：不抛，也不抹掉上一拍的好数据", async () => {
    vi.useFakeTimers();
    const importProgress = vi
      .fn()
      .mockResolvedValueOnce(snapshot({ stage: "parsing", rowsDone: 41 }))
      .mockRejectedValue(new Error("目录未加载"));
    const client = { importProgress };

    const { result } = renderHook(() => useImportProgress(client, 7));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(IMPORT_PROGRESS_POLL_MS);
    });
    expect(result.current?.rowsDone).toBe(41);

    // 第二拍被拒：目录未加载 / 滚动发布落到另一实例 / 网络抖动都长这样，
    // 三者都不该让导入失败，也不该把上一拍读到的进度抹成「不确定」。
    await act(async () => {
      await vi.advanceTimersByTimeAsync(IMPORT_PROGRESS_POLL_MS);
    });
    expect(importProgress).toHaveBeenCalledTimes(3);
    expect(result.current?.rowsDone).toBe(41);
  });

  it("重试复用同一个 id：新一轮首拍落地前不显示上一轮的残留快照", async () => {
    vi.useFakeTimers();
    let release: ((value: ImportProgress) => void) | undefined;
    const importProgress = vi
      .fn()
      .mockResolvedValueOnce(
        // 上一轮失败前最后读到的那一拍
        snapshot({ stage: "writing", bindingsTotal: 5, bindingsDone: 3 }),
      )
      .mockImplementation(
        () =>
          new Promise<ImportProgress>((resolve) => {
            release = resolve;
          }),
      );
    const client = { importProgress };

    const { result, rerender } = renderHook(
      ({ id }: { id: number | null }) => useImportProgress(client, id),
      { initialProps: { id: 7 as number | null } },
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current?.stage).toBe("writing");

    // 导入失败 → 组件把 id 置 null（`submitting` 落回 false）；点「重试导入」时
    // id 回到**同一个**值（`created` 非空就不再建源，见 XlsxImportWizard）。
    // 服务端此刻答的是新一轮的 loading，所以第二拍落地之前，一张上一轮的旧快照
    // 都不许显示——否则界面会拿「已完成 3/5 条绑定」去描述刚开始的那一轮。
    rerender({ id: null });
    expect(result.current).toBeNull();

    rerender({ id: 7 });
    expect(result.current).toBeNull();

    // 首拍落地之后才恢复显示。
    await act(async () => {
      release?.(snapshot({ stage: "loading", filesTotal: 2, filesDone: 0 }));
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current?.stage).toBe("loading");
  });
});
