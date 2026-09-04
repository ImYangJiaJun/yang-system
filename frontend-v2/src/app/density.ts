/**
 * 密度主题（ADR-5 §2.1 三档）：compact ≤36px / default 44px / loose ≥52px 行高，
 * CSS 变量 --density-cell-y 驱动（见 index.css），localStorage 持久化。
 */

export type Density = "compact" | "default" | "loose";

export const DENSITY_STORAGE_KEY = "yang.density.v1";

export const DENSITY_OPTIONS: ReadonlyArray<{
  value: Density;
  label: string;
}> = [
  { value: "compact", label: "紧凑" },
  { value: "default", label: "默认" },
  { value: "loose", label: "宽松" },
];

export function loadDensity(): Density {
  if (typeof localStorage === "undefined") return "default";
  const raw = localStorage.getItem(DENSITY_STORAGE_KEY);
  return raw === "compact" || raw === "loose" ? raw : "default";
}

export function persistDensity(density: Density): void {
  try {
    localStorage.setItem(DENSITY_STORAGE_KEY, density);
  } catch {
    // 存储不可用时保持内存态。
  }
}

/// 应用到文档根；default 不写属性（CSS 默认值）。
export function applyDensity(
  density: Density,
  target: Pick<HTMLElement, "dataset"> = document.documentElement,
): void {
  if (density === "default") {
    delete target.dataset.density;
    return;
  }
  target.dataset.density = density;
}
