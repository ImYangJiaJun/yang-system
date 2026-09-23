//! 级联（父级）在**写端**的形状：一条绑定的父列声明。
//!
//! # 为什么不在这里解析 JSON 了
//!
//! 表级改造前，级联声明存在 `feishu_datasource.linkage_mapping` 的一段手写 JSON 里：
//! 读端（`approval_options`）解析它决定按哪个父数据源过滤，写端（`pull`）消费同形状的
//! 一对值。那一列已经被**整个删除**（设计 §5、§8）：现在父由字段绑定行上的
//! `parent_field_id` 指向**同一张表**里的另一条绑定——读端按绑定体系推出父的
//! `source_key`（见 `approval_options::load_parent_source_key`），写端按同一指针取父的
//! **当前**列名（见 `pull::parent_linkage`）。
//!
//! 因此本模块只剩一件事：把「父的 `source_key` + 父在本表里的列名」这一对值定义成
//! 一个类型，供写端构造、供派生（`derive`）消费。
//!
//! 曾经在此的 `parse_linkage_mapping` / `match_linkage` 与通配键 `"*"` 解析，都是为
//! 那段 JSON 服务的。JSON 列没了之后它们已无任何生产消费者（已逐一确认：写端只用
//! [`Linkage`] 这个类型本身），故**连同其单测一并删除**——留着一段再也跑不到的解析器
//! 只会让人以为级联仍是 JSON 驱动的。裁定记在
//! `docs/architecture/feishu-datasource-table-config.md` §8 与本次修复的 ledger。

/// 一条绑定的父列声明。
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
