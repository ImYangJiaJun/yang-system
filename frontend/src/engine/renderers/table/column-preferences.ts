import type { TableViewSchema } from "@/engine/contracts/ui-catalog";

/**
 * 列显示偏好（旧 useColumnPreferences 语义 + localStorage 持久化）：
 * key 按 view_id 隔离；至少保留一列可见；存储损坏/越界时回退全量列。
 */

const KEY_PREFIX = "yang.column-prefs.v1";

function storageKey(viewId: string): string {
  return `${KEY_PREFIX}.${viewId}`;
}

export function allColumnFields(view: TableViewSchema): string[] {
  return view.columns.map((column) => column.field);
}

export function loadVisibleColumns(
  viewId: string,
  allFields: string[],
): string[] {
  try {
    const raw = localStorage.getItem(storageKey(viewId));
    if (!raw) return allFields;
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return allFields;
    // 只接受当前视图仍存在的字段，保持声明顺序。
    const visible = allFields.filter((field) => parsed.includes(field));
    return visible.length > 0 ? visible : allFields;
  } catch {
    return allFields;
  }
}

export function saveVisibleColumns(viewId: string, visible: string[]): void {
  try {
    localStorage.setItem(storageKey(viewId), JSON.stringify(visible));
  } catch {
    // 隐私模式等存储不可用时保持内存态，不阻断交互。
  }
}

/// 旧语义：隐藏时至少保留一列；返回新的可见字段数组。
export function setColumnVisible(
  visible: string[],
  field: string,
  flag: boolean,
): string[] {
  if (flag && !visible.includes(field)) return [...visible, field];
  if (!flag && visible.length > 1) {
    return visible.filter((name) => name !== field);
  }
  return visible;
}
