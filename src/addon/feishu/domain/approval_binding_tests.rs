//! 按需选项与首次提交快照的回归；真库用例只操作专用测试库。
#[cfg(test)]
mod tests {
    use super::super::super::approval_match::{Column, FormWidget};
    use super::super::super::approval_option_binding::{
        bindings_from_rows, exact_option, load_bindings, resolve_options, BindingLevel,
        ExternalBinding,
    };
    use super::super::super::context::FeishuContext;
    use super::super::super::repository::Repository;
    use super::super::tests::{fake_tokens, NoSleep, RecordingBackfill, ScriptedTransport};
    use super::super::*;
    use serde_json::{json, Map};
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use yang_base::{table::Record, BaseError};

    fn row(value: Value) -> Record {
        let mut row = Record::new();
        for (key, value) in value.as_object().unwrap_or_else(|| panic!("object")) {
            row.insert(key, value.clone());
        }
        row
    }

    fn columns() -> Vec<Column> {
        ["省", "市", "支行"]
            .iter()
            .enumerate()
            .map(|(i, name)| Column {
                field_id: format!("target{i}"),
                field_name: (*name).into(),
                options: vec![],
            })
            .collect()
    }

    fn rows() -> Vec<Record> {
        ["省", "市", "支行"]
            .iter()
            .enumerate()
            .map(|(i, name)| {
                row(json!({
                    "id": i + 1, "datasource_id": 1, "field_id": format!("source{i}"),
                    "field_name": name, "source_key": format!("bank{i}"), "enabled": true,
                    "parent_field_id": if i == 0 { None } else { Some(format!("source{}", i - 1)) },
                }))
            })
            .collect()
    }

    fn binding() -> ExternalBinding {
        ExternalBinding {
            datasource_id: 1,
            levels: (0..3)
                .map(|i| BindingLevel {
                    binding_id: i + 1,
                    source_key: format!("bank{i}"),
                    field_id: format!("source{i}"),
                    bitable_field: format!("target{i}"),
                })
                .collect(),
        }
    }

    fn linked_widget() -> WidgetMap {
        WidgetMap {
            widget_id: "w".into(),
            widget_type: "radioV2".into(),
            required: true,
            bitable_field: "target2".into(),
            converter: Converter::Option,
            option_map: BTreeMap::new(),
            external_binding: Some(binding()),
            currency: None,
        }
    }

