//! 级联（父级）声明的**唯一事实源**：形状、通配键与解析。
//!
//! # 形状
//!
//! `feishu_datasource.linkage_mapping` 是一段 JSON 文本：
//!
//! ```json
//! { "<联动控件代码>": { "parent_source_key": "payment_currency",
//!                      "parent_field": "币种/Currency（单选）" } }
//! ```
//!
//! # 为什么收敛到一个模块
//!
//! 读端（`approval_options`）与写端（`pull`）原先**各定义了自己那半份**形状：
//! 读端只认 `parent_source_key`，写端认三个成员。后果是 `cascade_field` 变成了
//! 一个「表单要求必填、系统零消费」的死字段——两边各看一半，谁都没发现没人用它。
//! 现在形状只有这一处定义，加成员就得同时面对两个消费者。
//!
//! # 通配键 `"*"`
//!
//! 联动的键是**飞书表单里那个控件的字段代码**（形如 `widget17796881173030001`）。
//! 要求用户去表单设计器里翻出它，是在索取一个**我们自己从未观测过真实报文**的值
//! （设计文档的 V4 至今未决）——填错了不会报错，只会静默退化成「无级联」。
//!
//! 而设计本身就假定了**一个控件 = 一个数据源 = 一个 `source_key`**（C3 硬要求的
//! 「不带 linkage_params 要回退全量」正是为此）。既然一个数据源只会服务一个联动控件，
//! 那么声明了级联时出现的任何联动参数**只可能是它**。于是允许用 `"*"` 作通配键：
//!
//! - 映射里有精确键 → 按精确键匹配（兼容已经手工配好的存量数据）；
//! - 映射里只有 `"*"` → 匹配任意联动参数键；
//! - 两者都有 → 精确键优先。
//!
//! 通配不会放大歧义：命中多个参数时读端仍然 fail-closed（`LINKAGE_AMBIGUOUS`），
//! 与精确键时的行为一致。

use serde::Deserialize;

/// 通配键：留空控件代码时前端写这个。
pub(crate) const WILDCARD_KEY: &str = "*";

/// 一条级联声明。
///
/// **只有两个成员**。曾经有第三个 `cascade_field`（本表里承载子值的列），但它
/// 零消费——拉取用的是数据源自己的取数列（`PullSource::field_name`），与它无关。
/// 留着它只会制造「它可以和取数列不一样」的错觉，而真不一样时没有任何行为差异。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Linkage {
    /// 父数据源的 `source_key`；父键列里存的是父的 `option_id`，没有它拼不出来。
    pub(crate) parent_source_key: String,
    /// **本表**里承载父文案的列名（父键由同行共现读出）。
    pub(crate) parent_field: String,
}

/// 反序列化用的原始形状。
#[derive(Debug, Deserialize)]
struct LinkageEntry {
    #[serde(default)]
    parent_source_key: String,
    #[serde(default)]
    parent_field: String,
    /// 已废弃：解析时**显式忽略**而不是拒绝，否则存量配置会突然失效。
    #[serde(default, rename = "cascade_field")]
    _cascade_field: Option<String>,
}

/// 解析结果：`(控件代码, 声明)`。控件代码可能是 [`WILDCARD_KEY`]。
///
/// 成员缺失的条目会被**跳过**而不是让整段解析失败：一条配错的级联只该让那个数据源
/// 退化成「无级联」，不该打挂整个 addon。
pub(crate) fn parse_linkage_mapping(raw: &str) -> Vec<(String, Linkage)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let map: std::collections::BTreeMap<String, LinkageEntry> = match serde_json::from_str(trimmed)
    {
        Ok(map) => map,
        Err(error) => {
            tracing::warn!(error = %error, "linkage_mapping 不是合法 JSON，按无级联处理");
            return Vec::new();
        }
    };
    map.into_iter()
        .filter_map(|(key, entry)| {
            let parent_source_key = entry.parent_source_key.trim();
            let parent_field = entry.parent_field.trim();
            if parent_source_key.is_empty() || parent_field.is_empty() {
                // 缺一个就拼不出父键：`parent_source_key` 缺了不知道父在哪，
                // `parent_field` 缺了读不出同行共现。
                tracing::warn!(
                    widget = %key,
                    "linkage_mapping 的条目缺 parent_source_key 或 parent_field，跳过该条"
                );
                return None;
            }
            Some((
                key,
                Linkage {
                    parent_source_key: parent_source_key.to_string(),
                    parent_field: parent_field.to_string(),
                },
            ))
        })
        .collect()
}

