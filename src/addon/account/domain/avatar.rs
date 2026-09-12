//! 用户头像持久化仓储与图片校验：`user_avatar` 表的 upsert/读取/事务内删除。
//!
//! 头像以 base64 文本落库（一人一行的运行支撑表，不进 UI Catalog），
//! `etag` 是内容 sha256 十六进制前 32 字符，供前端按 `avatar_version` 做缓存
//! 失效。图片校验是纯函数（MIME 白名单 + magic bytes 一致性 + 头部分析宽高），
//! 不触碰 I/O，便于单元测试。
//!
//! 该仓储不触碰 `users` 授权事实，不属于 authorization-writer allowlist。

use std::sync::Arc;
use yang_base::action::ActionContext;
use yang_base::table::{Record, TableDefinition, TableQuery};
use yang_base::BaseError;
use yang_db::Transaction;

/// 解码后图片字节上限：base64 编码后约 54 KB，低于 TEXT 的 64 KB 上限。
pub(crate) const AVATAR_MAX_BYTES: usize = 40 * 1024;
/// 头像宽高上限（像素）。
pub(crate) const AVATAR_MAX_DIMENSION: u32 = 1024;

const USER_ID: &str = "user_id";
const MIME: &str = "mime";
const CONTENT_BASE64: &str = "content_base64";
const ETAG: &str = "etag";
const UPDATED_AT: &str = "updated_at";

/// 一条头像记录（读取投影）。
pub(crate) struct AvatarRecord {
    pub(crate) mime: String,
    pub(crate) content_base64: String,
    pub(crate) etag: String,
}

pub(crate) struct AvatarRepository {
    avatars: TableDefinition,
}

impl AvatarRepository {
    pub(crate) fn new(avatars: TableDefinition) -> Self {
        Self { avatars }
    }

    fn query(&self, ctx: &ActionContext) -> Result<TableQuery, BaseError> {
        let pool = Arc::new(ctx.tools().mysql()?.pool().clone());
        Ok(self.avatars.bind(pool).query(["system"]))
    }

    /// 写入或覆盖用户头像（主键即用户 ID，一人一张）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn upsert(
        &self,
        ctx: &ActionContext,
        user_id: i64,
        mime: &str,
        content_base64: &str,
        etag: &str,
        now: i64,
    ) -> Result<(), BaseError> {
        let existing = self
            .query(ctx)?
            .select_fields(&[USER_ID])?
            .where_eq(USER_ID, serde_json::Value::Number(user_id.into()))?
            .page(1, 1)?
            .all()
            .await?;
        if existing.is_empty() {
            let record = Record::new()
                .set(USER_ID, user_id)
                .set(MIME, mime)
                .set(CONTENT_BASE64, content_base64)
                .set(ETAG, etag)
                .set(UPDATED_AT, now);
            self.query(ctx)?.insert(record).await?;
        } else {
            let update = Record::new()
                .set(MIME, mime)
                .set(CONTENT_BASE64, content_base64)
                .set(ETAG, etag)
                .set(UPDATED_AT, now);
            self.query(ctx)?
                .where_eq(USER_ID, serde_json::Value::Number(user_id.into()))?
                .update(update)
                .await?;
        }
        Ok(())
    }

    /// 读取用户头像完整记录（无头像返回 None）。
    pub(crate) async fn find_by_user(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<Option<AvatarRecord>, BaseError> {
        let rows = self
            .query(ctx)?
            .select_fields(&[MIME, CONTENT_BASE64, ETAG])?
            .where_eq(USER_ID, serde_json::Value::Number(user_id.into()))?
            .page(1, 1)?
            .all()
            .await?;
        rows.first()
            .map(|record| {
                Ok(AvatarRecord {
                    mime: record.require(MIME)?,
                    content_base64: record.require(CONTENT_BASE64)?,
                    etag: record.require(ETAG)?,
                })
            })
            .transpose()
    }

    /// 只读取头像 etag（me/register 视图投影用，避免搬运 base64 全文）。
    pub(crate) async fn etag_for(
        &self,
        ctx: &ActionContext,
        user_id: i64,
    ) -> Result<Option<String>, BaseError> {
        let rows = self
            .query(ctx)?
            .select_fields(&[ETAG])?
            .where_eq(USER_ID, serde_json::Value::Number(user_id.into()))?
            .page(1, 1)?
            .all()
            .await?;
        rows.first().map(|record| record.require(ETAG)).transpose()
    }

    /// 在调用方事务内删除用户头像行（注销匿名化的隐私清理，与匿名化原子提交）。
    pub(crate) async fn delete_in_tx(
        &self,
        ctx: &ActionContext,
        transaction: &mut Transaction,
        user_id: i64,
    ) -> Result<u64, BaseError> {
        let affected = self
            .query(ctx)?
            .where_eq(USER_ID, serde_json::Value::Number(user_id.into()))?
            .delete_in_tx(transaction)
            .await?;
        Ok(affected)
    }
}

