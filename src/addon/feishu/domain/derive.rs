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
//! # 父键：必须等于父的 `option_id`，而这要求沿**整条祖先链**折叠
//!
//! 读端拿**父行的真实 `option_id`** 去与子行的 `parent_key` 做等值匹配
//! （`approval_options.rs` 的 `resolve_parent_key` → `where_in("parent_key", …)`），
//! 所以唯一正确的不变量是：
//!
//! ```text
//! 子行的 parent_key  ==  父行自己的 option_id
//! ```
//!
//! 2026-09-27 之前，父键由 [`option_id_of`] 恒传 `None` 算出来（即假定「父源自己没有父」）。
//! 只要父行自己也有一格文案，它的 id 就把**祖父键**哈希进了输入，两式只差这一个入参而
//! **恒不相等**——于是父是中间列的那些字段，下拉静默变空。
//!
//! 现在改成一件事：**父键 = 从根到直接父那一串逐级折叠出来的键**
//! （[`ancestor_key`]），子行自己的 id 则由同一个折叠再叠上本行。
//! 两式由同一个函数、同一组输入算出，相等是**构造性**的，与链深无关。
//!
//! 真机实测（2026-09-26）确认过这个失效形态不是「三级一律坏」而是更刁钻的一种：
//! 存下来的 `parent_key` 只在**父行自己那一格的父键为空**时才等于父的真实 `option_id`。
//! 所以同一个链里有的字段好用（它恰好只挂在父的无父行下）、有的全空
//! （`fldm0j5do3` 43 个子项 0 命中）——按「三级一律坏」去排查会找不到规律。
//!
//! # 空级怎么处理
//!
//! - **直接父那一格为空** ⇒ 本行没有父（`parent_key` 落空串）。
//!   这一条**不能**靠「跳过空级」实现：跳过等于把本行挂到祖父上。
//! - **中间的某一级为空** ⇒ 跳过那一级。父行自己的 id 也跳过了它，所以两边一致。
//! - 整条链为空（顶层字段）⇒ 走无父分支。
//!
//! # 为什么不按 label 折叠
//!
//! **去重必须按 `option_id`，不能按 label。** 按 label 折叠会把「同 label 不同父」
//! 重新并成一个，父级选到其中一个时另一个**静默消失**。

use std::collections::HashSet;

use super::token::hash_token;

/// 派生规则版本。**改动派生口径（哈希输入、id 形状、文案拼法）时必须手动 bump**，
/// 并把它并入摘要输入——否则换了规则而源内容不变时，新列永远填不上。
///
/// v2（2026-09-27）：父键由「一跳」改为「沿祖先链折叠」。**变的只有**深度 ≥ 2
/// 且父行自己那一格的父键非空的行；深度 0/1 的行（以及父行无父的那些行）口径与
/// 取值都不变——根行的父键为空，折叠与旧口径算出的是同一个式子。
pub(crate) const DERIVE_RULE_VERSION: u32 = 2;

/// 哈希输入里的分隔符。用 ASCII 单元分隔符（US, 0x1F）而不是 `:` 或 `|`：
/// 选项文案是外部数据，可能含任何可打印字符。与 `domain/pagination.rs` 的游标
/// 用同一个字符，理由相同。
const SEPARATOR: char = '\u{1f}';

/// `option_id` 里哈希部分的长度（hex 字符数）。
const HASH_PREFIX_LEN: usize = 12;

/// 一行待派生的原始取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RawValue<'a> {
    /// **祖先链的文案**，从根到**直接父**，与 [`derive_options`] 的
    /// `ancestor_sources` 逐位对应。顶层字段为空切片。
    ///
    /// 每一级读的是**同一条 record** 里那一列的值（见 `pull::extract_values_owned`），
    /// 而不是去父表反查——这是设计 A7b 的直接依据。链有多长就取几格。
    ///
    /// 某一级为空串表示那一格没值（或不是文本）。**直接父那一格为空 ⇒ 本行没有父**；
    /// 中间某级为空 ⇒ 跳过那一级（父行的 id 也跳过了它）。见模块文档「空级怎么处理」。
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
    /// 固有形态，接受。
    pub(crate) ancestors: &'a [&'a str],
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

