//! `page_token` 游标编解码。
//!
//! 飞书的分页是游标语义，而 `TableQuery` 是 `page/page_size` 语义。这里把
//! `(sort_order, option_id)` 这个稳定排序键编成不透明游标，翻页时用 **keyset**
//! 条件（排序键严格大于游标）而不是 offset——offset 在数据变动时会漏行或重复，
//! 而审批人翻页时数据被多维表格改掉是常态。

use base64::Engine;
use yang_base::BaseError;

/// 游标内部的分隔符。
///
/// 用 ASCII 单元分隔符（US, 0x1F）而不是常见的 `:` 或 `|`：选项 id 是外部传入的
/// 业务主键，可能含任何可打印字符。US 在 JSON 字符串里会被转义，实测中极不可能出现。
const SEPARATOR: char = '\u{1f}';

/// base64 URL-safe 无填充编码器。
const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// 编码游标：`base64(sort_order ‖ US ‖ option_id)`。
pub(crate) fn encode_cursor(sort_order: i64, option_id: &str) -> String {
    let raw = format!("{sort_order}{SEPARATOR}{option_id}");
    BASE64.encode(raw.as_bytes())
}

/// 解码游标；空串或全空白表示请求第一页。
///
/// # 错误
///
/// 非法 base64、非 UTF-8、缺分隔符、或排序键不是整数时返回
/// [`BaseError::ParamInvalid`]（fail-closed）。静默当作第一页会让翻页重复吐数据，
/// 在「多选」控件上表现为选项重复，比报错更难排查。
pub(crate) fn decode_cursor(raw: &str) -> Result<Option<(i64, String)>, BaseError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let invalid = || BaseError::ParamInvalid("page_token".to_string(), "分页标记非法".to_string());

    let bytes = BASE64.decode(trimmed).map_err(|_| invalid())?;
    let decoded = String::from_utf8(bytes).map_err(|_| invalid())?;
    let (order, id) = decoded.split_once(SEPARATOR).ok_or_else(invalid)?;
    let sort_order = order.parse::<i64>().map_err(|_| invalid())?;
    Ok(Some((sort_order, id.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips() {
        let encoded = encode_cursor(42, "dept_sales");
        assert_eq!(
            decode_cursor(&encoded).unwrap_or_else(|error| panic!("应可解码: {error}")),
            Some((42, "dept_sales".to_string()))
        );
    }

    #[test]
    fn negative_and_zero_sort_order_round_trip() {
        for sort_order in [-7, 0, i64::MAX] {
            let encoded = encode_cursor(sort_order, "x");
            assert_eq!(
                decode_cursor(&encoded).unwrap_or_else(|error| panic!("应可解码: {error}")),
                Some((sort_order, "x".to_string())),
                "sort_order={sort_order} 必须原样往返"
            );
        }
    }

    #[test]
    fn option_id_containing_separator_like_characters_round_trips() {
        // 选项 id 里出现冒号、空格、中文都不该破坏游标
        for option_id in ["a:b c", "部门_销售", "a/b?c=d", "\u{1e}weird"] {
            let encoded = encode_cursor(1, option_id);
            assert_eq!(
                decode_cursor(&encoded).unwrap_or_else(|error| panic!("应可解码: {error}")),
                Some((1, option_id.to_string())),
                "option_id={option_id:?} 必须原样往返"
            );
        }
    }

    #[test]
    fn empty_cursor_means_first_page() {
        assert_eq!(
            decode_cursor("").unwrap_or_else(|error| panic!("空游标合法: {error}")),
            None
        );
        assert_eq!(
            decode_cursor("   ").unwrap_or_else(|error| panic!("空白游标合法: {error}")),
            None
        );
    }

    #[test]
    fn malformed_cursor_is_rejected_not_ignored() {
        // fail-closed：非法游标必须报错。若静默从头返回，翻页会重复吐第一页，
        // 在「多选」控件上表现为选项重复，比报错更难排查
        assert!(decode_cursor("not-base64!!").is_err());
        assert!(
            decode_cursor("bm90LWEtY3Vyc29y").is_err(),
            "base64 合法但结构不对也要拒绝"
        );
        // 能解码、但是 UTF-8 里没有分隔符
        assert!(
            decode_cursor(&super::BASE64.encode(b"12345")).is_err(),
            "缺分隔符要拒绝"
        );
        // 有分隔符但排序键不是整数
        assert!(
            decode_cursor(&super::BASE64.encode("abc\u{1f}x".as_bytes())).is_err(),
            "排序键不是整数要拒绝"
        );
    }

    #[test]
    fn cursor_does_not_leak_the_raw_key() {
        // 游标对调用方是不透明的：不应能直接读出业务字段
        let encoded = encode_cursor(7, "dept_sales");
        assert!(
            !encoded.contains("dept_sales"),
            "游标不得明文包含选项 id: {encoded}"
        );
    }
}
