import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  applyDensity,
  DENSITY_STORAGE_KEY,
  loadDensity,
  persistDensity,
} from "@/shell/density";

beforeEach(() => localStorage.clear());

describe("密度主题", () => {
  it("默认 default，非法存储值回退 default", () => {
    expect(loadDensity()).toBe("default");
    localStorage.setItem(DENSITY_STORAGE_KEY, "huge");
    expect(loadDensity()).toBe("default");
  });

  it("持久化并可恢复", () => {
    persistDensity("compact");
    expect(loadDensity()).toBe("compact");
    persistDensity("loose");
    expect(loadDensity()).toBe("loose");
  });

  it("应用到文档根：default 不写属性", () => {
    const target = { dataset: {} as Record<string, string> };
    applyDensity("compact", target as HTMLElement);
    expect(target.dataset.density).toBe("compact");
    applyDensity("loose", target as HTMLElement);
    expect(target.dataset.density).toBe("loose");
    applyDensity("default", target as HTMLElement);
    expect(target.dataset.density).toBeUndefined();
  });

  it("Storage 读写抛错时降级为默认值/内存态，不阻断启动", () => {
    const getItem = vi
      .spyOn(Storage.prototype, "getItem")
      .mockImplementation(() => {
        throw new Error("storage denied");
      });
    expect(loadDensity()).toBe("default");
    getItem.mockRestore();

    const setItem = vi
      .spyOn(Storage.prototype, "setItem")
      .mockImplementation(() => {
        throw new Error("storage denied");
      });
    expect(() => persistDensity("compact")).not.toThrow();
    setItem.mockRestore();
  });

  it("localStorage getter 本身抛错时仍返回 default", () => {
    const original = Object.getOwnPropertyDescriptor(
      globalThis,
      "localStorage",
    );
    Object.defineProperty(globalThis, "localStorage", {
      configurable: true,
      get() {
        throw new Error("storage denied");
      },
    });
    try {
      expect(loadDensity()).toBe("default");
    } finally {
      if (original) {
        Object.defineProperty(globalThis, "localStorage", original);
      } else {
        Reflect.deleteProperty(globalThis, "localStorage");
      }
    }
  });
});
