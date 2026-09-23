//! 从多维表格记录派生飞书选项：`option_id`、文案、父键、排序与内容摘要。
//!
//! # `option_id` 的派生规则
//!
//! ```text
//! 无父：{source_key}:{hex(sha256(label))[:12]}
//! 有父：{source_key}:{hex(sha256(parent_key ‖ U+001F ‖ label))[:12]}
//! ```
//!
//! 三条都是被约束逼出来的，不是随手选的：
//!
//! - **`source_key` 前缀**：`option_id` 是**表级**唯一索引，另有跨源夺取预检。
//!   前缀让「两个数据源用同一个 label」在结构上不可能碰撞。
//! - **父值进哈希**：保证同一 `source_key` 内「同 label 不同父」不碰撞。
//! - **不引入序号**：序号会随重排漂移，破坏飞书要求的「固定」。
//!   同理**不能用行号 / 编号 / `record_id`** 当码。
//!
//! 飞书的 select 选项没有稳定 id，只能由文案派生。代价是已知并接受的：
//! 改文案 = 新 id + 旧 id 被补集停用（不删除）；**改父文案会连同其全部子选项的
//! id 一起变**——断链是子树级的。
//!
//! # 为什么不按 label 折叠
//!
//! **去重必须按 `option_id`，不能按 label。** 按 label 折叠会把「同 label 不同父」
//! 重新并成一个，父级选到其中一个时另一个**静默消失**。

use std::collections::HashSet;

use super::token::hash_token;

/// 派生规则版本。**改动派生口径（哈希输入、id 形状、文案拼法）时必须手动 bump**，
/// 并把它并入摘要输入——否则换了规则而源内容不变时，新列永远填不上。
pub(crate) const DERIVE_RULE_VERSION: u32 = 1;

/// 哈希输入里的分隔符。用 ASCII 单元分隔符（US, 0x1F）而不是 `:` 或 `|`：
/// 选项文案是外部数据，可能含任何可打印字符。与 `domain/pagination.rs` 的游标
/// 用同一个字符，理由相同。
const SEPARATOR: char = '\u{1f}';

/// `option_id` 里哈希部分的长度（hex 字符数）。
const HASH_PREFIX_LEN: usize = 12;

/// 一行待派生的原始取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RawValue<'a> {
    /// 同一行里父列的文案；无级联时为 `None`。
    ///
    /// 父键由**同行共现**读出（而不是去父表反查）——这是设计 A7b 的直接依据。
    ///
    /// 注意：「一子多父」**是存在的**，不要照抄旧结论。这里原先记的是「实测三条
    /// 父子关系全部 0 例『一子多父』，共现即正确配对」，那句「0 例」是在**银行网点
    /// xlsx** 上量的，对**目标台账不成立**：`费用类型 → 银行流水摘要-编码` 实测有
    /// **6 例**「一子多父」，例如 `pay for services-YL-AR` 同时挂在
    /// `推广测评服务费` 与 `预付储值款` 下（设计 §4.7）。
    ///
    /// 共现读法仍然正确，因为**同文案不同父会派生成两个选项**：`option_id` 把
    /// `parent_key` 哈希进了输入（见模块文档的派生规则与 `option_id_of`），
    /// 有测试钉着这一点。代价是同一段文案在飞书下拉里可能出现两次——那是级联的
    /// 固有形态，接受；**算法不变，变的只是对数据的描述**。
    pub(crate) parent_label: Option<&'a str>,
    /// 本行的取值文案。
    pub(crate) label: &'a str,
}

/// 派生出的一个选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DerivedOption {
    pub(crate) option_id: String,
    pub(crate) label: String,
    /// 父选项的 `option_id`；无父时为空串。
    pub(crate) parent_key: String,
    /// 快照行序（去重后的稳定序号）。
    pub(crate) sort_order: i64,
}

