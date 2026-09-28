/**
 * xlsx 文件导入向导：填名称 + 选文件 → 勾列 → 配源标识与父列 → 创建并导入。
 *
 * 这一份用例钉的是「做错了只有提交后才看得出来」的几件事：
 * - 列名即身份：勾选项渲染成可勾的列名，默认一个都不勾；
 * - `source_key` 不能直接用中文列名（后端要求 ASCII `[a-z0-9_]`、首字节小写字母），
 *   所以默认值必须是**派生的 ASCII 键**，且派生的那个值直接可用；
 * - 父列下拉只列**同源内已勾选**的其它列（A11：`load_parent_source_key` 按
 *   `datasource_id` 过滤），既不能自指，也不能把没勾的列塞进去；
 * - 提交是**两个请求且顺序固定**（先建源拿到 `datasource_id`，再拿它导入），
 *   导入用的是**第 1 步那批同一批 File**（服务端零暂存，用户只选一次文件）。
 *
 * 数据访问走注入的 `client`（真实实现在 `api.ts` 的 `useXlsxImportClient()`），
 * 因此这里不依赖会话与界面目录——那两样由 `api.test.ts` 单独钉住。
 */

import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { XlsxImportWizard } from "@/features/feishu/components/XlsxImportWizard";
import type {
  CreatedTable,
  CreateTableSubmission,
  XlsxImportClient,
} from "@/features/feishu/api";

function stubClient(
  overrides: Partial<XlsxImportClient> = {},
): XlsxImportClient {
  return {
    probe: vi.fn().mockResolvedValue({
      sheetName: "境内银行网点信息管理",
      sheets: ["境内银行网点信息管理"],
      headerRow: 1,
      columns: [
        { name: "开户行行名", index: 2 },
        { name: "联行号", index: 5 },
        { name: "地区名称", index: 7 },
      ],
      files: [{ name: "bank_1.xlsx" }],
    }),
    createTable: vi
      .fn()
      .mockResolvedValue({ datasourceId: 9, credentials: [] }),
    importFiles: vi.fn().mockResolvedValue({
      datasourceId: 9,
      elapsedMs: 1200,
      files: [{ name: "bank_1.xlsx", rowsRead: 5 }],
      bindings: [],
    }),
    ...overrides,
  };
}

function file(name = "bank_1.xlsx"): File {
  return new File(["PK\x03\x04"], name, {
    type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  });
}

/// 走到「提交」那一步：填名称 + 选文件 → 解析表头 → 勾两列 → 配源标识与父列。
///
/// 返回第 1 步选进去的那个 `File` **实例**——调用方据此断言「导入收到的是同一个实例」，
/// 而不是「一个同名文件」（否则有人改成「提交时从 DOM 重读」测试照样绿）。
async function driveToStep4(
  client: XlsxImportClient,
  options: {
    uploaded?: File;
    onSubmitted?: (
      created: CreatedTable,
      submission: CreateTableSubmission,
    ) => void;
  } = {},
): Promise<File> {
  const uploaded = options.uploaded ?? file();
  render(
    <XlsxImportWizard
      client={client}
      onCancel={vi.fn()}
      onSubmitted={options.onSubmitted ?? vi.fn()}
    />,
  );
  // 第 1 步：填名称 + 选文件
  await userEvent.type(screen.getByLabelText("名称"), "银行网点");
  await userEvent.upload(screen.getByLabelText("xlsx 文件"), uploaded);
  await userEvent.click(screen.getByRole("button", { name: "解析表头" }));
  // 等 probe 回来
  await screen.findByText("开户行行名");
  await userEvent.click(screen.getByRole("button", { name: "下一步" }));
  // 第 2 步：勾两列
  await userEvent.click(screen.getByLabelText("开户行行名"));
  await userEvent.click(screen.getByLabelText("联行号"));
  await userEvent.click(screen.getByRole("button", { name: "下一步" }));
  // 第 3 步：配源标识与父列（默认 source_key 已自动派生）
  await userEvent.click(screen.getByRole("button", { name: "下一步" }));
  // 第 4 步
  await screen.findByText(/第 4 步/);
  return uploaded;
}

