//! 审批定义控件目录：批量审批个人视图用它校验字段引用，并给前端选择器下发控件树。

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::addon::feishu::domain::approval_match::FormWidget;
use crate::addon::feishu::domain::widget_rules;

#[derive(Debug, Clone)]
pub(crate) struct WidgetInfo {
    pub(crate) name: String,
    pub(crate) widget_type: String,
    /// 明细/控件组下的可配置子控件 id；普通控件为空。
    pub(crate) children: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct WidgetCatalog {
    entries: Vec<Value>,
    index: HashMap<String, WidgetInfo>,
}

impl WidgetCatalog {
    /// 目录 JSON：顶层控件按定义顺序排列，明细的 `children` 带子控件。
    pub(crate) fn to_json(&self) -> Value {
        Value::Array(self.entries.clone())
    }

    pub(crate) fn contains(&self, widget_id: &str) -> bool {
        self.index.contains_key(widget_id)
    }

    pub(crate) fn is_detail(&self, widget_id: &str) -> bool {
        self.index
            .get(widget_id)
            .is_some_and(|info| info.widget_type == "fieldList")
    }

    pub(crate) fn children(&self, widget_id: &str) -> &[String] {
        self.index
            .get(widget_id)
            .map(|info| info.children.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn widget_type(&self, widget_id: &str) -> Option<&str> {
        self.index
            .get(widget_id)
            .map(|info| info.widget_type.as_str())
    }

    pub(crate) fn name(&self, widget_id: &str) -> Option<&str> {
        self.index.get(widget_id).map(|info| info.name.as_str())
    }
}

/// 由审批定义表单构建目录。
pub(crate) fn build(form: &[FormWidget]) -> WidgetCatalog {
    let mut catalog = WidgetCatalog::default();
    catalog.entries = walk(form, None, "", &mut catalog.index);
    catalog
}

#[cfg(test)]
pub(crate) fn for_test(items: &[(&str, &str, &[&str])]) -> WidgetCatalog {
    let mut index = HashMap::new();
    for (id, widget_type, children) in items {
        index.insert(
            id.to_string(),
            WidgetInfo {
                name: id.to_string(),
                widget_type: widget_type.to_string(),
                children: children.iter().map(ToString::to_string).collect(),
            },
        );
    }
    WidgetCatalog {
        entries: Vec::new(),
        index,
    }
}

fn walk(
    form: &[FormWidget],
    parent_id: Option<&str>,
    parent_qualified: &str,
    index: &mut HashMap<String, WidgetInfo>,
) -> Vec<Value> {
    let mut entries = Vec::new();
    for widget in form {
        // 官方不支持提单的控件不展示，但它的子控件仍可能可配置（如控件组）。
        if !widget_rules::is_configurable(&widget.r#type) {
            entries.extend(walk(&widget.children, parent_id, parent_qualified, index));
            continue;
        }
        let name = display_name(widget);
        let qualified = if parent_qualified.is_empty() {
            name.clone()
        } else {
            format!("{parent_qualified}_{name}")
        };
        let child_entries = walk(&widget.children, Some(&widget.id), &qualified, index);
        let child_ids: Vec<String> = child_entries
            .iter()
            .filter_map(|entry| entry.get("id").and_then(Value::as_str))
            .map(ToString::to_string)
            .collect();
        index.insert(
            widget.id.clone(),
            WidgetInfo {
                name: name.clone(),
                widget_type: widget.r#type.clone(),
                children: child_ids.clone(),
            },
        );
        entries.push(json!({
            "id": widget.id,
            "name": name,
            "qualified_name": qualified,
            "type": widget.r#type,
            "required": widget.required,
            "parent_id": parent_id,
            "children": child_ids,
        }));
        entries.extend(child_entries);
    }
    entries
}

fn display_name(widget: &FormWidget) -> String {
    if !widget.name.trim().is_empty() {
        return widget.name.clone();
    }
    if widget.r#type == "fieldList" {
        return "明细".to_string();
    }
    widget.r#type.clone()
}

#[cfg(test)]
mod tests {
    use super::build;
    use crate::addon::feishu::domain::approval_match::FormWidget;
    use serde_json::json;

    fn widget(id: &str, name: &str, widget_type: &str) -> FormWidget {
        FormWidget {
            id: id.to_string(),
            name: name.to_string(),
            r#type: widget_type.to_string(),
            required: false,
            value: json!(null),
            option: None,
            external_data: None,
            children: Vec::new(),
        }
    }

    #[test]
    fn detail_parent_exposes_children_and_is_marked() {
        let mut detail = widget("items", "费用明细", "fieldList");
        detail.children = vec![
            widget("item_name", "物品", "input"),
            widget("amount", "金额", "amount"),
        ];
        let catalog = build(&[detail]);
        assert!(catalog.is_detail("items"));
        assert_eq!(catalog.children("items"), ["item_name", "amount"]);
        assert_eq!(catalog.widget_type("amount"), Some("amount"));
        assert!(!catalog.is_detail("amount"));
        let payload = catalog.to_json();
        assert_eq!(payload[0]["children"][1], "amount");
    }

    #[test]
    fn empty_detail_name_falls_back_to_chinese_label() {
        let mut detail = widget("items", "", "fieldList");
        detail.children = vec![widget("amount", "金额", "amount")];
        let catalog = build(&[detail]);
        assert_eq!(catalog.to_json()[0]["name"], "明细");
        assert_eq!(catalog.to_json()[1]["qualified_name"], "明细_金额");
    }
}