/// 计算一个选项 id 的哈希部分。
fn option_id_of(source_key: &str, parent_key: Option<&str>, label: &str) -> String {
    let digest = match parent_key.filter(|key| !key.is_empty()) {
        Some(parent_key) => hash_token(&format!("{parent_key}{SEPARATOR}{label}")),
        None => hash_token(label),
    };
    // hash_token 恒返回 64 字符小写 hex，切片安全（按字节切在 ASCII hex 上等价于按字符）。
    format!("{source_key}:{}", &digest[..HASH_PREFIX_LEN])
}

/// 由文案算父键（父数据源的 option_id）。父数据源没有父键。
pub(crate) fn parent_option_id(parent_source_key: &str, parent_label: &str) -> String {
    option_id_of(parent_source_key, None, parent_label)
}

/// 把一轮快照的取值派生成选项。
///
/// 按 `option_id` 去重并**保留首次出现的顺序**：`sort_order` 取该顺序的序号，
/// 取选项端点按 `sort_order ASC, option_id ASC` 排序并据此编码游标
/// （`approval_options.rs`）——全落 0 会让选项序退化成哈希序且跨页不稳。
///
/// 空文案（trim 后为空）一律跳过：空单元格不贡献任何值。
pub(crate) fn derive_options(
    source_key: &str,
    parent_source_key: Option<&str>,
    values: &[RawValue<'_>],
) -> Vec<DerivedOption> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut derived = Vec::new();
    for value in values {
        let label = value.label.trim();
        if label.is_empty() {
            continue;
        }
        // 父键要么来自声明的父数据源，要么为空；有父文案却缺父源配置时**不猜**，
        // 按无父处理——猜错会让整棵子树挂到错误的父上。
        let parent_key = match (value.parent_label, parent_source_key) {
            (Some(parent_label), Some(parent_source_key)) => {
                let parent_label = parent_label.trim();
                if parent_label.is_empty() {
                    String::new()
                } else {
                    parent_option_id(parent_source_key, parent_label)
                }
            }
            _ => String::new(),
        };
        let option_id = option_id_of(source_key, Some(&parent_key), label);
        if !seen.insert(option_id.clone()) {
            continue;
        }
        derived.push(DerivedOption {
            option_id,
            label: label.to_string(),
            parent_key,
            sort_order: derived.len() as i64,
        });
    }
    derived
}

