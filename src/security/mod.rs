//! 应用级安全边界。

mod client_ip;

pub(crate) use client_ip::{
    validate_trusted_proxy_cidrs, TrustedClientIpMiddleware, CLIENT_IP_META_KEY,
};