/// 由**整条祖先链**算父键：从根往下逐级折叠，得到的就是直接父那一行的 `option_id`。
///
/// `ancestor_sources` 与 `ancestors` 逐位对应（`ancestors[i]` 是 `ancestor_sources[i]`
/// 那一列在**本行**里的文案）。正常情况下两者等长；**不等长按较短的算**——
/// `ancestor_sources` 短表示这条绑定声明了父文案却没配（全）父源，多出来的文案一律
/// 忽略，**不猜**。这与本模块一贯的取舍一致：猜错会让整棵子树挂到错误的父上。
/// 短到一级都不剩就落空串（无父）。
///
/// 两条空值规则见模块文档「空级怎么处理」，这里是它们唯一的实现处：
///
/// - **直接父为空 ⇒ 整条链作废**（返回空串 = 无父）。这一步**不能**并进下面那个
///   "跳过空级"里：跳过直接父等于把本行挂到祖父上，而祖父并不是它的父。
/// - **中间某级为空 ⇒ 跳过那一级**。父行自己的 id 是同一个折叠算出来的，也跳过了它。
///
/// 「直接父」取的是 `ancestors.last()`（**文案**那一侧）而不是 source 那一侧：
/// 缺父源配置时正是靠这一侧判出「有一格父文案」的。
pub(crate) fn ancestor_key(ancestor_sources: &[&str], ancestors: &[&str]) -> String {
    let Some(direct_parent) = ancestors.last() else {
        return String::new();
    };
    if direct_parent.trim().is_empty() {
        return String::new();
    }
    let mut key = String::new();
    for (source_key, label) in ancestor_sources.iter().zip(ancestors.iter()) {
        let label = label.trim();
        if label.is_empty() {
            continue;
        }
        // 头一级的 `key` 是空串，`option_id_of` 把空串当 `None` 处理，
        // 所以根那一级落的是无父分支——与它作为根行被派生时同一个式子。
        key = option_id_of(source_key, Some(&key), label);
    }
    key
}