describe("xlsx 导入向导 · 探表头与勾列", () => {
  it("第 1 步传文件后进第 2 步，并把列名渲染成可勾选项", async () => {
    const client = stubClient();
    render(<XlsxImportWizard client={client} onCancel={vi.fn()} />);
    await userEvent.type(screen.getByLabelText("名称"), "银行网点");
    await userEvent.upload(screen.getByLabelText("xlsx 文件"), file());
    await userEvent.click(screen.getByRole("button", { name: "解析表头" }));

    // 三个列名都渲染出来，且**未勾选**（默认一个都不勾）
    for (const name of ["开户行行名", "联行号", "地区名称"]) {
      const box = await screen.findByLabelText(name);
      expect(box).not.toBeChecked();
    }
    // 传进去的确实是 File 实例（引擎靠 instanceof File 决定走不走 FormData）
    expect(vi.mocked(client.probe).mock.calls[0]?.[0]?.[0]).toBeInstanceOf(
      File,
    );
  });

  it("一个列都不勾时不能进第 3 步", async () => {
    // 「勾零列」建出来的数据源一条绑定都没有——它不会出数，
    // 但会在台账里占一行，用户得先勾一列。
    const client = stubClient();
    render(<XlsxImportWizard client={client} onCancel={vi.fn()} />);
    await userEvent.type(screen.getByLabelText("名称"), "银行网点");
    await userEvent.upload(screen.getByLabelText("xlsx 文件"), file());
    await userEvent.click(screen.getByRole("button", { name: "解析表头" }));
    await screen.findByText("开户行行名");
    await userEvent.click(screen.getByRole("button", { name: "下一步" }));

    // 已经到第 2 步（勾列），但一个都没勾
    expect(screen.getByText(/第 2 步/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "下一步" })).toBeDisabled();
  });
});

describe("xlsx 导入向导 · 源标识与父列", () => {
  it("每列的源标识默认派生成合法 ASCII 键，且中文列名不会直接进 sourceKey", async () => {
    // 后端约束：1..=64 字节、首字节小写 ASCII 字母、其余 [a-z0-9_]。
    // 中文列名直接当 source_key 会被 400 拒掉，所以默认值必须是派生的 ASCII 键，
    // 前端先挡一道，别让用户提交后才吃错误。
    const client = stubClient();
    await driveToStep4(client);
    const sourceKeyInput = screen.getByLabelText(
      "开户行行名 的源标识",
    ) as HTMLInputElement;
    expect(sourceKeyInput.value).toMatch(/^[a-z][a-z0-9_]{0,63}$/);
    expect(sourceKeyInput.value).not.toContain("开户行行名");
  });

  it("父列下拉只列同源内已勾选的其它列", async () => {
    // A11：父子关系在同源内（后端 load_parent_source_key 按 datasource_id 过滤）。
    // 列一个跨源的父选项只会让用户在提交后才被拒。
    const client = stubClient();
    await driveToStep4(client);

    await userEvent.click(screen.getByLabelText("联行号 的父列"));
    const options = await screen.findAllByRole("option");
    const labels = options.map((option) => option.textContent);
    expect(labels).toContain("开户行行名");
    expect(labels).not.toContain("地区名称"); // 没勾的列不能当父
    expect(labels).not.toContain("联行号"); // 不能自指
  });

  it("取消勾选某列时，「以它为父」的那一行会被清成无父列", async () => {
    // 父必须在同源内勾选集合里：留着一条指向已取消列的父指针，只会在提交时被后端拒。
    const client = stubClient();
    await driveToStep4(client);

    await userEvent.click(screen.getByLabelText("联行号 的父列"));
    await userEvent.click(
      await screen.findByRole("option", { name: "开户行行名" }),
    );

    // 回第 2 步把「开户行行名」取消勾选，再回到第 4 步提交
    await userEvent.click(screen.getByRole("button", { name: "上一步" }));
    await userEvent.click(screen.getByRole("button", { name: "上一步" }));
    await userEvent.click(screen.getByLabelText("开户行行名"));
    await userEvent.click(screen.getByRole("button", { name: "下一步" }));
    await userEvent.click(screen.getByRole("button", { name: "下一步" }));
    await userEvent.click(screen.getByRole("button", { name: "创建并导入" }));

    await waitFor(() => expect(client.createTable).toHaveBeenCalled());
    const submission = vi.mocked(client.createTable).mock.calls[0]?.[0];
    expect(submission?.fields.map((field) => field.fieldId)).toEqual([
      "联行号",
    ]);
    expect(submission?.fields[0]?.parentFieldId).toBeNull();
  });

  it("提交时先建源再导入，且导入用的是同一批 File", async () => {
    // D11：服务端零暂存——第 1 步的文件只用于探表头，导入时要重传同一批
    // （浏览器里 File 对象一直在，用户不用重新选）。
    const client = stubClient();
    const uploaded = await driveToStep4(client);

    // 勾了「联行号」且父列选了「开户行行名」，源标识用默认值
    await userEvent.click(screen.getByLabelText("联行号 的父列"));
    await userEvent.click(
      await screen.findByRole("option", { name: "开户行行名" }),
    );
    await userEvent.click(screen.getByRole("button", { name: "创建并导入" }));

    await waitFor(() => expect(client.importFiles).toHaveBeenCalledTimes(1));

    // 顺序固定：先建源（拿到 datasourceId），再拿它去导入
    const createdId = vi.mocked(client.createTable).mock.results[0]?.value;
    void createdId; // createTable 是 mock，断言调用顺序即可
    expect(
      vi.mocked(client.createTable).mock.invocationCallOrder[0],
    ).toBeLessThan(
      vi.mocked(client.importFiles).mock.invocationCallOrder[0] ?? 0,
    );

    // **同一个 File 实例被重传**——这是「用户只选一次文件」的实现方式。
    // 用 `toBe` 而不是比文件名：若有人改成「提交时从 DOM 重新读一遍」，这里会红。
    const [datasourceId, passed] =
      vi.mocked(client.importFiles).mock.calls[0] ?? [];
    expect(datasourceId).toBe(9); // stubClient 里 createTable 回的 datasourceId
    expect(passed?.[0]).toBe(uploaded);

    // 建源请求里 field_name 必须带上（漏了会让审批选项装配整批失败）
    const submission = vi.mocked(client.createTable).mock.calls[0]?.[0];
    expect(submission?.ingestMode).toBe("xlsx_import");
    for (const field of submission?.fields ?? []) {
      expect(field.fieldName).not.toBe("");
      expect(field.fieldId).toBe(field.fieldName);
    }
  });
});

