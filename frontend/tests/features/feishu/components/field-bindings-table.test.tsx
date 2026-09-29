import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { FieldBindingsTable } from "@/features/feishu/components/FieldBindingsTable";
import type { DatasourceFieldBinding } from "@/features/feishu/types";

/**
 * 字段绑定表：一条数据源 = 一张表，这张表就是它的 N 个字段，同时也是切换器。
 *
 * 它替掉的是一句谎话——原先列表页有「加密返回 / 默认语言」两列，读的却是**表级行**
 * 上早已不存在的键，于是对每一条数据源都恒画「—」与一个空语言徽标。那两个属性
 * 属于**绑定层**，只有在这里逐字段显示才是真的。
 */

function binding(
  overrides: Partial<DatasourceFieldBinding> = {},
): DatasourceFieldBinding {
  return {
    fieldId: "fldA",
    fieldName: "币种/Currency",
    sourceKey: "currency",
    parentFieldId: null,
    enabled: true,
    encryptEnabled: false,
    defaultLocale: "zh_cn",
    tokenRotatedAt: null,
    ...overrides,
  };
}

function rows(container: HTMLElement): HTMLElement[] {
  return Array.from(
    container.querySelectorAll<HTMLElement>('[data-slot="binding-row"]'),
  );
}

const CHAIN = [
  binding({
    fieldId: "fldCode",
    fieldName: "银行流水摘要-编码",
    sourceKey: "summary_code",
    parentFieldId: "fldType",
  }),
  binding({
    fieldId: "fldType",
    fieldName: "费用类型/Fee Type*",
    sourceKey: "fee_type",
    parentFieldId: "fldCat",
  }),
  binding({
    fieldId: "fldCat",
    fieldName: "费用大类/Main Exp Cat*",
    sourceKey: "main_exp_cat",
  }),
];

describe("FieldBindingsTable", () => {
  it("一条绑定一行，行序是父→子（连带深度），缩进由 padding 表达", () => {
    const { container } = render(
      <FieldBindingsTable
        bindings={CHAIN}
        selectedSourceKey={null}
        onSelect={() => {}}
      />,
    );

    // 父在最前（深度 0），孙最后（深度 2）
    expect(rows(container)).toHaveLength(3);
    expect(rows(container)[0]).toHaveTextContent("费用大类/Main Exp Cat*");
    expect(rows(container)[0]?.dataset.depth).toBe("0");
    expect(rows(container)[1]).toHaveTextContent("费用类型/Fee Type*");
    expect(rows(container)[1]?.dataset.depth).toBe("1");
    expect(rows(container)[2]).toHaveTextContent("银行流水摘要-编码");
    expect(rows(container)[2]?.dataset.depth).toBe("2");
  });

  it("父子关系用缩进表达：子行缩进到父行之下，不再单列「父字段」", () => {
    // 曾经是一张六列表格，父子关系靠「父字段」那一列点名；现在是树形导航，
    // 层级由 `data-depth` + 行内缩进表达——父在前、子紧随其后，缩进随深度递增。
    const { container } = render(
      <FieldBindingsTable
        bindings={CHAIN}
        selectedSourceKey={null}
        onSelect={() => {}}
      />,
    );

    const rowEls = rows(document.body);
    expect(rowEls.map((row) => row.dataset.depth)).toEqual(["0", "1", "2"]);
    // 子行的缩进比父行深（`paddingInlineStart` 由 `data-depth` 推导）
    const depth0 = rowEls.find((row) => row.dataset.depth === "0");
    const depth1 = rowEls.find((row) => row.dataset.depth === "1");
    const depth2 = rowEls.find((row) => row.dataset.depth === "2");
    expect(Number(depth0?.dataset.depth)).toBeLessThan(
      Number(depth1?.dataset.depth),
    );
    expect(Number(depth1?.dataset.depth)).toBeLessThan(
      Number(depth2?.dataset.depth),
    );
    // `fldCat` 只应出现在 title 属性里，不铺成可见文本
    const visible = container.textContent ?? "";
    expect(visible).not.toContain("fldCat");
  });

  it("点一行把那个字段的 source_key 交给上层（它同时就是切换器）", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(
      <FieldBindingsTable
        bindings={CHAIN}
        selectedSourceKey={null}
        onSelect={onSelect}
      />,
    );

    await user.click(
      screen.getByRole("button", { name: "费用类型/Fee Type*" }),
    );
    expect(onSelect).toHaveBeenCalledWith("fee_type");
  });

  it("当前正在看的那一行被标出来（aria-pressed），别的行没有", () => {
    render(
      <FieldBindingsTable
        bindings={CHAIN}
        selectedSourceKey="fee_type"
        onSelect={() => {}}
      />,
    );

    expect(
      screen.getByRole("button", { name: "费用类型/Fee Type*", pressed: true }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "费用大类/Main Exp Cat*" }),
    ).toHaveAttribute("aria-pressed", "false");
  });

  it("加密返回与默认语言是**逐字段**的，不再是一句对整条源说的话", () => {
    render(
      <FieldBindingsTable
        bindings={[
          binding({ encryptEnabled: true, defaultLocale: "en_us" }),
          binding({ fieldId: "fldB", sourceKey: "fx", defaultLocale: "ja_jp" }),
        ]}
        selectedSourceKey={null}
        onSelect={() => {}}
      />,
    );

    // 只有开了的那一条才有「加密返回」标记（表头也有这三个字，所以按行取）
    const bodyRows = rows(document.body);
    expect(bodyRows[0]).toHaveTextContent("加密返回");
    expect(bodyRows[1]).not.toHaveTextContent("加密返回");
    // 两条各自的语言都如实显示，而不是一个空徽标
    expect(screen.getByText("English")).toBeInTheDocument();
    expect(screen.getByText("日本語")).toBeInTheDocument();
  });

  it("停用的绑定仍然列出来（它还会被整表拉取跳过，必须看得见）", () => {
    render(
      <FieldBindingsTable
        bindings={[
          binding({ fieldId: "fldOff", sourceKey: "old_rate", enabled: false }),
        ]}
        selectedSourceKey={null}
        onSelect={() => {}}
      />,
    );

    expect(
      screen.getByRole("button", { name: "币种/Currency" }),
    ).toBeInTheDocument();
    expect(screen.getByText("已停用")).toBeInTheDocument();
  });

  it("一条绑定都没有时整块不渲染（空表会被读成「这张表没有列」）", () => {
    const { container } = render(
      <FieldBindingsTable
        bindings={[]}
        selectedSourceKey={null}
        onSelect={() => {}}
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });
});
