import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  DEFAULT_ORDER_BY,
  VIEW_STORAGE_KEY,
  clearFilters,
  initialQueryState,
  loadView,
  nextStateOnPage,
  nextStateOnPageSize,
  nextStateOnSearch,
  nextStateOnSort,
  nextStateOnStatus,
  nextStateOnView,
  persistView,
  useListQuery,
  withStableOrder,
} from "@/features/feishu/list-query";

/// 列表查询状态归约：默认值、页码归 1、视图持久化的存储异常兜底。

function stateAtPage3() {
  return {
    ...initialQueryState("ledger"),
    page: 3,
    search: "北京",
    orderBy: [{ field: "id", direction: "Asc" as const }],
  };
}

afterEach(() => {
  vi.restoreAllMocks();
  window.localStorage.clear();
});

describe("默认值", () => {
  it("默认视图是台账（按预期量级选的，不是按观感）", () => {
    expect(initialQueryState().view).toBe("ledger");
  });

  it("默认排序按名称升序（表级行上没有可排的 `source_key`）", () => {
    expect(initialQueryState().orderBy).toEqual(DEFAULT_ORDER_BY);
    expect(DEFAULT_ORDER_BY).toEqual([{ field: "title", direction: "Asc" }]);
  });

  it("默认第 1 页、每页 10、无搜索、状态筛选为全部", () => {
    const state = initialQueryState("cards");
    expect(state).toMatchObject({
      page: 1,
      pageSize: 10,
      search: "",
      status: "all",
    });
  });
});

describe("结果集变化 → 回第 1 页", () => {
  it("搜索词变化回到第 1 页", () => {
    const next = nextStateOnSearch(stateAtPage3(), "上海");
    expect(next.search).toBe("上海");
    expect(next.page).toBe(1);
  });

  it("状态筛选变化回到第 1 页", () => {
    const next = nextStateOnStatus(stateAtPage3(), "disabled");
    expect(next.status).toBe("disabled");
    expect(next.page).toBe(1);
  });

  it("每页条数变化回到第 1 页", () => {
    const next = nextStateOnPageSize(stateAtPage3(), 50);
    expect(next.pageSize).toBe(50);
    expect(next.page).toBe(1);
  });

  it("清除筛选同时清掉搜索与状态，并回到第 1 页", () => {
    const next = clearFilters(stateAtPage3());
    expect(next).toMatchObject({ search: "", status: "all", page: 1 });
  });
});

describe("不改动结果集的操作", () => {
  it("切视图不动页码、搜索与排序（两个视图共用同一份状态）", () => {
    const before = stateAtPage3();
    const next = nextStateOnView(before, "cards");
    expect(next.view).toBe("cards");
    expect(next.page).toBe(before.page);
    expect(next.search).toBe(before.search);
    expect(next.orderBy).toEqual(before.orderBy);
  });

  it("翻页只动页码，且不会低于 1", () => {
    expect(nextStateOnPage(stateAtPage3(), 2).page).toBe(2);
    expect(nextStateOnPage(stateAtPage3(), 0).page).toBe(1);
  });

  it("排序不重置页码（排序不改变结果集大小）", () => {
    const next = nextStateOnSort(stateAtPage3(), "title");
    expect(next.page).toBe(3);
    expect(next.orderBy).toEqual([{ field: "title", direction: "Asc" }]);
  });

  it("同一列再点一次反转方向", () => {
    // 夹具当前排的是 `id`，所以点「名称」是第一次选中它 → 升序。
    const ascending = nextStateOnSort(stateAtPage3(), "title");
    expect(ascending.orderBy).toEqual([{ field: "title", direction: "Asc" }]);
    const descending = nextStateOnSort(ascending, "title");
    expect(descending.orderBy).toEqual([{ field: "title", direction: "Desc" }]);
  });
});

describe("withStableOrder", () => {
  it("空数组兜底为默认排序，并补上唯一键收尾", () => {
    expect(withStableOrder([])).toEqual([
      { field: "title", direction: "Asc" },
      { field: "id", direction: "Asc" },
    ]);
  });

  it("非唯一列排序时补上唯一键收尾（否则翻页会重复/漏行）", () => {
    expect(withStableOrder([{ field: "title", direction: "Desc" }])).toEqual([
      { field: "title", direction: "Desc" },
      { field: "id", direction: "Asc" },
    ]);
  });

  it("收尾键是表级行的真唯一键 `id`——发 `source_key` 会把整个列表请求搭进去", () => {
    // `feishu_datasource` 的表级行上**没有** `source_key`（它属于字段绑定那一层）。
    // 后端 `validate_order_field` 对不存在的字段直接 `FieldNotFound`，
    // 所以这不是「排序不生效」，是**列表根本打不开**。
    for (const clauses of withStableOrder([
      { field: "title", direction: "Asc" },
    ])) {
      expect(clauses.field).not.toBe("source_key");
    }
    expect(
      withStableOrder([{ field: "title", direction: "Asc" }]).at(-1),
    ).toEqual({ field: "id", direction: "Asc" });
  });

  it("已经有 id 时不重复追加", () => {
    expect(withStableOrder([{ field: "id", direction: "Desc" }])).toEqual([
      { field: "id", direction: "Desc" },
    ]);
  });
});

describe("视图持久化", () => {
  it("写入后读回", () => {
    persistView("cards");
    expect(window.localStorage.getItem(VIEW_STORAGE_KEY)).toBe("cards");
    expect(loadView()).toBe("cards");
  });

  it("非法存值回落台账", () => {
    window.localStorage.setItem(VIEW_STORAGE_KEY, "gallery");
    expect(loadView()).toBe("ledger");
  });

  it("读取抛错时不崩，回落台账", () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("storage disabled");
    });
    expect(loadView()).toBe("ledger");
  });

  it("写入抛错时不崩", () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("storage disabled");
    });
    expect(() => persistView("cards")).not.toThrow();
  });
});

describe("useListQuery", () => {
  it("初始视图是台账，且查询快照带非空排序", () => {
    const { result } = renderHook(() => useListQuery());
    expect(result.current.state.view).toBe("ledger");
    expect(result.current.query.orderBy.length).toBeGreaterThan(0);
  });

  it("搜索立即更新输入态并回到第 1 页，查询快照等去抖后才跟上", async () => {
    const { result } = renderHook(() => useListQuery());
    act(() => {
      result.current.setPage(3);
    });
    expect(result.current.state.page).toBe(3);

    act(() => {
      result.current.setSearch("北京");
    });
    expect(result.current.state.search).toBe("北京");
    expect(result.current.state.page).toBe(1);

    await waitFor(() => {
      expect(result.current.query.search).toBe("北京");
    });
  });

  it("切视图会写进 localStorage", () => {
    const { result } = renderHook(() => useListQuery());
    act(() => {
      result.current.setView("cards");
    });
    expect(result.current.state.view).toBe("cards");
    expect(window.localStorage.getItem(VIEW_STORAGE_KEY)).toBe("cards");
  });
});