/// 校验头像图片：MIME 白名单、magic bytes 与声明一致、从头部分析出的宽高
/// 都在 1..=1024 像素内。返回 `(宽, 高)`。
pub(crate) fn validate_avatar_image(mime: &str, bytes: &[u8]) -> Result<(u32, u32), BaseError> {
    if bytes.is_empty() || bytes.len() > AVATAR_MAX_BYTES {
        return Err(BaseError::ParamInvalid(
            "content_base64".to_string(),
            format!("头像大小必须在 1..={AVATAR_MAX_BYTES} 字节内"),
        ));
    }
    let (width, height) = match mime {
        "image/png" => parse_png_dimensions(bytes)?,
        "image/jpeg" => parse_jpeg_dimensions(bytes)?,
        "image/webp" => parse_webp_dimensions(bytes)?,
        "image/gif" => parse_gif_dimensions(bytes)?,
        _ => {
            return Err(BaseError::ParamInvalid(
                "mime".to_string(),
                "头像格式只支持 image/png、image/jpeg、image/webp、image/gif".to_string(),
            ));
        }
    };
    if width == 0 || height == 0 || width > AVATAR_MAX_DIMENSION || height > AVATAR_MAX_DIMENSION {
        return Err(BaseError::ParamInvalid(
            "content_base64".to_string(),
            format!("头像宽高必须在 1..={AVATAR_MAX_DIMENSION} 像素内"),
        ));
    }
    Ok((width, height))
}

/// 文件内容与声明 MIME 不一致（或文件头损坏）的统一错误。
fn image_mismatch() -> BaseError {
    BaseError::ParamInvalid(
        "content_base64".to_string(),
        "头像文件内容与声明的图片格式不一致".to_string(),
    )
}

/// PNG：8 字节 magic + IHDR 段，宽高在文件头 offset 16（大端 u32）。
fn parse_png_dimensions(bytes: &[u8]) -> Result<(u32, u32), BaseError> {
    const MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 24 || bytes[..8] != MAGIC || &bytes[12..16] != b"IHDR" {
        return Err(image_mismatch());
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Ok((width, height))
}

/// GIF：magic 为 "GIF87a"/"GIF89a"，宽高在 offset 6（小端 u16）。
fn parse_gif_dimensions(bytes: &[u8]) -> Result<(u32, u32), BaseError> {
    if bytes.len() < 10 || (&bytes[..6] != b"GIF87a" && &bytes[..6] != b"GIF89a") {
        return Err(image_mismatch());
    }
    let width = u32::from(u16::from_le_bytes([bytes[6], bytes[7]]));
    let height = u32::from(u16::from_le_bytes([bytes[8], bytes[9]]));
    Ok((width, height))
}

/// JPEG：FF D8 起始后顺序扫描 marker 段，在 SOF0-SOF15（不含 DHT/DAC/JPG 扩展）
/// 段内取宽高（大端 u16）。
fn parse_jpeg_dimensions(bytes: &[u8]) -> Result<(u32, u32), BaseError> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 || bytes[2] != 0xFF {
        return Err(image_mismatch());
    }
    let mut offset = 2;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xFF {
            offset += 1;
            continue;
        }
        let marker = bytes[offset + 1];
        // 无长度段：SOI/EOI/RSTn/TEM。
        if marker == 0xD8 || marker == 0xD9 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            offset += 2;
            continue;
        }
        let length = usize::from(u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]));
        if length < 2 {
            break;
        }
        let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            if offset + 9 > bytes.len() {
                break;
            }
            let height = u32::from(u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]));
            let width = u32::from(u16::from_be_bytes([bytes[offset + 7], bytes[offset + 8]]));
            return Ok((width, height));
        }
        offset += 2 + length;
    }
    Err(image_mismatch())
}