/// 按联动参数里出现的键挑出唯一命中的那条声明。
///
/// 返回**命中的那个参数键**与声明：调用方要用键去 `linkage_params` 里取值，
/// 而通配命中时那个键不是任何一个映射键——只返回声明的话调用方还得自己再推一遍。
///
/// 规则见模块文档：精确键优先于通配；命中 0 条返回 `None`（调用方回退全量），
/// 命中 ≥2 条返回 `Err`（无法判定父级，fail-closed）。
pub(crate) fn match_linkage<'a>(
    entries: &'a [(String, Linkage)],
    param_keys: impl Iterator<Item = &'a str>,
) -> Result<Option<(&'a str, &'a Linkage)>, ()> {
    let keys: Vec<&str> = param_keys.collect();
    if keys.is_empty() {
        // 没有联动参数就无从谈起——回退全量是调用方的事，这里给「没命中」。
        return Ok(None);
    }

    // 精确命中：**数的是命中几个参数，不是命中几个条目**。一个数据源服务一个联动
    // 控件，请求里出现两个联动参数时无法判定哪个携带父值，此时必须 fail-closed；
    // 若只看条目数，单条通配就会把「两个参数」误判成「唯一命中」。
    let exact: Vec<&str> = keys
        .iter()
        .copied()
        .filter(|key| entries.iter().any(|(entry_key, _)| entry_key == key))
        .collect();
    if !exact.is_empty() {
        if exact.len() > 1 {
            return Err(());
        }
        let entry = entries
            .iter()
            .find(|(entry_key, _)| entry_key == exact[0])
            .map(|(_, linkage)| (exact[0], linkage));
        return Ok(entry);
    }

    // 没有精确命中时看通配：通配对**每一个**联动参数都成立，所以同样按参数个数判。
    let Some((_, wildcard)) = entries.iter().find(|(key, _)| key == WILDCARD_KEY) else {
        return Ok(None);
    };
    if keys.len() > 1 {
        return Err(());
    }
    Ok(Some((keys[0], wildcard)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAPPING: &str = r#"{"widget1":{"parent_source_key":"payment_currency",
        "parent_field":"币种/Currency（单选）","cascade_field":"汇率/Exchange Rate"}}"#;

    #[test]
    fn parses_the_declared_pair() {
        let entries = parse_linkage_mapping(MAPPING);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "widget1");
        assert_eq!(entries[0].1.parent_source_key, "payment_currency");
        assert_eq!(entries[0].1.parent_field, "币种/Currency（单选）");
    }

    #[test]
    fn legacy_cascade_field_is_ignored_not_rejected() {
        // 存量配置里带着已废弃的成员：必须照常解析，否则它们会突然失效。
        // 这条同时钉住「去掉它」不等于「拒绝它」。
        assert_eq!(parse_linkage_mapping(MAPPING).len(), 1);
    }

    #[test]
    fn blank_or_broken_input_yields_no_entries() {
        for raw in ["", "   ", "{not json", "[]", r#""text""#] {
            assert!(
                parse_linkage_mapping(raw).is_empty(),
                "{raw:?} 应解析出空集合"
            );
        }
    }

    #[test]
    fn incomplete_entries_are_skipped_without_killing_the_rest() {
        let raw = r#"{"bad":{"parent_source_key":"c"},
                      "blank":{"parent_source_key":"  ","parent_field":"  "},
                      "good":{"parent_source_key":"c","parent_field":"p"}}"#;
        let entries = parse_linkage_mapping(raw);
        assert_eq!(entries.len(), 1, "只有完整的那条应留下");
        assert_eq!(entries[0].0, "good");
    }

    #[test]
    fn exact_key_wins() {
        let entries = parse_linkage_mapping(
            r#"{"*":{"parent_source_key":"wild","parent_field":"w"},
                "widget1":{"parent_source_key":"exact","parent_field":"e"}}"#,
        );
        let (key, linkage) = match_linkage(&entries, ["widget1"].into_iter())
            .unwrap_or_else(|_| panic!("不该判为歧义"))
            .unwrap_or_else(|| panic!("应命中"));
        assert_eq!(key, "widget1");
        assert_eq!(linkage.parent_source_key, "exact");
    }

    #[test]
    fn wildcard_matches_any_key() {
        let entries =
            parse_linkage_mapping(r#"{"*":{"parent_source_key":"wild","parent_field":"w"}}"#);
        for key in ["widget17796881173030001", "anything", "a-b_c"] {
            let (matched_key, linkage) = match_linkage(&entries, [key].into_iter())
                .unwrap_or_else(|_| panic!("不该判为歧义"))
                .unwrap_or_else(|| panic!("通配键应命中 {key}"));
            // 通配命中时返回的是**请求里的那个键**，不是 "*"
            assert_eq!(matched_key, key);
            assert_eq!(linkage.parent_source_key, "wild");
        }
    }

    #[test]
    fn no_match_returns_none_so_callers_fall_back_to_all() {
        let entries = parse_linkage_mapping(MAPPING);
        assert!(match_linkage(&entries, ["other"].into_iter())
            .unwrap_or_else(|_| panic!("不该判为歧义"))
            .is_none());
        // 空映射同理
        assert!(match_linkage(&[], ["anything"].into_iter())
            .unwrap_or_else(|_| panic!("不该判为歧义"))
            .is_none());
    }

    #[test]
    fn multiple_matches_fail_closed() {
        // 通配也不会掩盖歧义：两个参数都命中通配时仍要判失败
        let entries =
            parse_linkage_mapping(r#"{"*":{"parent_source_key":"wild","parent_field":"w"}}"#);
        assert!(
            match_linkage(&entries, ["a", "b"].into_iter()).is_err(),
            "命中多个联动参数必须 fail-closed"
        );

        let two_exact = parse_linkage_mapping(
            r#"{"w1":{"parent_source_key":"a","parent_field":"p"},
                "w2":{"parent_source_key":"b","parent_field":"p"}}"#,
        );
        assert!(match_linkage(&two_exact, ["w1", "w2"].into_iter()).is_err());
    }

    #[test]
    fn a_wildcard_does_not_mask_an_unmatched_exact_key() {
        // 精确键存在但请求里没有它 → 走通配，而不是判「已命中精确」后落空
        let entries = parse_linkage_mapping(
            r#"{"*":{"parent_source_key":"wild","parent_field":"w"},
                "widget1":{"parent_source_key":"exact","parent_field":"e"}}"#,
        );
        let (_, linkage) = match_linkage(&entries, ["widget2"].into_iter())
            .unwrap_or_else(|_| panic!("不该判为歧义"))
            .unwrap_or_else(|| panic!("应回退到通配"));
        assert_eq!(linkage.parent_source_key, "wild");
    }

    #[test]
    fn a_lone_wildcard_with_no_params_yields_none() {
        let entries =
            parse_linkage_mapping(r#"{"*":{"parent_source_key":"wild","parent_field":"w"}}"#);
        assert!(match_linkage(&entries, [].into_iter())
            .unwrap_or_else(|_| panic!("不该判为歧义"))
            .is_none());
    }
}
