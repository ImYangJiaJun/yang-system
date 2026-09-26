//! 级联（父级）在**写端**的形状：一条绑定的祖先链。
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
//! 因此本模块只剩一件事：把「这条绑定的各级祖先是谁」定义成一个类型，供写端构造、
//! 供派生（`derive`）消费。
//!
//! 曾经在此的 `parse_linkage_mapping` / `match_linkage` 与通配键 `"*"` 解析，都是为
//! 那段 JSON 服务的。JSON 列没了之后它们已无任何生产消费者（已逐一确认：写端只用
//! [`Linkage`] 这个类型本身），故**连同其单测一并删除**——留着一段再也跑不到的解析器
//! 只会让人以为级联仍是 JSON 驱动的。裁定记在
//! `docs/architecture/feishu-datasource-table-config.md` §8 与本次修复的 ledger。
//!
//! # 为什么是**一串**祖先，而不是一个父
//!
//! 2026-09-27 改。原来这里只有「父的 `source_key` + 父在本表的列名」两项，派生时据此
//! 用 `parent_option_id(父 source_key, 父文案)` 算父键——那个式子把 `option_id_of` 的
//! 中间参数**硬编码成 `None`**，等价于断言「父源自己没有父」。
//!
//! 但读端拿的是**父行真实的 `option_id`** 去做等值匹配，而父行自己的 id 折进了它的父键。
//! 于是父一旦是链的中间列，子算出的父键与父的真实 id **恒不相等**，那个字段的下拉
//! **静默变空**（真机实测：`fldm0j5do3` 43 个子项 0 命中）。
//!
//! 修法不是改读端，而是让写端算对：**父键 = 从根到直接父逐级折叠出来的键**。
//! 这要求写端拿得到整条祖先链的文案，所以这个类型从「一个父」扩成「一串祖先」。
//!
//! 这一维是**有真实消费者**地加回来的，与当初删掉 `cascade_field`（零消费）性质不同：
//! 少了它，深度 ≥2 的字段就没有正确的父键可算。

/// 链上的一级祖先：它是哪条数据源、在**本表**里是哪一列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkageLevel {
    /// 该级数据源的 `source_key`。进 `option_id` 的前缀，读端靠它拼父键。
    pub(crate) source_key: String,
    /// **本表**里承载该级文案的列名（父键由同行共现读出）。
    ///
    /// 取**本轮解析后**的名字，不用绑定行上的缓存：缓存是上一轮的名字，父列改名后按旧
    /// 名字读快照会一片空白（`record.fields.get` 取不到键，整棵子树的父键一起塌成空）。
    pub(crate) field_name: String,
}

/// 一条绑定的祖先链，**从根到直接父**。空 = 这条绑定没有父。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Linkage {
    pub(crate) ancestors: Vec<LinkageLevel>,
}

impl Linkage {
    /// 各级的 `source_key`，顺序同 [`Linkage::ancestors`]。派生按它与祖先文案逐位折叠。
    pub(crate) fn source_keys(&self) -> Vec<&str> {
        self.ancestors
            .iter()
            .map(|level| level.source_key.as_str())
            .collect()
    }
}