    #[test]
    fn binding_uses_complete_chain_and_ignores_unrelated_duplicate_or_blank_names() {
        let mut rows = rows();
        rows.push(row(json!({"field_name": "", "enabled": true})));
        rows.push(row(json!({"field_name": "无关", "enabled": true})));
        rows.push(row(json!({"field_name": "无关", "enabled": true})));
        let bindings = bindings_from_rows(&rows, &BTreeSet::from(["支行".into()]), &columns())
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bindings["支行"], binding());
    }

    #[test]
    fn binding_rejects_ambiguous_missing_disabled_cyclic_or_wrong_table_chains() {
        let names = BTreeSet::from(["支行".into()]);
        let base = rows();
        let mut duplicate = base.clone();
        duplicate.push(base[2].clone());
        let mut disabled = base.clone();
        disabled[1].insert("enabled", json!(false));
        let mut cycle = base.clone();
        cycle[0].insert("parent_field_id", json!("source2"));
        let mut wrong = base.clone();
        wrong[1].insert("datasource_id", json!(2));
        for rows in [
            duplicate,
            disabled,
            cycle,
            wrong,
            base[1..].to_vec(),
            vec![],
        ] {
            assert!(bindings_from_rows(&rows, &names, &columns()).is_err());
        }
        let mut duplicate_columns = columns();
        duplicate_columns.push(columns()[0].clone());
        assert!(bindings_from_rows(&base, &names, &duplicate_columns).is_err());
        assert!(bindings_from_rows(&base, &names, &columns()[1..]).is_err());
    }

    #[test]
    fn exact_label_never_picks_an_approximate_or_ambiguous_option() {
        let base = row(json!({"label": "银行A", "option_id": "a"}));
        let near = row(json!({"label": "银行a", "option_id": "b"}));
        assert_eq!(
            exact_option(&[base.clone(), near], "银行A").unwrap_or_else(|e| panic!("{e}")),
            "a"
        );
        assert!(exact_option(&[base.clone(), base], "银行A").is_err());
        assert!(exact_option(&[], "不存在").is_err());
    }

    #[test]
    fn binding_json_is_strict_and_legacy_mapping_is_preserved() {
        let mut value = json!({"widget_id":"w", "widget_type":"radioV2", "bitable_field":"target2", "converter":"option", "option_map":"{\"支行\":\"old\"}"});
        let read = |v: &Value| {
            widget_maps_from_rows(
                &[v.as_object().unwrap_or_else(|| panic!("object")).clone()],
                |_| None,
            )
        };
        let legacy = read(&value).unwrap_or_else(|| panic!("legacy"));
        assert_eq!(legacy[0].option_map["支行"], "old");
        assert!(legacy[0].external_binding.is_none());
        value["external_binding"] =
            json!(serde_json::to_string(&binding()).unwrap_or_else(|e| panic!("{e}")));
        assert_eq!(
            read(&value).unwrap_or_else(|| panic!("binding"))[0].external_binding,
            Some(binding())
        );
        for raw in [
            "{}",
            "null",
            "{\"datasource_id\":1,\"levels\":[],\"extra\":1}",
        ] {
            value["external_binding"] = json!(raw);
            assert!(read(&value).is_none());
        }
    }

    #[test]
    fn payload_validation_keeps_optional_empty_form_but_rejects_corruption() {
        assert!(parse_payload(r#"{"open_id":"ou_a","form":"[]"}"#).is_ok());
        for raw in [
            "null",
            "{}",
            r#"{"open_id":" ","form":"[]"}"#,
            r#"{"open_id":"ou_a","form":"{}"}"#,
            r#"{"open_id":"ou_a","form":"[]","extra":1}"#,
        ] {
            assert!(parse_payload(raw).is_err());
        }
    }

    fn context(pool: sqlx::MySqlPool) -> FeishuContext {
        let pool = Arc::new(pool);
        let repo = |spec: Result<yang_base::definition::TableSpec, BaseError>| {
            Repository::new(
                spec.unwrap_or_else(|e| panic!("{e}"))
                    .table_definition()
                    .unwrap_or_else(|e| panic!("{e}")),
                Arc::clone(&pool),
            )
        };
        use crate::addon::feishu::{approval, datasource, option};
        FeishuContext::new(
            repo(datasource::table::table_spec()),
            repo(datasource::domain::field_table::table_spec()),
            repo(option::table::table_spec()),
            repo(approval::table::table_spec()),
            repo(approval::domain::field_map_table::table_spec()),
            repo(approval::domain::task_table::table_spec()),
            repo(approval::domain::request_log_table::table_spec()),
            None,
        )
    }

    #[tokio::test]
    #[ignore = "需要专用 MySQL 测试库；154363 行选项与并发快照"]
    async fn real_binding_large_options_and_persisted_retry() -> anyhow::Result<()> {
        let url = std::env::var("YANG_SYSTEM_TEST_DATABASE_URL")?;
        let db = yang_db::Database::connect(&url).await?;
        let name: String = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(db.pool())
            .await?;
        anyhow::ensure!(name.ends_with("_test"), "拒绝非测试库");
        let tables = [
            "binding_snapshot_writes",
            "feishu_approval_task",
            "feishu_approval_field_map",
            "feishu_option",
            "feishu_datasource_field",
            "feishu_datasource",
        ];
        for table in tables {
            sqlx::query(&format!("DROP TABLE IF EXISTS `{table}`"))
                .execute(db.pool())
                .await?;
        }
        crate::schema::sync_with_database(
            db,
            yang_db::DatabaseConfig::default(),
            Arc::new(crate::config::SecuritySettings::default()),
        )
        .await?;
        let pool = sqlx::MySqlPool::connect(&url).await?;
        let context = context(pool.clone());
        let result: anyhow::Result<()> = async {
        sqlx::query("INSERT INTO feishu_approval_task (config_id,record_id,uuid,state,attempts,created_at,updated_at) VALUES (99,'legacy','legacy','pending',0,NOW(),NOW())").execute(&pool).await?;
        sqlx::query("ALTER TABLE feishu_approval_task DROP COLUMN prepared_payload").execute(&pool).await?;
        sqlx::query("ALTER TABLE feishu_approval_field_map DROP COLUMN external_binding").execute(&pool).await?;
        sqlx::query("DROP INDEX idx_feishu_option_label ON feishu_option").execute(&pool).await?;
        crate::schema::sync_with_database(yang_db::Database::connect(&url).await?, yang_db::DatabaseConfig::default(), Arc::new(crate::config::SecuritySettings::default())).await?;
        let legacy: (String, Option<String>) = sqlx::query_as("SELECT record_id,prepared_payload FROM feishu_approval_task WHERE uuid='legacy'").fetch_one(&pool).await?;
        assert_eq!(legacy, ("legacy".into(), None));
        let binding_column: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.COLUMNS WHERE TABLE_SCHEMA=DATABASE() AND TABLE_NAME='feishu_approval_field_map' AND COLUMN_NAME='external_binding' AND IS_NULLABLE='YES'").fetch_one(&pool).await?;
        assert_eq!(binding_column, 1);
        context.datasources().query().insert(row(json!({"title":"bank"}))).await?;
        for mut row in rows() {
            row.insert("token_hash", json!(format!("hash{}", row.require::<i64>("id")?)));
            context.datasource_fields().query().insert(row).await?;
        }
        for chunk in (0..154357).collect::<Vec<_>>().chunks(1000) {
            let mut query = sqlx::QueryBuilder::<sqlx::MySql>::new("INSERT INTO feishu_option (option_id,source_key,label,parent_key,enabled,created_at,updated_at) ");
            query.push_values(chunk, |mut b, i| { b.push_bind(format!("noise{i}")).push_bind("bank2").push_bind(format!("其他支行{i}")).push_bind("city_b").push_bind(true).push("NOW()").push("NOW()"); });
            query.build().execute(&pool).await?;
        }
        for (id, source, label, parent) in [
            ("province_a", "bank0", "省A", Some("")), ("province_b", "bank0", "省B", None),
            ("city_a", "bank1", "同名市", Some("province_a")), ("city_b", "bank1", "同名市", Some("province_b")),
            ("branch_a", "bank2", "同名支行", Some("city_a")), ("branch_b", "bank2", "同名支行", Some("city_b")),
        ] { context.options().query().insert(row(json!({"option_id":id,"source_key":source,"label":label,"parent_key":parent}))).await?; }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feishu_option").fetch_one(&pool).await?;
        assert_eq!(count, 154363);
        let form: Vec<FormWidget> = serde_json::from_value(json!([{"id":"w","name":"支行","type":"radioV2","externalData":{"externalDataLinkage":true}}]))?;
        assert_eq!(load_bindings(&context, &form, &columns()).await?["支行"], binding());
        let cells = json!({"target0":"省A","target1":"同名市","target2":"同名支行","applicant":[{"id":"ou_a"}]}).as_object().cloned().unwrap_or_default();
        sqlx::query("RENAME TABLE feishu_option TO binding_options_hidden").execute(&pool).await?;
        let without_options = load_bindings(&context, &form, &columns()).await;
        sqlx::query("RENAME TABLE binding_options_hidden TO feishu_option").execute(&pool).await?;
        assert_eq!(without_options?["支行"], binding(), "配置创建不能扫描选项表");
        let fixed_form: Vec<FormWidget> = serde_json::from_value(json!([{"id":"fixed","name":"固定","type":"input"}]))?;
        assert!(load_bindings(&context, &fixed_form, &[]).await?.is_empty());
        let mut widgets = vec![linked_widget()];
        resolve_options(&context, &mut widgets, &cells).await?;
        assert_eq!(widgets[0].option_map["同名支行"], "branch_a");
        let mut other_cells = cells.clone();
        other_cells.insert("target0".into(), json!("省B"));
        let mut other_widgets = vec![linked_widget()];
        resolve_options(&context, &mut other_widgets, &other_cells).await?;
        assert_eq!(other_widgets[0].option_map["同名支行"], "branch_b");
        for parent in [Value::Null, json!(["省A", "省B"])] {
            let mut missing_parent = cells.clone();
            missing_parent.insert("target0".into(), parent);
            let error = resolve_options(&context, &mut [linked_widget()], &missing_parent).await.err().unwrap_or_else(|| panic!("必须拒绝不唯一的父级"));
            assert!(error.to_string().contains("父级必须选择唯一文案"), "必须在查选项前拒绝不唯一的父级：{error}");
        }
        for key in ["binding_id", "source_key", "field_id", "bitable_field", "datasource_id", "levels", "cycle"] {
            let mut raw = serde_json::to_value(binding())?;
            match key {
                "binding_id" => raw["levels"][0][key] = json!(999),
                "source_key" | "field_id" | "bitable_field" => raw["levels"][2][key] = json!("changed"),
                "datasource_id" => raw[key] = json!(999),
                "cycle" => raw["levels"][1]["binding_id"] = json!(1),
                _ => raw[key] = json!([]),
            }
            let mut widget = linked_widget();
            widget.external_binding = Some(serde_json::from_value(raw)?);
            assert!(resolve_options(&context, &mut [widget], &cells).await.is_err(), "{key}");
        }
        sqlx::query("UPDATE feishu_datasource_field SET enabled=0 WHERE id=2").execute(&pool).await?;
        assert!(resolve_options(&context, &mut [linked_widget()], &cells).await.is_err());
        sqlx::query("UPDATE feishu_datasource_field SET enabled=1, parent_field_id='changed' WHERE id=2").execute(&pool).await?;
        assert!(resolve_options(&context, &mut [linked_widget()], &cells).await.is_err());
        sqlx::query("UPDATE feishu_datasource_field SET parent_field_id='source0' WHERE id=2").execute(&pool).await?;
        let mut optional = linked_widget();
        optional.required = false;
        optional.external_binding.as_mut().unwrap_or_else(|| panic!("binding")).levels.clear();
        resolve_options(&context, &mut [optional], &Map::new()).await?;
        for i in 0..100 {
            let label: String = "ABCDEFG".chars().enumerate().map(|(bit, c)| if i & (1 << bit) == 0 { c } else { c.to_ascii_lowercase() }).collect();
            context.options().query().insert(row(json!({"option_id":format!("cap{i}"),"source_key":"bank2","label":label,"parent_key":"city_a"}))).await?;
        }
        let mut capped_cells = cells.clone();
        capped_cells.insert("target2".into(), json!("ABCDEFG"));
        let error = resolve_options(&context, &mut [linked_widget()], &capped_cells).await.err().unwrap_or_else(|| panic!("must reject"));
        assert!(error.to_string().contains("过多近似命中"));
        let explain: String = sqlx::query_scalar("EXPLAIN FORMAT=JSON SELECT option_id,label FROM feishu_option WHERE source_key='bank2' AND enabled=1 AND label='同名支行' AND parent_key='city_a' LIMIT 100").fetch_one(&pool).await?;
        let explain: Value = serde_json::from_str(&explain)?;
        assert_eq!(explain["query_block"]["table"]["key"], "idx_feishu_option_label");
        let index_columns: Vec<String> = sqlx::query_scalar("SELECT COLUMN_NAME FROM information_schema.STATISTICS WHERE TABLE_SCHEMA=DATABASE() AND TABLE_NAME='feishu_option' AND INDEX_NAME='idx_feishu_option_label' ORDER BY SEQ_IN_INDEX").fetch_all(&pool).await?;
        assert_eq!(index_columns, ["source_key","enabled","label","parent_key"]);
        let widgets = vec![linked_widget()];
        let coordinates = BitableCoordinates { app_token:"app".into(), table_id:"table".into(), view_id:None };
        let input = DispatchInput { coordinates:&coordinates, record_id:"rec_binding", cells:&cells, applicant_field:"applicant", backfill_field_name:"审批编号", approval_code:"approval", widgets:&widgets, timezone_offset:FixedOffset::from_seconds(0).unwrap_or_else(|e| panic!("{e}")) };
        let mut static_widget = linked_widget();
        static_widget.external_binding = None;
        static_widget.option_map.insert("同名支行".into(), "legacy_id".into());
        let static_widgets = vec![static_widget];
        let legacy_input = DispatchInput { record_id:"static", widgets:&static_widgets, ..input };
        let legacy = prepare_persisted(&context, 1, &legacy_input).await.unwrap_or_else(|e| panic!("{e:?}"));
        assert!(legacy.form.contains("legacy_id"));
        let empty_optional = WidgetMap { required:false, ..static_widgets[0].clone() };
        let optional_widgets = vec![empty_optional];
        let applicant_only = json!({"applicant":[{"id":"ou_a"}]}).as_object().cloned().unwrap_or_default();
        let optional_input = DispatchInput { record_id:"optional", cells:&applicant_only, widgets:&optional_widgets, ..input };
        assert_eq!(prepare_persisted(&context, 1, &optional_input).await.unwrap_or_else(|e| panic!("{e:?}")).form, "[]");
        let (a,b) = tokio::join!(prepare_persisted(&context,1,&input),prepare_persisted(&context,1,&input));
        let a = a.unwrap_or_else(|e| panic!("{e:?}")); let b = b.unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(a.form,b.form); assert_eq!(a.open_id,b.open_id);
        assert!(a.form.contains("branch_a"));
        let task_row = row(json!({"config_id":1,"record_id":"rec_binding","uuid":derive_uuid("app","table","approval","rec_binding"),"state":"pending"}));
        insert_task(&context, task_row.clone()).await?;
        let mut wrong_owner = task_row;
        wrong_owner.insert("config_id", json!(2));
        assert!(insert_task(&context, wrong_owner).await.is_err());
        sqlx::query("CREATE TABLE binding_snapshot_writes (payload TEXT NOT NULL)").execute(&pool).await?;
        sqlx::raw_sql("CREATE TRIGGER binding_snapshot_write AFTER UPDATE ON feishu_approval_task FOR EACH ROW BEGIN IF NEW.record_id='racing' THEN INSERT INTO binding_snapshot_writes VALUES (NEW.prepared_payload); END IF; END").execute(&pool).await?;
        other_cells.insert("applicant".into(), json!([{"id":"ou_b"}]));
        let racing_a = DispatchInput { record_id:"racing", ..input };
        let racing_b = DispatchInput { cells:&other_cells, ..racing_a };
        let (left, right) = tokio::join!(prepare_persisted(&context,1,&racing_a), prepare_persisted(&context,1,&racing_b));
        let left = left.unwrap_or_else(|e| panic!("{e:?}"));
        let right = right.unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(left.form, right.form);
        assert_eq!(left.open_id, right.open_id);
        assert!(left.form.contains(if left.open_id == "ou_a" { "branch_a" } else { "branch_b" }));
        let writes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM binding_snapshot_writes").fetch_one(&pool).await?;
        assert_eq!(writes, 1, "并发快照只能写入一次，不能覆盖已有提交内容");
        sqlx::query("UPDATE feishu_option SET enabled=0").execute(&pool).await?;
        let changed_cells = json!({"applicant":[{"id":"ou_changed"}]}).as_object().cloned().unwrap_or_default();
        let changed = DispatchInput { cells:&changed_cells, ..input };
        let retry = prepare_persisted(&context,1,&changed).await.unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(retry.form,a.form); assert_eq!(retry.open_id,"ou_a");
        let transport = ScriptedTransport::new(vec![
            (200, r#"{"code":60012,"msg":"uuid conflict"}"#),
            (200, r#"{"code":0,"data":{"instance_code":"inst","serial_number":"serial","status":"PENDING"}}"#),
        ]);
        let result = dispatch_persisted(&context,1,&transport,&NoSleep,&fake_tokens(),&RecordingBackfill::default(),&changed).await;
        assert!(matches!(result, DispatchResult::Backfilled{..}));
        {
            let bodies = transport.bodies.lock().unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(bodies[0]["form"], a.form);
            assert_eq!(bodies[0]["open_id"], "ou_a");
        }
        assert!(matches!(prepare_persisted(&context,2,&changed).await, Err(DispatchResult::Terminal{..})));
        let fresh = DispatchInput { record_id:"fresh", ..input };
        assert!(matches!(prepare_persisted(&context,1,&fresh).await, Err(DispatchResult::Terminal{..})));
        let terminal_backfill = RecordingBackfill::default();
        let no_requests = ScriptedTransport::new(vec![]);
        let failed = dispatch_persisted(&context,1,&no_requests,&NoSleep,&fake_tokens(),&terminal_backfill,&fresh).await;
        assert!(matches!(failed, DispatchResult::Terminal{..}));
        assert_eq!(terminal_backfill.writes().len(), 1);
        assert!(no_requests.requests().is_empty());
        sqlx::query("UPDATE feishu_option SET enabled=1").execute(&pool).await?;
        sqlx::query("UPDATE feishu_datasource SET status='disabled'").execute(&pool).await?;
        assert!(load_bindings(&context,&form,&columns()).await.is_err());
        assert!(resolve_options(&context, &mut [linked_widget()], &cells).await.is_err());
        let empty = serde_json::Map::new();
        let missing = DispatchInput { record_id:"missing",cells:&empty,..input };
        assert!(matches!(prepare_persisted(&context,1,&missing).await,Err(DispatchResult::Waiting{..})));
        let waiting_backfill = RecordingBackfill::default();
        let waiting = dispatch_persisted(&context,1,&no_requests,&NoSleep,&fake_tokens(),&waiting_backfill,&missing).await;
        assert!(matches!(waiting, DispatchResult::Waiting{..}));
        assert!(waiting_backfill.writes().is_empty());
        assert!(no_requests.requests().is_empty());
        let incomplete = DispatchInput { record_id:"incomplete", cells:&applicant_only, ..input };
        assert!(matches!(prepare_persisted(&context,1,&incomplete).await,Err(DispatchResult::Waiting{..})));
        let invalid_widget = WidgetMap { widget_type:"unsupported".into(), ..static_widgets[0].clone() };
        let invalid_widgets = vec![invalid_widget];
        let invalid = DispatchInput { record_id:"invalid", widgets:&invalid_widgets, ..input };
        assert!(matches!(prepare_persisted(&context,1,&invalid).await,Err(DispatchResult::Terminal{..})));
        sqlx::query("UPDATE feishu_approval_task SET state='pending' WHERE record_id='rec_binding'").execute(&pool).await?;
        record_pending_outcome(&context,2,&input,&DispatchResult::Terminal{message:"wrong owner".into()}).await?;
        let unchanged: String = sqlx::query_scalar("SELECT state FROM feishu_approval_task WHERE record_id='rec_binding'").fetch_one(&pool).await?;
        assert_eq!(unchanged,"pending");
        record_pending_outcome(&context,1,&input,&DispatchResult::Backfilled{serial_number:"s".into()}).await?;
        let state: String = sqlx::query_scalar("SELECT state FROM feishu_approval_task WHERE record_id='rec_binding'").fetch_one(&pool).await?;
        assert_eq!(state,"backfilled");
        sqlx::query("UPDATE feishu_approval_task SET state='creating' WHERE record_id='rec_binding'").execute(&pool).await?;
        record_pending_outcome(&context,1,&input,&DispatchResult::Waiting{reason:"x".into()}).await?;
        let state: String = sqlx::query_scalar("SELECT state FROM feishu_approval_task WHERE record_id='rec_binding'").fetch_one(&pool).await?;
        assert_eq!(state,"creating");
        let corrupt = DispatchInput { record_id:"corrupt", ..input };
        context.approval_tasks().query().insert(row(json!({"config_id":1,"record_id":"corrupt","uuid":derive_uuid("app","table","approval","corrupt"),"prepared_payload":"{}"}))).await?;
        assert!(matches!(prepare_persisted(&context,1,&corrupt).await,Err(DispatchResult::Terminal{..})));
        let different_approval = DispatchInput { approval_code:"different", ..input };
        assert!(matches!(prepare_persisted(&context,1,&different_approval).await,Err(DispatchResult::Terminal{..})));
        pool.close().await;
        let transport = ScriptedTransport::new(vec![]);
        let result = dispatch_persisted(&context,1,&transport,&NoSleep,&fake_tokens(),&RecordingBackfill::default(),&input).await;
        assert!(matches!(result, DispatchResult::Retryable{..}));
        assert!(transport.requests().is_empty(), "快照落库失败不能调用飞书");
        Ok(())
    }.await;
        let cleanup = sqlx::MySqlPool::connect(&url).await?;
        for table in tables {
            sqlx::query(&format!("DROP TABLE IF EXISTS `{table}`"))
                .execute(&cleanup)
                .await?;
        }
        result
    }
}
