//! 同步码二维码：只在本机渲染 SVG，不经过网络、不写日志。
//!
//! 二维码内容就是完整同步码（含解密密钥），所以调用方默认应遮罩展示，
//! 用户主动点「显示」时才生成；生成结果只进当前 WebView。

use qrcode::render::svg;
use qrcode::QrCode;

use super::error::{SyncError, QR};

pub const MAX_QR_TEXT_BYTES: usize = 1024;

pub fn svg(text: &str) -> Result<String, SyncError> {
    if text.is_empty() || text.len() > MAX_QR_TEXT_BYTES {
        return Err(SyncError::new(QR, "同步码长度不适合生成二维码"));
    }
    let code = QrCode::new(text.as_bytes())
        .map_err(|e| SyncError::new(QR, format!("二维码生成失败: {e:?}")))?;
    Ok(code
        .render::<svg::Color>()
        .min_dimensions(220, 220)
        .quiet_zone(true)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_svg_without_embedding_the_code_as_text() {
        let code = "GT1.AQEAABEiM0RVZneImaq7zN3u_wABAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4fBJYDxA";
        let out = svg(code).unwrap();
        assert!(out.contains("<svg"));
        assert!(out.contains("<path"));
        assert!(!out.contains(code), "二维码图形不应把同步码当文本写进 SVG");
    }

    #[test]
    fn rejects_empty_and_oversized() {
        assert_eq!(svg("").unwrap_err().code, QR);
        assert_eq!(svg(&"x".repeat(MAX_QR_TEXT_BYTES + 1)).unwrap_err().code, QR);
    }
}
