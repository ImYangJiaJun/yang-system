//! multipart 文件字段的「一个或一组」反序列化。
//!
//! # 为什么不能直接给字段写 `Vec<UploadedFile>`
//!
//! 传输层是按「同名 part 第几次出现」决定装成对象还是数组的
//! （`yang-base` 的 `transport/axum.rs::insert_multipart_value`）：**第一个** part 放的是
//! **裸对象** `{field_name, original_filename, content_type, size, path, temp_root}`，
//! 只有**第二个**同名 part 到来时才把它升级成 `Value::Array`。
//!
//! 而 `ParamInput` 的默认 `decode` 是 `serde_json::from_value(request.body)`，
//! `Vec<T>` 遇到 map 直接 `Err("invalid type: map, expected a sequence")`。
//! 也就是说**只传一个文件**——最普通的用法——会 400。
//!
//! 框架侧不改（那是另一个仓库，且它自带的用例覆盖的是两个 part 的形态），
//! 在应用侧把两种形态都收下来。

use serde::{Deserialize, Deserializer};
use yang_base::action::UploadedFile;

/// 同时接受**单个文件句柄对象**与**文件句柄数组**，都归一成 `Vec<UploadedFile>`。
///
/// 用法（`schemars` 侧仍是 `Vec`，所以构建期的「multipart 必须声明二进制字段」校验不受影响）：
///
/// ```ignore
/// #[derive(Debug, Deserialize, JsonSchema)]
/// #[serde(deny_unknown_fields)]
/// pub(super) struct ImportInput {
///     #[serde(deserialize_with = "one_or_many_files")]
///     pub(super) files: Vec<UploadedFile>,
/// }
/// ```
pub(crate) fn one_or_many_files<'de, D>(deserializer: D) -> Result<Vec<UploadedFile>, D::Error>
where
    D: Deserializer<'de>,
{
    /// 两种形态的判别。`Many` 放前面：多文件时省一次注定失败的尝试。
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        Many(Vec<UploadedFile>),
        One(UploadedFile),
    }

    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::Many(files) => files,
        OneOrMany::One(file) => vec![file],
    })
}