/// 内容摘要：只在**真变化**时写库与追加审计。
///
/// 输入是**派生结果**而不是原始记录：原始记录里带了大量与选项无关的列（金额、
/// 附件、审批状态……），把它们算进摘要会让「台账里改了个无关字段」也触发一次全量重写。
///
/// # `enabled` 为何不在摘要里
///
/// 它表达的是**库里当前的状态**，而摘要算的是**本轮应当是什么**——两者不同源，
/// 把它算进来会让摘要随上一轮写入而变，失去「内容没变就跳过」的意义。
///
/// 代价是一个必须由调用方补上的守卫：某行被补集停用后，若本轮内容与上轮完全相同，
/// 摘要相同 → 跳过写库 → **那行永远不会被重新启用**。所以调用方在走「摘要相同即跳过」
/// 的快路径前，必须先确认该数据源**当前没有已停用的行**（一次 `count` 查询）。
pub(crate) fn snapshot_digest(derived: &[DerivedOption]) -> String {
    let mut hasher_input = String::new();
    // 规则版本并入输入：换规则而源内容不变时，摘要必须跟着变。
    hasher_input.push_str(&format!("v{DERIVE_RULE_VERSION}{SEPARATOR}"));
    for option in derived {
        hasher_input.push_str(&option.option_id);
        hasher_input.push(SEPARATOR);
        hasher_input.push_str(&option.parent_key);
        hasher_input.push(SEPARATOR);
        hasher_input.push_str(&option.label);
        hasher_input.push(SEPARATOR);
        hasher_input.push_str(&option.sort_order.to_string());
        hasher_input.push(SEPARATOR);
    }
    hash_token(&hasher_input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values<'a>(labels: &'a [&'a str]) -> Vec<RawValue<'a>> {
        labels
            .iter()
            .map(|label| RawValue {
                parent_label: None,
                label,
            })
            .collect()
    }

    #[test]
    fn option_id_is_prefixed_by_source_key() {
        // 表级唯一索引：两个数据源用同一个 label 必须不可能碰撞
        let a = derive_options("alpha", None, &values(&["北京"]));
        let b = derive_options("beta", None, &values(&["北京"]));
        assert_ne!(a[0].option_id, b[0].option_id);
        assert!(a[0].option_id.starts_with("alpha:"));
        assert!(b[0].option_id.starts_with("beta:"));
    }

    #[test]
    fn option_id_is_stable_across_rounds() {
        // 「固定」是飞书的硬要求：同一份数据每轮必须派生出同一批 id
        let first = derive_options("demo", None, &values(&["北京", "上海"]));
        let second = derive_options("demo", None, &values(&["北京", "上海"]));
        assert_eq!(first, second);
    }

    #[test]
    fn same_label_under_different_parents_does_not_collide() {
        // 父值进哈希的直接目的。按 label 去重会把这两条并成一个，父级选到其中一个
        // 时另一个会静默消失。
        let rows = [
            RawValue {
                parent_label: Some("USD 美元"),
                label: "6.9025",
            },
            RawValue {
                parent_label: Some("HKD 港币"),
                label: "6.9025",
            },
        ];
        let derived = derive_options("fx", Some("currency"), &rows);
        assert_eq!(derived.len(), 2, "同 label 不同父必须派生出两个选项");
        assert_ne!(derived[0].option_id, derived[1].option_id);
        assert_ne!(derived[0].parent_key, derived[1].parent_key);
    }

    #[test]
    fn parent_key_points_at_the_parent_sources_option_id() {
        let rows = [RawValue {
            parent_label: Some("USD 美元"),
            label: "6.9025",
        }];
        let derived = derive_options("fx", Some("currency"), &rows);
        let expected = derive_options("currency", None, &values(&["USD 美元"]))[0]
            .option_id
            .clone();
        assert_eq!(derived[0].parent_key, expected);
        // 父键必须带父数据源的前缀，否则读端拼不出来
        assert!(derived[0].parent_key.starts_with("currency:"));
    }

    #[test]
    fn duplicate_ids_are_deduped_by_id_not_by_label() {
        // 同一父下完全重复的行只产出一次
        let rows = [
            RawValue {
                parent_label: Some("USD 美元"),
                label: "6.9025",
            },
            RawValue {
                parent_label: Some("USD 美元"),
                label: "6.9025",
            },
        ];
        assert_eq!(derive_options("fx", Some("currency"), &rows).len(), 1);
    }

    #[test]
    fn blank_labels_are_skipped_and_do_not_consume_a_sort_order() {
        // 台账里 228 行只有 43 行有值，空单元格不贡献任何值
        let rows = [
            RawValue {
                parent_label: None,
                label: "  ",
            },
            RawValue {
                parent_label: None,
                label: "第一个",
            },
            RawValue {
                parent_label: None,
                label: "",
            },
            RawValue {
                parent_label: None,
                label: "第二个",
            },
        ];
        let derived = derive_options("demo", None, &rows);
        assert_eq!(derived.len(), 2);
        assert_eq!(derived[0].sort_order, 0);
        assert_eq!(derived[1].sort_order, 1, "空值不得占用序号");
    }

    #[test]
    fn labels_are_trimmed() {
        let derived = derive_options("demo", None, &values(&["  北京  "]));
        assert_eq!(derived[0].label, "北京");
        // trim 后再哈希：否则同一个值因首尾空格不同而派生出两个 id
        let untrimmed = derive_options("demo", None, &values(&["北京"]));
        assert_eq!(derived[0].option_id, untrimmed[0].option_id);
    }

    #[test]
    fn a_declared_child_without_a_parent_source_falls_back_to_no_parent() {
        // 有父文案却缺父源配置时不猜：猜错会让整棵子树挂到错误的父上
        let rows = [RawValue {
            parent_label: Some("USD 美元"),
            label: "6.9025",
        }];
        let derived = derive_options("fx", None, &rows);
        assert_eq!(derived[0].parent_key, "", "缺父源配置时按无父处理");
    }

    #[test]
    fn blank_parent_label_yields_no_parent_key() {
        // 「父列有值、子列为空」与「父列为空」都要落成无父，而不是挂到空父上
        let rows = [RawValue {
            parent_label: Some("   "),
            label: "6.9025",
        }];
        let derived = derive_options("fx", Some("currency"), &rows);
        assert_eq!(derived[0].parent_key, "");
    }

    #[test]
    fn sort_order_follows_snapshot_row_order() {
        let derived = derive_options("demo", None, &values(&["丙", "甲", "乙"]));
        assert_eq!(
            derived.iter().map(|o| o.sort_order).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(
            derived.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(),
            vec!["丙", "甲", "乙"],
            "必须保持快照行序，不能按文案排序"
        );
    }

    #[test]
    fn digest_is_stable_and_content_sensitive() {
        let a = derive_options("demo", None, &values(&["北京", "上海"]));
        let b = derive_options("demo", None, &values(&["北京", "上海"]));
        assert_eq!(snapshot_digest(&a), snapshot_digest(&b), "同内容必须同摘要");

        let c = derive_options("demo", None, &values(&["北京", "广州"]));
        assert_ne!(
            snapshot_digest(&a),
            snapshot_digest(&c),
            "内容变了摘要必须变"
        );

        // 顺序变了摘要也要变：顺序进 sort_order，而 sort_order 决定翻页游标
        let reordered = derive_options("demo", None, &values(&["上海", "北京"]));
        assert_ne!(snapshot_digest(&a), snapshot_digest(&reordered));
    }

    #[test]
    fn digest_changes_with_the_source_key_and_parent() {
        let a = derive_options("alpha", None, &values(&["北京"]));
        let b = derive_options("beta", None, &values(&["北京"]));
        assert_ne!(
            snapshot_digest(&a),
            snapshot_digest(&b),
            "不同数据源的同一份文案必须是不同摘要"
        );
    }

    #[test]
    fn digest_of_an_empty_snapshot_is_computable() {
        // 空快照也有摘要；「要不要据此停用补集」是调用方的决定，不是本函数的
        let digest = snapshot_digest(&[]);
        assert_eq!(digest.len(), 64, "恒为 sha256 hex");
    }

    #[test]
    fn rule_version_is_part_of_the_digest_input() {
        // 回归守卫：摘要必须**把规则版本并入输入**，而不是只哈希内容。
        // 少了版本，「换派生规则而源内容不变」的场景摘要不变 → 新列永远填不上。
        // 这里手工重建「不含版本」的同一份输入，断言两者不同——
        // 有人删掉 snapshot_digest 里那行 push_str 时这条会红。
        let derived = derive_options("demo", None, &values(&["北京"]));
        let mut without_version = String::new();
        for option in &derived {
            without_version.push_str(&option.option_id);
            without_version.push(SEPARATOR);
            without_version.push_str(&option.parent_key);
            without_version.push(SEPARATOR);
            without_version.push_str(&option.label);
            without_version.push(SEPARATOR);
            without_version.push_str(&option.sort_order.to_string());
            without_version.push(SEPARATOR);
        }
        assert_ne!(
            snapshot_digest(&derived),
            hash_token(&without_version),
            "摘要必须并入 DERIVE_RULE_VERSION；只哈希内容会让换规则后摘要不变"
        );
    }
}
