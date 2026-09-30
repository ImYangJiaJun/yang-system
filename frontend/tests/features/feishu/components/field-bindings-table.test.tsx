/**
 * 字段绑定表：一条数据源 = 一张表，这张表就是它的 N 个字段，同时也是切换器。
 *
 * C3/C4 设计：简化为树形导航，每行只留状态点 + 字段名 + sourceKey，
 * 父子关系用 `ml-5` 缩进表达，不再显示加密/语言/状态徽标。
 */

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { FieldBindingsTable } from "@/features/feishu/components/FieldBindingsTable";
import type { DatasourceFieldBinding } from "@/features/feishu/types";

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
  it("一条绑定一行，行序是父→子（连带深度），子行用 ml-5 缩进", () => {
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

    // 子行有 ml-5 类名
    expect(rows(container)[1]).toHaveClass("ml-5");
    expect(rows(container)[2]).toHaveClass("ml-5");
    expect(rows(container)[0]).not.toHaveClass("ml-5");
  });

  it("点一行把那个字段的 source_key 交给上层（它同时就是切换器）", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    const { container } = render(
      <FieldBindingsTable
        bindings={CHAIN}
        selectedSourceKey={null}
        onSelect={onSelect}
      />,
    );

    await user.click(rows(container)[1]); // 点击第二行（费用类型）
    expect(onSelect).toHaveBeenCalledWith("fee_type");
  });

  it("当前正在看的那一行被标出来（bg-blue-50 + border-l-[3px]），别的行没有", () => {
    render(
      <FieldBindingsTable
        bindings={CHAIN}
        selectedSourceKey="fee_type"
        onSelect={() => {}}
      />,
    );

    const selectedRow = rows(document.body).find(
      (row) => row.dataset.selected === "true",
    );
    expect(selectedRow).toBeInTheDocument();
    expect(selectedRow).toHaveTextContent("费用类型/Fee Type*");
    expect(selectedRow).toHaveClass("bg-blue-50");
    expect(selectedRow).toHaveClass("border-l-[3px]");

    // 其他行没有选中样式
    const otherRows = rows(document.body).filter(
      (row) => row.dataset.selected !== "true",
    );
    otherRows.forEach((row) => {
      expect(row).not.toHaveClass("bg-blue-50");
    });
  });

  it("停用的绑定用灰色状态点表达（不再显示「已停用」文字）", () => {
    render(
      <FieldBindingsTable
        bindings={[
          binding({ fieldId: "fldOff", sourceKey: "old_rate", enabled: false }),
        ]}
        selectedSourceKey={null}
        onSelect={() => {}}
      />,
    );

    const row = rows(document.body)[0];
    expect(row).toBeInTheDocument();
    // 状态点是灰色（bg-gray-400）而不是绿色（bg-green-500）
    const statusDot = row?.querySelector(".bg-gray-400");
    expect(statusDot).toBeInTheDocument();
  });

  it("启用的绑定用绿色状态点表达", () => {
    render(
      <FieldBindingsTable
        bindings={[binding({ enabled: true })]}
        selectedSourceKey={null}
        onSelect={() => {}}
      />,
    );

    const row = rows(document.body)[0];
    const statusDot = row?.querySelector(".bg-green-500");
    expect(statusDot).toBeInTheDocument();
  });

  it("一条绑定都没有时整块不渲染", () => {
    const { container } = render(
      <FieldBindingsTable
        bindings={[]}
        selectedSourceKey={null}
        onSelect={() => {}}
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("底部显示图例：绿色点 = 启用中，黄色点 = 有问题", () => {
    render(
      <FieldBindingsTable
        bindings={[binding()]}
        selectedSourceKey={null}
        onSelect={() => {}}
      />,
    );

    expect(screen.getByText("启用中")).toBeInTheDocument();
    expect(screen.getByText("有问题")).toBeInTheDocument();
  });
});
