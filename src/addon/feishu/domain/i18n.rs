//! 飞书外部选项的 `@i18n@` 占位符与 `i18nResources` 构造。
//!
//! 飞书侧用 `options[].value` 里的键到 `i18nResources[].texts` 中匹配当前语言下的文案，
//! 因此**同一个键必须在所有语言里都出现**。文档明示 `i18nResources` 必须返回且非空
//! （「返回空会导致显示是空的」），所以任何结果集都至少带一条默认语言。
//!

use std::collections::BTreeMap;

use super::protocol::{FeishuI18nResource, FeishuOption, FeishuResultBody};

/// 未指定语言时的默认语言环境。
pub(crate) const DEFAULT_LOCALE: &str = "zh_cn";

/// 由选项 id 生成 `@i18n@` 键。
///
/// `option_id` 全局唯一且固定，因此它天然是合适的文案键。
pub(crate) fn i18n_key(option_id: &str) -> String {
    format!("{I18N_PREFIX}{option_id}")
}

/// `value` 的固定前缀。飞书侧用 `options[].value` 到 `i18nResources[].texts` 里匹配文案，
/// 因此这个前缀是**我们与飞书之间的约定**，剥/拼两侧必须用同一个常量。
pub(crate) const I18N_PREFIX: &str = "@i18n@";

/// 把飞书回传的联动值归一成可用的父值。
///
/// **真机回传的是文案，不是 `option_id`**（2026-09-24 云上抓包实测
/// `{"手动填写内容":"成都"}`，见 `docs/architecture/feishu-datasource-table-config.md`
/// §11.2）——这里既没有前缀也没有哈希，函数原样返回。`@i18n@<option_id>` 那条路径
/// 是契约形态（父控件本身就是「关联外部选项」时才成立），保留但**不是主流**。
///
/// 因此**不能假定一定有前缀**：**有前缀才剥、没前缀原样用、两端一律 trim**。
///
/// 归一化只负责去壳，**不负责判定「这个值能不能用」**：能用的形态有两种（`option_id`
/// 或文案），解析成父键是 `approval_options::resolve_parent_key` 的事。调用方必须对
/// 「归一化后仍匹配不上」做可归因的失败，而不是让它表现为「这个父值没有子项」——
/// 后者是**静默的 0 行**。
pub(crate) fn normalize_linkage_value(raw: &str) -> &str {
    let trimmed = raw.trim();
    let stripped = trimmed.strip_prefix(I18N_PREFIX).unwrap_or(trimmed);
    // 剥完再 trim 一次：`"@i18n@ x "` 这种形态在实测里出现过（占位符与值之间带空格）。
    stripped.trim()
}

/// 构造响应所需的一行选项数据。
#[derive(Debug, Clone)]
pub(crate) struct OptionRow {
    /// 选项唯一标识（全局唯一且固定）。
    pub(crate) option_id: String,
    /// 默认语言下的文案。
    pub(crate) label: String,
    /// 额外语言下的文案，键为语言环境。
    pub(crate) i18n: BTreeMap<String, String>,
    /// 稳定排序键；不被本模块消费，供分页游标编码使用。
    pub(crate) sort_order: i64,
    /// 是否为默认选项。
    pub(crate) is_default: bool,
}

