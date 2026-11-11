//! 账号删除时清理飞书侧个人数据。
//!
//! account 域在匿名化事务里调用本钩子；删除顺序必须先子后父：
//! `feishu_batch_job_item` → `feishu_batch_job` → `feishu_user_credential`，
//! 最后由 account 删除 `user_identity`。凭证不保留，避免匿名化账号仍能用旧
//! 令牌访问飞书。

use crate::addon::account::AccountDeletionHook;
use crate::db::Transaction;
use serde_json::Value;
use yang_base::action::ActionContext;
use yang_base::BaseError;

use super::batch_job_item_table::ITEM_JOB_ID;
use super::batch_job_table::{JOB_ID, JOB_USER_ID};
use super::context::FeishuContext;
use super::user_credential_table::CREDENTIAL_USER_ID;

#[async_trait::async_trait]
impl AccountDeletionHook for FeishuContext {
    async fn delete_user_data(
        &self,
        _ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<(), BaseError> {
        let jobs = self
            .batch_jobs()
            .query()
            .select_fields(&[JOB_ID])?
            .where_eq(JOB_USER_ID, Value::from(user_id))?
            .all_in_tx(transaction)
            .await?;
        let job_ids: Vec<Value> = jobs
            .into_iter()
            .filter_map(|row| row.optional::<i64>(JOB_ID).ok().flatten())
            .map(Value::from)
            .collect();
        if !job_ids.is_empty() {
            self.batch_job_items()
                .query()
                .where_in(ITEM_JOB_ID, job_ids)?
                .delete_in_tx(transaction)
                .await?;
        }
        self.batch_jobs()
            .query()
            .where_eq(JOB_USER_ID, Value::from(user_id))?
            .delete_in_tx(transaction)
            .await?;
        self.user_credentials()
            .query()
            .where_eq(CREDENTIAL_USER_ID, Value::from(user_id))?
            .delete_in_tx(transaction)
            .await?;
        Ok(())
    }
}
