import { beforeEach, describe, expect, it } from "vitest";

import {
  applyDensity,
  DENSITY_STORAGE_KEY,
  loadDensity,
  persistDensity,
} from "./density";

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
});