/// 组装 `data.result` 的明文内容。
///
/// 语言集合是「默认语言 ∪ 各行出现过的额外语言」，且**默认语言恒存在**并标记
/// `isDefault`——因此本函数不可能产出空的 `i18nResources`。某语言缺某条文案时退回
/// 默认语言，保证每个键在所有语言里都存在（飞书按 `options[].value` 的键去
/// `i18nResources.texts` 里查，缺键会显示为空）。
pub(crate) fn build_result_body(
    rows: &[OptionRow],
    default_locale: &str,
    has_more: bool,
    next_page_token: Option<String>,
) -> FeishuResultBody {
    let options = rows
        .iter()
        .map(|row| FeishuOption {
            id: row.option_id.clone(),
            value: i18n_key(&row.option_id),
            is_default: row.is_default.then_some(true),
        })
        .collect();

    // 语言顺序固定：默认语言在前，其余按字典序——同一份数据每次产出相同字节
    let mut locales: Vec<String> = vec![default_locale.to_string()];
    let mut extra: Vec<&String> = rows
        .iter()
        .flat_map(|row| row.i18n.keys())
        .filter(|locale| locale.as_str() != default_locale)
        .collect();
    extra.sort();
    extra.dedup();
    locales.extend(extra.into_iter().cloned());

    let i18n_resources = locales
        .into_iter()
        .map(|locale| {
            let is_default = locale == default_locale;
            let texts = rows
                .iter()
                .map(|row| {
                    let text = if is_default {
                        row.label.clone()
                    } else {
                        row.i18n
                            .get(&locale)
                            .cloned()
                            .unwrap_or_else(|| row.label.clone())
                    };
                    (i18n_key(&row.option_id), text)
                })
                .collect::<BTreeMap<_, _>>();
            FeishuI18nResource {
                locale,
                is_default,
                texts,
            }
        })
        .collect();

    FeishuResultBody {
        options,
        i18n_resources,
        has_more,
        next_page_token,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        id: &str,
        label: &str,
        extra: &[(&str, &str)],
        sort_order: i64,
        is_default: bool,
    ) -> OptionRow {
        OptionRow {
            option_id: id.to_string(),
            label: label.to_string(),
            i18n: extra
                .iter()
                .map(|(locale, text)| ((*locale).to_string(), (*text).to_string()))
                .collect(),
            sort_order,
            is_default,
        }
    }

    #[test]
    fn value_is_i18n_placeholder_keyed_by_option_id() {
        let body = build_result_body(
            &[row("dept_sales", "销售部", &[], 0, false)],
            "zh_cn",
            false,
            None,
        );
        assert_eq!(body.options[0].id, "dept_sales");
        assert_eq!(body.options[0].value, "@i18n@dept_sales");
    }

    #[test]
    fn default_locale_always_present_even_without_extra_translations() {
        // 这是飞书唯一明示必传的字段：返回空会导致控件显示为空
        let body = build_result_body(&[row("a", "甲", &[], 0, false)], "zh_cn", false, None);
        assert!(!body.i18n_resources.is_empty(), "i18nResources 不得为空");
        let zh = body
            .i18n_resources
            .iter()
            .find(|resource| resource.locale == "zh_cn")
            .unwrap_or_else(|| panic!("必须含默认语言: {:?}", body.i18n_resources));
        assert!(zh.is_default, "默认语言必须标记 isDefault");
        assert_eq!(zh.texts.get("@i18n@a").map(String::as_str), Some("甲"));
    }

    #[test]
    fn empty_result_still_returns_one_locale() {
        // 即使一个选项都没有，也必须回一种语言，否则控件显示为空
        let body = build_result_body(&[], "zh_cn", false, None);
        assert_eq!(body.i18n_resources.len(), 1);
        assert!(body.i18n_resources[0].is_default);
        assert!(body.i18n_resources[0].texts.is_empty());
    }

    #[test]
    fn extra_locales_share_the_same_keys() {
        let body = build_result_body(
            &[row(
                "a",
                "甲",
                &[("en_us", "Alpha"), ("ja_jp", "アルファ")],
                0,
                true,
            )],
            "zh_cn",
            false,
            None,
        );
        assert_eq!(body.i18n_resources.len(), 3, "默认语言 + 两个额外语言");
        for resource in &body.i18n_resources {
            assert!(
                resource.texts.contains_key("@i18n@a"),
                "所有语言必须使用同一组键: {resource:?}"
            );
        }
        let en = body
            .i18n_resources
            .iter()
            .find(|resource| resource.locale == "en_us")
            .unwrap_or_else(|| panic!("应有 en_us: {:?}", body.i18n_resources));
        assert_eq!(en.texts.get("@i18n@a").map(String::as_str), Some("Alpha"));
        assert!(!en.is_default, "非默认语言不得标记 isDefault");
    }

    #[test]
    fn missing_translation_falls_back_to_default_label() {
        // 某语言缺这条文案时必须退回默认语言，否则该键在那种语言下会缺席
        let body = build_result_body(
            &[
                row("a", "甲", &[("en_us", "Alpha")], 0, false),
                row("b", "乙", &[], 1, false),
            ],
            "zh_cn",
            false,
            None,
        );
        let en = body
            .i18n_resources
            .iter()
            .find(|resource| resource.locale == "en_us")
            .unwrap_or_else(|| panic!("应有 en_us: {:?}", body.i18n_resources));
        assert_eq!(en.texts.get("@i18n@b").map(String::as_str), Some("乙"));
    }

    #[test]
    fn is_default_flag_is_propagated() {
        let body = build_result_body(&[row("a", "甲", &[], 0, true)], "zh_cn", false, None);
        assert_eq!(body.options[0].is_default, Some(true));

        let body = build_result_body(&[row("a", "甲", &[], 0, false)], "zh_cn", false, None);
        assert_eq!(body.options[0].is_default, None, "非默认选项不输出该键");
    }

    #[test]
    fn next_page_token_only_when_has_more() {
        let body = build_result_body(&[], "zh_cn", true, Some("cursor".to_string()));
        assert!(body.has_more);
        assert_eq!(body.next_page_token.as_deref(), Some("cursor"));
    }

    #[test]
    fn locale_order_is_stable() {
        // 同一份数据必须每次产出相同字节，否则响应无法比对/缓存
        let rows = [
            row(
                "a",
                "甲",
                &[("ja_jp", "アルファ"), ("en_us", "Alpha")],
                0,
                false,
            ),
            row("b", "乙", &[("en_us", "Beta")], 1, false),
        ];
        let first = build_result_body(&rows, "zh_cn", false, None);
        let second = build_result_body(&rows, "zh_cn", false, None);
        let locales: Vec<&str> = first
            .i18n_resources
            .iter()
            .map(|resource| resource.locale.as_str())
            .collect();
        assert_eq!(
            locales,
            vec!["zh_cn", "en_us", "ja_jp"],
            "默认语言在前，其余字典序"
        );
        assert_eq!(
            serde_json::to_string(&first).ok(),
            serde_json::to_string(&second).ok(),
            "同一份数据两次构造必须字节一致"
        );
    }

    #[test]
    fn linkage_value_strips_the_i18n_prefix() {
        assert_eq!(
            normalize_linkage_value("@i18n@currency:abc123"),
            "currency:abc123"
        );
    }

    #[test]
    fn linkage_value_without_a_prefix_is_used_as_is() {
        // 不能假定一定有前缀：裸 id 与裸文案都可能回传
        assert_eq!(
            normalize_linkage_value("currency:abc123"),
            "currency:abc123"
        );
        assert_eq!(normalize_linkage_value("USD 美元"), "USD 美元");
    }

    #[test]
    fn linkage_value_is_trimmed_on_both_ends() {
        // 占位符与值之间带空格这种形态实测里出现过
        assert_eq!(normalize_linkage_value("  @i18n@ x  "), "x");
        assert_eq!(normalize_linkage_value("  裸值  "), "裸值");
    }

    #[test]
    fn normalization_is_empty_for_blank_input() {
        // 空值由调用方判为「用户尚未选父级」并回退全量，而不是拼一个空的父键
        assert_eq!(normalize_linkage_value(""), "");
        assert_eq!(normalize_linkage_value("   "), "");
        assert_eq!(normalize_linkage_value("@i18n@"), "");
        assert_eq!(normalize_linkage_value("@i18n@   "), "");
    }

    #[test]
    fn only_a_leading_prefix_is_stripped() {
        // 值本身含 `@i18n@` 时不能被误剥（文案里出现该字面量是可能的）
        assert_eq!(normalize_linkage_value("a@i18n@b"), "a@i18n@b");
    }
}