/// 把一轮快照的取值派生成选项。
///
/// 按 `option_id` 去重并**保留首次出现的顺序**：`sort_order` 取该顺序的序号，
/// 取选项端点按 `sort_order ASC, option_id ASC` 排序并据此编码游标
/// （`approval_options.rs`）——全落 0 会让选项序退化成哈希序且跨页不稳。
///
/// 空文案（trim 后为空）一律跳过：空单元格不贡献任何值。
///
/// `ancestor_sources` 是从**根到直接父**的 `source_key` 链；没有父时传空切片。
/// 有父文案却缺父源配置时**不猜**，按无父处理——猜错会让整棵子树挂到错误的父上。
pub(crate) fn derive_options(
    source_key: &str,
    ancestor_sources: &[&str],
    values: &[RawValue<'_>],
) -> Vec<DerivedOption> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut derived = Vec::new();
    for value in values {
        let label = value.label.trim();
        if label.is_empty() {
            continue;
        }
        let parent_key = ancestor_key(ancestor_sources, value.ancestors);
        // 本行的 id = 同一条折叠再叠上本行，所以 `child.parent_key == parent.option_id`
        // 是构造性成立的（见模块文档）。
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

    /// 顶层字段的行：没有祖先。
    fn values<'a>(labels: &'a [&'a str]) -> Vec<RawValue<'a>> {
        labels
            .iter()
            .map(|label| RawValue {
                ancestors: &[],
                label,
            })
            .collect()
    }

    /// 带祖先链的行。`ancestors` 从根到直接父。
    fn with_ancestors<'a>(ancestors: &'a [&'a str], label: &'a str) -> RawValue<'a> {
        RawValue { ancestors, label }
    }

    // -----------------------------------------------------------------------
    // 不变式：子行的 parent_key 必须等于父行自己的 option_id
    //
    // 这是读端 `where_in("parent_key", 父的 option_id)` 的唯一依据。
    // 2026-09-26 真机实测：旧口径下这条只在「父行自己的父键为空」时才成立，
    // `fldm0j5do3` 的 43 个子项 0 命中。
    // -----------------------------------------------------------------------

    #[test]
    fn depth_one_rows_keep_the_old_formula() {
        // 折叠链只有一级时，第一级的 key 从空串起，而 `option_id_of` 把空串当 `None`
        // ——算出来就是旧口径那个式子。这是「bump 只影响深度 ≥2」的机械依据：
        // 若哪天 `option_id_of` 不再把空串当 `None`，深度 1 也会开始 churn，
        // 而本用例会红。
        let root = derive_options("cat", &[], &values(&["大类A"]));
        let child = derive_options("type", &["cat"], &[with_ancestors(&["大类A"], "类型X")]);
        assert_eq!(child[0].parent_key, root[0].option_id);
        assert!(child[0].parent_key.starts_with("cat:"), "父键带父源前缀");
    }

    #[test]
    fn a_two_level_chain_pairs_correctly() {
        let parents = derive_options("currency", &[], &values(&["USD 美元", "HKD 港币"]));
        let children = derive_options(
            "fx",
            &["currency"],
            &[
                with_ancestors(&["USD 美元"], "6.9025"),
                with_ancestors(&["HKD 港币"], "6.9025"),
            ],
        );
        assert_eq!(children[0].parent_key, parents[0].option_id);
        assert_eq!(children[1].parent_key, parents[1].option_id);
    }

    #[test]
    fn three_level_chain_parent_key_equals_the_middle_rows_option_id() {
        // 修复前的失效点：中间那一行自己有父，它的 id 把祖父母的键也哈希了进去，
        // 而孙级算出的父键没有——两者恒不相等，第三级下拉静默变空。
        let middle = derive_options("type", &["cat"], &[with_ancestors(&["大类A"], "类型X")]);
        let leaf = derive_options(
            "code",
            &["cat", "type"],
            &[with_ancestors(&["大类A", "类型X"], "编码Z")],
        );
        assert_eq!(
            leaf[0].parent_key, middle[0].option_id,
            "三级链的父键必须命中中间行真实的 option_id"
        );
    }

    #[test]
    fn four_level_chain_pairs_correctly() {
        // 真机上那条链是四级（交易类型 → 费用大类 → 费用类型 → 银行流水摘要-编码）
        let l1 = derive_options("l1", &[], &values(&["甲"]));
        let l2 = derive_options("l2", &["l1"], &[with_ancestors(&["甲"], "乙")]);
        let l3 = derive_options("l3", &["l1", "l2"], &[with_ancestors(&["甲", "乙"], "丙")]);
        let l4 = derive_options(
            "l4",
            &["l1", "l2", "l3"],
            &[with_ancestors(&["甲", "乙", "丙"], "丁")],
        );
        assert_eq!(l2[0].parent_key, l1[0].option_id);
        assert_eq!(l3[0].parent_key, l2[0].option_id);
        assert_eq!(l4[0].parent_key, l3[0].option_id);
    }

    #[test]
    fn the_same_label_under_different_grandparents_no_longer_collapses() {
        // 修复前这两行会算成同一个 option_id（父键都按「中间行无父」算），
        // 于是「大类A / 大类B 下的同一个类型名」在库里坍成一条——
        // 一个跨祖父分支的静默错集。
        let a = derive_options(
            "code",
            &["cat", "type"],
            &[with_ancestors(&["大类A", "类型X"], "编码Z")],
        );
        let b = derive_options(
            "code",
            &["cat", "type"],
            &[with_ancestors(&["大类B", "类型X"], "编码Z")],
        );
        assert_ne!(a[0].parent_key, b[0].parent_key);
        assert_ne!(a[0].option_id, b[0].option_id);
    }

    // -----------------------------------------------------------------------
    // 空级的两条规则（模块文档「空级怎么处理」）
    // -----------------------------------------------------------------------

    #[test]
    fn a_blank_direct_parent_yields_no_parent_key() {
        // 「父列有值、子列为空」与「父列为空」都要落成无父，而不是挂到空父上
        let leaf = derive_options(
            "code",
            &["cat", "type"],
            &[with_ancestors(&["大类A", "   "], "编码Z")],
        );
        assert_eq!(leaf[0].parent_key, "");
    }

    #[test]
    fn a_blank_direct_parent_does_not_attach_to_the_grandparent() {
        // 「跳过空级」不能越过**直接父**那一格：跳过等于把本行挂到祖父上，
        // 而祖父并不是它的父。这一条与上一条是同一个判据的两面。
        let leaf = derive_options(
            "code",
            &["cat", "type"],
            &[with_ancestors(&["大类A", ""], "编码Z")],
        );
        assert_eq!(leaf[0].parent_key, "", "不得落到 cat: 前缀的祖父键上");
        assert!(!leaf[0].parent_key.starts_with("cat:"));
    }

    #[test]
    fn a_blank_middle_level_is_skipped_exactly_like_the_parent_did() {
        // 中间级为空 ⇒ 跳过。父行自己的 id 是同一个折叠算的，也跳过了它，所以仍相等。
        let middle = derive_options("type", &["cat"], &[with_ancestors(&[""], "类型X")]);
        let leaf = derive_options(
            "code",
            &["cat", "type"],
            &[with_ancestors(&["", "类型X"], "编码Z")],
        );
        assert_eq!(leaf[0].parent_key, middle[0].option_id);
    }

    #[test]
    fn a_missing_parent_source_config_falls_back_to_no_parent() {
        // 有父文案却缺父源配置时不猜：猜错会让整棵子树挂到错误的父上
        let rows = [with_ancestors(&["USD 美元"], "6.9025")];
        let derived = derive_options("fx", &[], &rows);
        assert_eq!(derived[0].parent_key, "", "缺父源配置时按无父处理");
    }

    // -----------------------------------------------------------------------
    // id 形状与稳定性
    // -----------------------------------------------------------------------

    #[test]
    fn option_id_is_prefixed_by_source_key() {
        // 表级唯一索引：两个数据源用同一个 label 必须不可能碰撞
        let a = derive_options("alpha", &[], &values(&["北京"]));
        let b = derive_options("beta", &[], &values(&["北京"]));
        assert_ne!(a[0].option_id, b[0].option_id);
        assert!(a[0].option_id.starts_with("alpha:"));
        assert!(b[0].option_id.starts_with("beta:"));
    }

    #[test]
    fn option_id_is_stable_across_rounds() {
        // 「固定」是飞书的硬要求：同一份数据每轮必须派生出同一批 id
        let first = derive_options("demo", &[], &values(&["北京", "上海"]));
        let second = derive_options("demo", &[], &values(&["北京", "上海"]));
        assert_eq!(first, second);
    }

    #[test]
    fn same_label_under_different_parents_does_not_collide() {
        // 父值进哈希的直接目的。按 label 去重会把这两条并成一个，父级选到其中一个
        // 时另一个会静默消失。
        let rows = [
            with_ancestors(&["USD 美元"], "6.9025"),
            with_ancestors(&["HKD 港币"], "6.9025"),
        ];
        let derived = derive_options("fx", &["currency"], &rows);
        assert_eq!(derived.len(), 2, "同 label 不同父必须派生出两个选项");
        assert_ne!(derived[0].option_id, derived[1].option_id);
        assert_ne!(derived[0].parent_key, derived[1].parent_key);
    }

    #[test]
    fn duplicate_ids_are_deduped_by_id_not_by_label() {
        // 同一父下完全重复的行只产出一次
        let rows = [
            with_ancestors(&["USD 美元"], "6.9025"),
            with_ancestors(&["USD 美元"], "6.9025"),
        ];
        assert_eq!(derive_options("fx", &["currency"], &rows).len(), 1);
    }

    #[test]
    fn blank_labels_are_skipped_and_do_not_consume_a_sort_order() {
        // 台账里 228 行只有 43 行有值，空单元格不贡献任何值
        let rows = [
            RawValue {
                ancestors: &[],
                label: "  ",
            },
            RawValue {
                ancestors: &[],
                label: "第一个",
            },
            RawValue {
                ancestors: &[],
                label: "",
            },
            RawValue {
                ancestors: &[],
                label: "第二个",
            },
        ];
        let derived = derive_options("demo", &[], &rows);
        assert_eq!(derived.len(), 2);
        assert_eq!(derived[0].sort_order, 0);
        assert_eq!(derived[1].sort_order, 1, "空值不得占用序号");
    }

    #[test]
    fn labels_are_trimmed() {
        let derived = derive_options("demo", &[], &values(&["  北京  "]));
        assert_eq!(derived[0].label, "北京");
        // trim 后再哈希：否则同一个值因首尾空格不同而派生出两个 id
        let untrimmed = derive_options("demo", &[], &values(&["北京"]));
        assert_eq!(derived[0].option_id, untrimmed[0].option_id);
    }

    #[test]
    fn ancestor_labels_are_trimmed_like_the_parent_row_was() {
        // 实测该表有 "CNY 人民币\n" 这种尾随换行。父键与父行自己的 id 必须用同一份
        // trim 后的文案，否则子项指向一个不存在的父 option_id——失效形态是静默空下拉。
        let parent = derive_options("currency", &[], &values(&["CNY 人民币\n"]));
        let child = derive_options(
            "fx",
            &["currency"],
            &[with_ancestors(&["CNY 人民币\n"], "1.0000")],
        );
        assert_eq!(parent.len(), 1, "两种写法折叠成同一个父选项");
        assert_eq!(child[0].parent_key, parent[0].option_id);
    }

    #[test]
    fn sort_order_follows_snapshot_row_order() {
        let derived = derive_options("demo", &[], &values(&["丙", "甲", "乙"]));
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

    // -----------------------------------------------------------------------
    // 摘要
    // -----------------------------------------------------------------------

    #[test]
    fn digest_is_stable_and_content_sensitive() {
        let a = derive_options("demo", &[], &values(&["北京", "上海"]));
        let b = derive_options("demo", &[], &values(&["北京", "上海"]));
        assert_eq!(snapshot_digest(&a), snapshot_digest(&b), "同内容必须同摘要");

        let c = derive_options("demo", &[], &values(&["北京", "广州"]));
        assert_ne!(
            snapshot_digest(&a),
            snapshot_digest(&c),
            "内容变了摘要必须变"
        );

        // 顺序变了摘要也要变：顺序进 sort_order，而 sort_order 决定翻页游标
        let reordered = derive_options("demo", &[], &values(&["上海", "北京"]));
        assert_ne!(snapshot_digest(&a), snapshot_digest(&reordered));
    }

    #[test]
    fn digest_changes_with_the_source_key_and_parent() {
        let a = derive_options("alpha", &[], &values(&["北京"]));
        let b = derive_options("beta", &[], &values(&["北京"]));
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
        let derived = derive_options("demo", &[], &values(&["北京"]));
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

    #[test]
    fn the_rule_version_is_bumped_for_the_chain_fold() {
        // v2 是「父键沿祖先链折叠」。这条不是形式主义：版本进了摘要输入，
        // 不 bump 的话「口径变了而源内容没变」的字段摘要相同 → 跳过写库 →
        // 旧的错误 parent_key 永远留在库里。
        assert_eq!(DERIVE_RULE_VERSION, 2);
    }
}