describe("xlsx 导入向导 · 探表头里必须说出来的两件事", () => {
  /// 探表头 + 选文件 + 用给定的 probe 回执进第 2 步。
  async function probeWith(client: XlsxImportClient) {
    render(<XlsxImportWizard client={client} onCancel={vi.fn()} />);
    await userEvent.type(screen.getByLabelText("名称"), "银行网点");
    await userEvent.upload(screen.getByLabelText("xlsx 文件"), file());
    await userEvent.click(screen.getByRole("button", { name: "解析表头" }));
  }

  it("表头不在第 1 行时必须说出来（第 1 行整行为空会静默下沉）", async () => {
    // `read_header` 的规则是「表头 = 第一个非全空行」：第 1 行整行为空时会**静默**
    // 下沉到第 2 行。这个设计被判为可接受，前提正是用户能看见它发生过——否则
    // 「表头怎么变成数据第一行」只能事后翻文件对。
    const client = stubClient({
      probe: vi.fn().mockResolvedValue({
        sheetName: "Sheet1",
        sheets: ["Sheet1", "Sheet2"],
        headerRow: 2,
        columns: [{ name: "开户行行名", index: 1 }],
        files: [{ name: "bank_1.xlsx" }],
      }),
    });
    await probeWith(client);

    expect(await screen.findByText(/表头在第 2 行/)).toBeInTheDocument();
    // 多张 sheet 时只读第一张，这件事同样必须说出来
    expect(screen.getByText(/只读了第一张/)).toBeInTheDocument();
  });

  it("表头就在第 1 行且只有一张 sheet 时不提这两件事", async () => {
    // 无端提示等于噪音：每一次导入都弹一句「表头在第 1 行」会让人不再读它。
    const client = stubClient();
    await probeWith(client);

    await screen.findByText("开户行行名");
    expect(screen.queryByText(/表头在第/)).toBeNull();
    expect(screen.queryByText(/只读了第一张/)).toBeNull();
  });
});