/// WebP：RIFF....WEBP 容器，按 VP8（lossy）/VP8L（lossless）/VP8X（扩展）
/// 三种 chunk 布局分别解析宽高。
fn parse_webp_dimensions(bytes: &[u8]) -> Result<(u32, u32), BaseError> {
    if bytes.len() < 16 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return Err(image_mismatch());
    }
    match &bytes[12..16] {
        b"VP8 " => {
            // frame tag（3 字节）+ start code 9D 01 2A，宽高为 14 bit 小端。
            if bytes.len() < 30 || bytes[23..26] != [0x9D, 0x01, 0x2A] {
                return Err(image_mismatch());
            }
            let width = u32::from(u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3FFF);
            let height = u32::from(u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3FFF);
            Ok((width, height))
        }
        b"VP8L" => {
            // 签名字节 0x2F 后 4 字节按位打包宽（14 bit）高（14 bit），均存「值-1」。
            if bytes.len() < 25 || bytes[20] != 0x2F {
                return Err(image_mismatch());
            }
            let b1 = u32::from(bytes[21]);
            let b2 = u32::from(bytes[22]);
            let b3 = u32::from(bytes[23]);
            let b4 = u32::from(bytes[24]);
            let width = (b1 | ((b2 & 0x3F) << 8)) + 1;
            let height = ((b2 >> 6) | (b3 << 2) | ((b4 & 0x0F) << 10)) + 1;
            Ok((width, height))
        }
        b"VP8X" => {
            // flags（1 字节）+ 保留（3 字节）后，画布宽高各 3 字节小端，存「值-1」。
            if bytes.len() < 30 {
                return Err(image_mismatch());
            }
            let width =
                u32::from(bytes[24]) | (u32::from(bytes[25]) << 8) | (u32::from(bytes[26]) << 16);
            let height =
                u32::from(bytes[27]) | (u32::from(bytes[28]) << 8) | (u32::from(bytes[29]) << 16);
            Ok((width + 1, height + 1))
        }
        _ => Err(image_mismatch()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_with_dimensions(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend_from_slice(&13_u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    }

    fn gif_with_dimensions(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes
    }

    fn jpeg_with_dimensions(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
        bytes
    }

    fn webp_vp8x_with_dimensions(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(b"WEBPVP8X");
        bytes.extend_from_slice(&10_u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        let stored_width = width - 1;
        let stored_height = height - 1;
        bytes.extend_from_slice(&[
            stored_width as u8,
            (stored_width >> 8) as u8,
            (stored_width >> 16) as u8,
        ]);
        bytes.extend_from_slice(&[
            stored_height as u8,
            (stored_height >> 8) as u8,
            (stored_height >> 16) as u8,
        ]);
        bytes
    }

    fn webp_vp8l_with_dimensions(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(b"WEBPVP8L");
        bytes.extend_from_slice(&5_u32.to_le_bytes());
        bytes.push(0x2F);
        let stored_width = width - 1;
        let stored_height = height - 1;
        bytes.extend_from_slice(&[
            (stored_width & 0xFF) as u8,
            (((stored_width >> 8) & 0x3F) | ((stored_height & 0x03) << 6)) as u8,
            ((stored_height >> 2) & 0xFF) as u8,
            ((stored_height >> 10) & 0x0F) as u8,
        ]);
        bytes
    }

    fn webp_vp8_with_dimensions(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(b"WEBPVP8 ");
        bytes.extend_from_slice(&10_u32.to_le_bytes());
        bytes.extend_from_slice(&[0x00, 0x00, 0x00]);
        bytes.extend_from_slice(&[0x9D, 0x01, 0x2A]);
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes
    }

    #[test]
    fn accepts_each_supported_format_with_valid_header() {
        let cases: Vec<(&str, Vec<u8>, (u32, u32))> = vec![
            ("image/png", png_with_dimensions(640, 480), (640, 480)),
            ("image/gif", gif_with_dimensions(32, 32), (32, 32)),
            ("image/jpeg", jpeg_with_dimensions(1024, 1024), (1024, 1024)),
            (
                "image/webp",
                webp_vp8x_with_dimensions(300, 200),
                (300, 200),
            ),
            ("image/webp", webp_vp8l_with_dimensions(17, 19), (17, 19)),
            ("image/webp", webp_vp8_with_dimensions(800, 600), (800, 600)),
        ];
        for (mime, bytes, expected) in cases {
            let dimensions = validate_avatar_image(mime, &bytes)
                .unwrap_or_else(|error| panic!("{mime} 合法样本应通过校验: {error}"));
            assert_eq!(dimensions, expected, "{mime} 解析出的宽高不符");
        }
        // GIF87a 同样合法。
        let mut gif87 = gif_with_dimensions(1, 1);
        gif87[4] = b'7';
        assert!(validate_avatar_image("image/gif", &gif87).is_ok());
    }

    #[test]
    fn rejects_mime_outside_whitelist() {
        for mime in ["image/svg+xml", "image/bmp", "text/plain", ""] {
            assert!(
                validate_avatar_image(mime, &png_with_dimensions(1, 1)).is_err(),
                "{mime:?} 必须被拒绝"
            );
        }
    }

    #[test]
    fn rejects_magic_bytes_inconsistent_with_declared_mime() {
        assert!(validate_avatar_image("image/jpeg", &png_with_dimensions(1, 1)).is_err());
        assert!(validate_avatar_image("image/png", &jpeg_with_dimensions(1, 1)).is_err());
        assert!(validate_avatar_image("image/webp", &gif_with_dimensions(1, 1)).is_err());
        assert!(validate_avatar_image("image/gif", &webp_vp8x_with_dimensions(1, 1)).is_err());
        // 截断的文件头同样视为内容不一致。
        assert!(validate_avatar_image("image/png", &png_with_dimensions(1, 1)[..12]).is_err());
    }

    #[test]
    fn rejects_oversized_or_empty_payload() {
        assert!(validate_avatar_image("image/png", &[]).is_err());
        let oversized = vec![0_u8; AVATAR_MAX_BYTES + 1];
        assert!(validate_avatar_image("image/png", &oversized).is_err());
    }

    #[test]
    fn rejects_dimensions_outside_bounds() {
        assert!(validate_avatar_image("image/png", &png_with_dimensions(0, 100)).is_err());
        assert!(validate_avatar_image("image/png", &png_with_dimensions(100, 0)).is_err());
        assert!(validate_avatar_image("image/png", &png_with_dimensions(1025, 100)).is_err());
        assert!(validate_avatar_image("image/gif", &gif_with_dimensions(1, 1025)).is_err());
        assert!(validate_avatar_image("image/jpeg", &jpeg_with_dimensions(2000, 10)).is_err());
        // 边界值 1024 合法。
        assert!(validate_avatar_image("image/png", &png_with_dimensions(1024, 1024)).is_ok());
    }

    #[test]
    fn jpeg_scanner_skips_non_sof_segments() {
        // 无 SOF 段的 JPEG 必须拒绝。
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        bytes.extend_from_slice(&[0xFF, 0xD9]);
        assert!(validate_avatar_image("image/jpeg", &bytes).is_err());
    }
}
