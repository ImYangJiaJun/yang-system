//! `feishu_approval_task_applicant` 表声明：`feishu_approval_task` 的**属性扩展**。
//!
//! 设计见 `docs/architecture/2026-10-12-feishu-approval-backfill-initiator-tracking-design.md`
//! §8：全表异步受理时拿不到发起人，open_id 要等 `get_instance` 才能解析出来。Action
//! 先把解析结果落这里，worker 再读这里回填。
//!
//! 2026-10-16 起**单条点击**也写这里（见 [`upsert_task_applicant`]）：那条路径原本
//! 只在一次同步派发里用掉解析结果，worker 重试读的是表格申请人列，于是报出与点击时
//! 不同的原因、通知也退回记录创建人。
//!
//! 单独成表而不是给 `feishu_approval_task` 加列——那张表是派发**认领队列**，本表是
//! 解析结果的存放点，写入方与生命周期都不同。
//!
//! 不声明 `created_at`/`updated_at`：本表是任务的属性扩展而非台账，写入即终态，
//! 没有按时间扫描的查询。

use yang_base::definition::{FieldName, FieldRef, Int, Key, Str, TableName, TableSpec};
use yang_base::table::Record;
use yang_base::BaseError;

use crate::addon::feishu::domain::context::FeishuContext;

/// 按 `task_id` upsert 一行发起人：已有则更新 `open_id`，没有则插入。
///
/// # 两条写入路径共用
///
/// - **全表受理**：Action 解析出发起人后交给 worker，worker 播种任务行时落。
/// - **单条点击**（2026-10-16 补）：Action 回写任务行后落自己解析出的那个。
///
/// 后者原先缺失，后果是真机实测出来的：单条点击解析出的发起人只用于那一次同步
/// 派发，worker 重试时看不到它，于是去读表格的申请人列（空着正是 waiting 的主因），
/// 报出与点击时**不同**的阻塞原因，通知也退回「记录创建人」兜底——用户明明传了
/// 发起人，收到的消息却在说「缺少申请人」。
///
/// # 为什么收在这里
///
/// 查-改-插这套动作在两条路径上必须同口径：各写一遍就可能一边插入、一边更新，
/// 给同一个任务留下两行发起人（读取侧只取 `optional()` 的第一行，谁是「那个发起人」
/// 就变成看运气）。`task_id` 上的唯一索引 `uk_feishu_approval_task_applicant_task_id`
/// （见本文件 `table_spec`）是最后一道防线：并发插入时后者撞键，退化成更新。
pub(crate) async fn upsert_task_applicant(
    context: &FeishuContext,
    task_id: i64,
    open_id: &str,
) -> Result<(), BaseError> {
    let existing = context
        .approval_task_applicants()
        .query()
        .where_eq("task_id", serde_json::json!(task_id))?
        .optional()
        .await?;
    let mut row = Record::new();
    if existing.is_some() {
        row.insert("open_id", serde_json::json!(open_id));
        context
            .approval_task_applicants()
            .query()
            .where_eq("task_id", serde_json::json!(task_id))?
            .update(row)
            .await?;
    } else {
        row.insert("task_id", serde_json::json!(task_id));
        row.insert("open_id", serde_json::json!(open_id));
        let inserted = context.approval_task_applicants().query().insert(row).await;
        if let Err(error) = inserted {
            // 并发路径已经插入同一 task_id 时，唯一索引会拒绝本次 INSERT；
            // 退化成更新即可（本表的不变式是「一个任务至多一行发起人」）。
            let mut update = Record::new();
            update.insert("open_id", serde_json::json!(open_id));
            let affected = context
                .approval_task_applicants()
                .query()
                .where_eq("task_id", serde_json::json!(task_id))?
                .update(update)
                .await?;
            if affected != 1 {
                return Err(error);
            }
        }
    }
    Ok(())
}

fn field_ref(table_name: &TableName, field: &str) -> Result<FieldRef, BaseError> {
    let field = FieldName::new(field).map_err(|error| BaseError::ConfigError(error.to_string()))?;
    Ok(FieldRef::new(table_name.clone(), field))
}

/// 申请人解析结果表。
pub(crate) fn table_spec() -> Result<TableSpec, BaseError> {
    let table_name = yang_base::table!("feishu_approval_task_applicant");
    Ok(TableSpec::new(table_name.clone())
        .title("飞书审批任务申请人")
        .fields(yang_base::fields! {
            // 与 `task_table` 同一形态：`sortable`/`filterable` 是 fail-closed 的，
            // 漏了只在运行期报 FieldPermissionDenied，启动与单测都发现不了。
            id => Key::new().title("ID").filterable(true).sortable(true),
            task_id => Int::new()
                .title("任务 ID")
                .require(true)
                .indexed(true)
                .filterable(true),
            open_id => Str::new()
                .title("发起人 Open ID")
                .require(true)
                .max_length(64),
        })
        // 「一个任务至多一行发起人」由数据库钉死：读-改-插在两条写入路径并发时
        // 会各插一行，读取侧只取第一行，谁是发起人就成了看运气。
        .unique_named(
            "uk_feishu_approval_task_applicant_task_id",
            [field_ref(&table_name, "task_id")?],
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> yang_base::table::TableDefinition {
        table_spec()
            .unwrap_or_else(|error| panic!("表声明应有效: {error}"))
            .table_definition()
            .unwrap_or_else(|error| panic!("应可编译为表定义: {error}"))
    }

    #[test]
    fn table_has_expected_name_and_primary_key() {
        let definition = definition();
        assert_eq!(definition.name(), "feishu_approval_task_applicant");
        assert_eq!(definition.primary_key(), "id");
    }

    #[test]
    fn task_id_and_open_id_are_required() {
        // 两列都不可空：没有 task_id 无法回填，没有 open_id 这张表就没有意义。
        let definition = definition();
        for name in ["task_id", "open_id"] {
            let field = definition
                .field(name)
                .unwrap_or_else(|| panic!("{name} 字段必须存在"));
            assert!(field.is_required(), "{name} 必填");
        }
    }

    #[test]
    fn every_field_has_an_explicit_chinese_label() {
        let definition = definition();
        for field in definition.fields() {
            assert!(
                !field.label().is_empty(),
                "字段 {} 必须有展示名",
                field.name()
            );
            assert_ne!(
                field.label(),
                field.name(),
                "字段 {} 的展示名退化成了字段名（忘了 .title(..)）",
                field.name()
            );
        }
    }

    #[test]
    fn task_id_has_a_unique_index_for_the_single_row_invariant() {
        let spec = table_spec().unwrap_or_else(|error| panic!("表声明应有效: {error}"));
        assert!(
            spec.indexes.iter().any(|index| {
                index.unique
                    && index.name.as_deref() == Some("uk_feishu_approval_task_applicant_task_id")
            }),
            "task_id 必须有唯一索引，防止并发 upsert 留下两行"
        );
    }
}