describe("xlsx 导入向导 · 建源成功而导入失败", () => {
  it("不回滚数据源，失败时留在第 4 步，重试只重发导入那一步", async () => {
    // 建源是配置、导入是数据，两者各自可重试——这正是拆成两个请求的理由。
    // 回滚再建会撞 title 重名（`create_datasource_table` 没有幂等键）。
    const importFiles = vi
      .fn()
      .mockRejectedValueOnce(new Error("文件表头不一致"))
      .mockResolvedValue({
        datasourceId: 9,
        elapsedMs: 10,
        files: [],
        bindings: [],
      });
    const client = stubClient({ importFiles });
    await driveToStep4(client);

    await userEvent.click(screen.getByRole("button", { name: "创建并导入" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      /数据源已建好（#9），但导入失败/,
    );
    // 仍在第 4 步，且**只**建了一次数据源
    expect(screen.getByText(/第 4 步/)).toBeInTheDocument();
    expect(client.createTable).toHaveBeenCalledTimes(1);

    await userEvent.click(screen.getByRole("button", { name: "重试导入" }));
    await waitFor(() => expect(importFiles).toHaveBeenCalledTimes(2));
    expect(client.createTable).toHaveBeenCalledTimes(1);
  });

  it("配置冻结：回不去、表只读，但文件可换；交回调用方的就是真发出去的那份", async () => {
    // 建源成功之后，界面上看得见的配置**已经写进库了**。允许继续编辑却发不出去，
    // 是一条用户从屏幕上无法自查的坏路径：他改完点了重试，跑的还是第一份绑定，
    // 而调用方还会收到一份从未发出去的提交物。所以：配置冻结（并说明为什么），
    // 只有文件能换（原文件本身可能就是坏的）。
    const importFiles = vi
      .fn()
      .mockRejectedValueOnce(new Error("文件表头不一致"))
      .mockResolvedValue({
        datasourceId: 9,
        elapsedMs: 10,
        files: [],
        bindings: [],
      });
    const client = stubClient({ importFiles });
    const onSubmitted = vi.fn();
    await driveToStep4(client, { onSubmitted });

    // 先配一个父列，再提交——这份配置就是「落定」下来的那一份
    await userEvent.click(screen.getByLabelText("联行号 的父列"));
    await userEvent.click(
      await screen.findByRole("option", { name: "开户行行名" }),
    );
    await userEvent.click(screen.getByRole("button", { name: "创建并导入" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      /数据源已建好（#9），但导入失败/,
    );
    // 为什么冻结必须说出来：不说的话，用户会以为自己还能改
    expect(screen.getByText(/按第一次提交落定/)).toBeInTheDocument();

    // 编辑入口确已不可用：回不到第 1/2/3 步，表上的输入与下拉也只读
    expect(screen.getByRole("button", { name: "上一步" })).toBeDisabled();
    expect(screen.getByLabelText("联行号 的源标识")).toHaveAttribute(
      "readonly",
    );
    expect(screen.getByLabelText("联行号 的父列")).toBeDisabled();

    // 文件仍可换：换一份重试导入
    const retryFile = file("bank_2.xlsx");
    await userEvent.upload(
      screen.getByLabelText("重试导入用的 xlsx 文件"),
      retryFile,
    );
    await userEvent.click(screen.getByRole("button", { name: "重试导入" }));

    await waitFor(() => expect(importFiles).toHaveBeenCalledTimes(2));
    // 只重发导入（不再建源），且用的是换过的那份新文件
    expect(client.createTable).toHaveBeenCalledTimes(1);
    expect(importFiles.mock.calls[1]?.[1]?.[0]).toBe(retryFile);
    // 交回调用方的是**真发出去的那一份**（同一个对象），不是界面重算的
    expect(onSubmitted.mock.calls[0]?.[0]).toEqual({
      datasourceId: 9,
      credentials: [],
    });
    expect(onSubmitted.mock.calls[0]?.[1]).toBe(
      vi.mocked(client.createTable).mock.calls[0]?.[0],
    );
  });
});
