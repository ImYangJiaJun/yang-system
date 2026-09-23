//! 投影契约（`frontend/contracts/feishu-projections.json`）的对账工具。
//!
//! # 这份契约解决什么
//!
//! 本仓出过四次同一类 bug，全部逃过 553+ 单测 + clippy + 架构门禁：**字段没被删，
//! 只是搬到了另一层**（或前端读了一个后端从不发的键）。那种漂移对「扫已删字段」
//! 类检查**全盲**——`encrypt_enabled` 在绑定表上依然是合法字段名。
//!
//! 所以把「后端 emit 的键集」与「前端 read 的键集」各写一份、摆进**同一个文件**，
//! 两端各读一次并各自断言：谁漂移谁红。
//!
//! # 三个去处，互斥且穷尽
//!
//! - `emitted`：真的发出去。序列化键集必须**恰好**等于它（`assert_keys`）；
//! - `query_only`：查询要用、但不发出去（例如分组键 `datasource_id`）；
//! - `backend_only`：发了但前端不读，**必须写明理由**——是决定，不是垃圾桶。
//!
//! # 只覆盖结构体化的响应
//!
//! 断言靠 serde 推导出的键集，所以只对**结构体**响应有效。内联 `json!({...})`
//! 的响应（翻页信封、创建/更新回执）断言不到——那类要先把响应对成结构体，
//! 才能纳入这份契约。
#![cfg(test)]

use serde::Serialize;
use yang_base::definition::TableSpec;

/// 契约文件本身。`include_str!` 在**编译期**读入：文件一变，用到它的测试就重编。
pub(crate) fn contract() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../frontend/contracts/feishu-projections.json"
    ))
    .unwrap_or_else(|error| panic!("契约文件应是合法 JSON: {error}"))
}

/// 按路径取一个字符串数组（例如 `["list_datasources", "binding", "emitted"]`）。
pub(crate) fn string_array(contract: &serde_json::Value, path: &[&str]) -> Vec<String> {
    let mut node = contract;
    for step in path {
        node = node
            .get(step)
            .unwrap_or_else(|| panic!("契约里缺 {}", path.join(".")));
    }
    node.as_array()
        .unwrap_or_else(|| panic!("{} 应是数组", path.join(".")))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("{} 的元素应是字符串", path.join(".")))
                .to_string()
        })
        .collect()
}

/// 按路径取一个对象（`backend_only` / `query_only`）的键集。
pub(crate) fn bucket_keys(contract: &serde_json::Value, path: &[&str]) -> Vec<String> {
    let mut node = contract;
    for step in path {
        node = node
            .get(step)
            .unwrap_or_else(|| panic!("契约里缺 {}", path.join(".")));
    }
    node.as_object()
        .unwrap_or_else(|| panic!("{} 应是对象", path.join(".")))
        .keys()
        .cloned()
        .collect()
}

/// `value` 序列化出来的键集必须**恰好**等于契约声明的 `emitted`。
///
/// 这是「后端一侧」的断言：结构体加了字段而契约没改（或反过来），当场红。
pub(crate) fn assert_keys<T: Serialize>(value: &T, path: &[&str], what: &str) {
    let serialized =
        serde_json::to_value(value).unwrap_or_else(|error| panic!("{what} 应可序列化: {error}"));
    let mut actual: Vec<String> = serialized
        .as_object()
        .unwrap_or_else(|| panic!("{what} 应序列化成对象"))
        .keys()
        .cloned()
        .collect();
    actual.sort_unstable();

    let mut declared = string_array(&contract(), path);
    declared.sort_unstable();

    assert_eq!(
        actual,
        declared,
        "{what} 序列化出来的键与契约 {} 不一致——两者必须一起改",
        path.join(".")
    );
}

/// `backend_only` 只能记**真的 emit 了**的键；`query_only` 只能是**没有 emit**的键。
///
/// 这两条把「三个去处」钉成互斥且穷尽：写错一格就是用一个注释掩盖一次漂移。
pub(crate) fn assert_buckets_are_disjoint(
    contract: &serde_json::Value,
    endpoint: &str,
    level: &str,
) {
    let base = [endpoint, level];
    let mut emitted_path = base.to_vec();
    emitted_path.push("emitted");
    let emitted = string_array(contract, &emitted_path);

    let mut backend_only_path = base.to_vec();
    backend_only_path.push("backend_only");
    for key in bucket_keys(contract, &backend_only_path) {
        assert!(
            emitted.contains(&key),
            "{endpoint}.{level}.backend_only 里的 {key} 并不在 emitted 里——\
             那不是「发了但前端不读」，是写错了"
        );
    }

    let mut query_only_path = base.to_vec();
    query_only_path.push("query_only");
    for key in bucket_keys(contract, &query_only_path) {
        assert!(
            !emitted.contains(&key),
            "{endpoint}.{level}.query_only 里的 {key} 同时也在 emitted 里——\
             query_only 的语义是「不发出去」"
        );
    }
}

/// **轴一：客户端字段名。** 前端**发出**的字段名必须能在它那张表上用。
///
/// 为什么需要它：前端的排序/筛选键是**契约的一部分**，而它们曾经漂移过一次
/// （列表页的收尾键从 `source_key` 换成 `id`，而 `id` 当时没开 `sortable`——
/// 结果整个列表请求 400）。那一类漂移从后端一侧看就是「一个列名 + 一个能力位」，
/// 而这两样在这里是**离线可判**的。
///
/// 与轴二的区别：轴二管**值**（取值域），这一条管**名字**（列与能力位）。
pub(crate) fn assert_client_fields_are_usable(
    contract: &serde_json::Value,
    endpoint: &str,
    spec: &TableSpec,
    what: &str,
) {
    let section = contract
        .get("client_fields")
        .and_then(|node| node.get(endpoint))
        .unwrap_or_else(|| panic!("契约里缺 client_fields.{endpoint}"));

    for (verb, bit) in [("order_by", "sortable"), ("where", "filterable")] {
        let names = section
            .get(verb)
            .and_then(|node| node.as_array())
            .unwrap_or_else(|| panic!("契约里缺 client_fields.{endpoint}.{verb}"));
        for name in names {
            let name = name
                .as_str()
                .unwrap_or_else(|| panic!("{endpoint}.{verb} 的元素应是字符串"));
            let field = spec
                .fields
                .iter()
                .find(|field| field.name.as_str() == name)
                .unwrap_or_else(|| {
                    panic!(
                        "{what}: 前端在 {verb} 里发 `{name}`，而这张表上没有这一列                          ⇒ 运行期 FieldNotFound(400)，整个请求被打死"
                    )
                });
            let ok = match bit {
                "sortable" => field.access.sortable,
                "filterable" => field.access.filterable,
                _ => panic!("未知能力位 {bit}"),
            };
            assert!(
                ok,
                "{what}: 前端在 {verb} 里发 `{name}`，但这一列没声明 `{bit}`                  ⇒ 运行期 FieldPermissionDenied(403)（DSL 是 fail-closed）"
            );
        }
    }
}

/// **轴二：枚举取值域。** 契约里 `enforced_by: backend` 的取值域必须与表声明逐字一致。
///
/// 逐个比对而不是「包含」：表声明是唯一事实源，契约只是它的镜像；镜像多一个值、
/// 少一个值、顺序不同，都说明有人在两边各改了一半。
pub(crate) fn assert_enum_domains_match(
    contract: &serde_json::Value,
    table: &str,
    spec: &TableSpec,
) {
    let Some(section) = contract.get("enums").and_then(|node| node.get(table)) else {
        return;
    };
    for (field_name, entry) in section
        .as_object()
        .unwrap_or_else(|| panic!("enums.{table} 应是对象"))
    {
        if entry.get("enforced_by").and_then(|value| value.as_str()) != Some("backend") {
            // 后端零校验的那一列（例如 `default_locale`）：没有声明可对，跳过。
            continue;
        }
        let declared: Vec<String> = entry
            .get("values")
            .and_then(|value| value.as_array())
            .unwrap_or_else(|| panic!("enums.{table}.{field_name}.values 应是数组"))
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .unwrap_or_else(|| panic!("取值应是字符串"))
                    .to_string()
            })
            .collect();
        let field = spec
            .fields
            .iter()
            .find(|field| field.name.as_str() == field_name)
            .unwrap_or_else(|| panic!("{table} 上没有 `{field_name}` 这一列"));
        let actual: Vec<String> = field
            .options
            .iter()
            .map(|(value, _)| value.clone())
            .collect();
        assert_eq!(
            actual, declared,
            "{table}.{field_name} 的取值域与契约不一致——表声明是事实源，契约要跟着它改"
        );
    }
}
