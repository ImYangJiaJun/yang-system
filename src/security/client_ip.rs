//! 受信代理后的客户端 IP 解析。
//!
//! 外部转发头始终是不可信输入。只有 TCP 对端位于显式配置的代理网段时，才从右向左
//! 剥离可信代理；遇到第一个不受信地址即停止，从而忽略客户端伪造的更左侧前缀。

use async_trait::async_trait;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use yang_base::action::{ActionContext, ApiResponse};
use yang_base::router::{Middleware, Next};
use yang_base::BaseError;

pub(crate) const CLIENT_IP_META_KEY: &str = "yang.client_ip";

const MAX_TRUSTED_PROXY_CIDRS: usize = 64;
const MAX_FORWARDED_HEADER_BYTES: usize = 4_096;
const MAX_FORWARDED_HOPS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolutionSource {
    DirectPeer,
    Forwarded,
    XForwardedFor,
    InvalidForwardingHeader,
    MissingPeer,
}

impl ResolutionSource {
    const fn metric_label(self) -> &'static str {
        match self {
            Self::DirectPeer => "direct",
            Self::Forwarded => "forwarded",
            Self::XForwardedFor => "x_forwarded_for",
            Self::InvalidForwardingHeader => "invalid_header",
            Self::MissingPeer => "missing_peer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClientIpResolution {
    ip: Option<IpAddr>,
    source: ResolutionSource,
}

impl ClientIpResolution {
    fn identity(self) -> String {
        self.ip
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IpCidr {
    V4 { network: u32, mask: u32 },
    V6 { network: u128, mask: u128 },
}

impl IpCidr {
    fn parse(raw: &str) -> Result<Self, String> {
        let value = raw.trim();
        let (address, prefix) = value
            .split_once('/')
            .ok_or_else(|| "必须使用 address/prefix CIDR 形式".to_string())?;
        if prefix.contains('/') {
            return Err("CIDR 只能包含一个斜杠".to_string());
        }
        let address = address
            .parse::<IpAddr>()
            .map_err(|_| "网络地址无效".to_string())?;
        let prefix = prefix
            .parse::<u8>()
            .map_err(|_| "前缀长度无效".to_string())?;

        match normalize_ip(address) {
            IpAddr::V4(address) => {
                if prefix == 0 || prefix > 32 {
                    return Err("IPv4 前缀长度必须在 1..=32；禁止信任全部地址".to_string());
                }
                let mask = u32::MAX << (32 - prefix);
                Ok(Self::V4 {
                    network: u32::from(address) & mask,
                    mask,
                })
            }
            IpAddr::V6(address) => {
                if prefix == 0 || prefix > 128 {
                    return Err("IPv6 前缀长度必须在 1..=128；禁止信任全部地址".to_string());
                }
                let mask = u128::MAX << (128 - prefix);
                Ok(Self::V6 {
                    network: u128::from(address) & mask,
                    mask,
                })
            }
        }
    }

    fn contains(self, address: IpAddr) -> bool {
        match (self, normalize_ip(address)) {
            (Self::V4 { network, mask }, IpAddr::V4(address)) => {
                u32::from(address) & mask == network
            }
            (Self::V6 { network, mask }, IpAddr::V6(address)) => {
                u128::from(address) & mask == network
            }
            _ => false,
        }
    }
}

#[derive(Clone)]
struct TrustedClientIpResolver {
    trusted_proxies: Vec<IpCidr>,
}

impl TrustedClientIpResolver {
    fn from_cidrs(values: &[String]) -> Result<Self, BaseError> {
        if values.len() > MAX_TRUSTED_PROXY_CIDRS {
            return Err(BaseError::ConfigError(format!(
                "security.trusted_proxy_cidrs 最多允许 {MAX_TRUSTED_PROXY_CIDRS} 项"
            )));
        }
        let trusted_proxies = values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                IpCidr::parse(value).map_err(|error| {
                    BaseError::ConfigError(format!(
                        "security.trusted_proxy_cidrs[{index}] 无效: {error}"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { trusted_proxies })
    }

    fn resolve(
        &self,
        peer: Option<SocketAddr>,
        forwarded: Option<&str>,
        x_forwarded_for: Option<&str>,
    ) -> ClientIpResolution {
        let Some(peer_ip) = peer.map(|address| normalize_ip(address.ip())) else {
            return ClientIpResolution {
                ip: None,
                source: ResolutionSource::MissingPeer,
            };
        };

        if !self.is_trusted(peer_ip) {
            return ClientIpResolution {
                ip: Some(peer_ip),
                source: ResolutionSource::DirectPeer,
            };
        }

        let parsed = match (forwarded, x_forwarded_for) {
            (Some(raw), _) => parse_forwarded(raw).map(|hops| (hops, ResolutionSource::Forwarded)),
            (None, Some(raw)) => {
                parse_x_forwarded_for(raw).map(|hops| (hops, ResolutionSource::XForwardedFor))
            }
            (None, None) => {
                return ClientIpResolution {
                    ip: Some(peer_ip),
                    source: ResolutionSource::DirectPeer,
                };
            }
        };

        let Ok((hops, source)) = parsed else {
            return ClientIpResolution {
                ip: Some(peer_ip),
                source: ResolutionSource::InvalidForwardingHeader,
            };
        };
        let mut client_ip = peer_ip;
        for candidate in hops.iter().rev() {
            if !self.is_trusted(client_ip) {
                break;
            }
            client_ip = normalize_ip(*candidate);
        }
        ClientIpResolution {
            ip: Some(client_ip),
            source,
        }
    }

    fn is_trusted(&self, address: IpAddr) -> bool {
        self.trusted_proxies
            .iter()
            .any(|network| network.contains(address))
    }
}

/// 校验受信代理配置；空列表表示完全忽略外部转发头。
pub(crate) fn validate_trusted_proxy_cidrs(values: &[String]) -> Result<(), BaseError> {
    TrustedClientIpResolver::from_cidrs(values).map(|_| ())
}

/// 把解析后的客户端 IP 写入只有受信代码可构造的请求传输扩展。
#[derive(Clone)]
pub(crate) struct TrustedClientIpMiddleware {
    resolver: TrustedClientIpResolver,
}

impl TrustedClientIpMiddleware {
    pub(crate) fn from_cidrs(values: &[String]) -> Result<Self, BaseError> {
        Ok(Self {
            resolver: TrustedClientIpResolver::from_cidrs(values)?,
        })
    }
}

#[async_trait]
impl Middleware for TrustedClientIpMiddleware {
    async fn handle(
        &self,
        mut ctx: ActionContext,
        next: Next<'_>,
    ) -> Result<ApiResponse, BaseError> {
        let resolution = self.resolver.resolve(
            ctx.request_meta.peer_addr,
            ctx.request.get_header("forwarded"),
            ctx.request.get_header("x-forwarded-for"),
        );
        metrics::counter!(
            "client_ip_resolution_total",
            "source" => resolution.source.metric_label()
        )
        .increment(1);
        if resolution.source == ResolutionSource::InvalidForwardingHeader {
            tracing::warn!(
                request_id = %ctx.request_id(),
                "受信代理转发的客户端 IP 头无效，已安全退回 TCP 对端地址"
            );
        } else if resolution.source == ResolutionSource::MissingPeer {
            tracing::warn!(
                request_id = %ctx.request_id(),
                "请求缺少 TCP 对端地址，认证限流将使用共享 unknown 分组"
            );
        }
        ctx.request_meta
            .extensions
            .insert(CLIENT_IP_META_KEY.to_string(), resolution.identity());
        next.run(ctx).await
    }
}

fn parse_forwarded(raw: &str) -> Result<Vec<IpAddr>, ()> {
    validate_forwarding_header(raw)?;
    let mut hops = Vec::new();
    for element in raw.split(',') {
        if hops.len() >= MAX_FORWARDED_HOPS {
            return Err(());
        }
        let mut forwarded_for = None;
        for parameter in element.split(';') {
            let (name, value) = parameter.split_once('=').ok_or(())?;
            if name.trim().eq_ignore_ascii_case("for") {
                if forwarded_for.is_some() {
                    return Err(());
                }
                forwarded_for = Some(parse_forwarded_node(value)?);
            }
        }
        hops.push(forwarded_for.ok_or(())?);
    }
    if hops.is_empty() {
        return Err(());
    }
    Ok(hops)
}

fn parse_x_forwarded_for(raw: &str) -> Result<Vec<IpAddr>, ()> {
    validate_forwarding_header(raw)?;
    let mut hops = Vec::new();
    for value in raw.split(',') {
        if hops.len() >= MAX_FORWARDED_HOPS {
            return Err(());
        }
        hops.push(parse_forwarded_node(value)?);
    }
    if hops.is_empty() {
        return Err(());
    }
    Ok(hops)
}

fn validate_forwarding_header(raw: &str) -> Result<(), ()> {
    if raw.is_empty() || raw.len() > MAX_FORWARDED_HEADER_BYTES {
        return Err(());
    }
    Ok(())
}

fn parse_forwarded_node(raw: &str) -> Result<IpAddr, ()> {
    let value = unquote(raw.trim())?;
    if value.is_empty()
        || value.eq_ignore_ascii_case("unknown")
        || value.starts_with('_')
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(());
    }
    if let Ok(address) = value.parse::<IpAddr>() {
        return Ok(normalize_ip(address));
    }
    if let Ok(address) = value.parse::<SocketAddr>() {
        return Ok(normalize_ip(address.ip()));
    }
    if let Some(address) = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        return address
            .parse::<Ipv6Addr>()
            .map(IpAddr::V6)
            .map(normalize_ip)
            .map_err(|_| ());
    }
    Err(())
}

fn unquote(value: &str) -> Result<&str, ()> {
    match (value.strip_prefix('"'), value.strip_suffix('"')) {
        (Some(without_start), Some(_)) if value.len() >= 2 => {
            let inner = &without_start[..without_start.len().saturating_sub(1)];
            if inner.contains('"') || inner.contains('\\') {
                return Err(());
            }
            Ok(inner)
        }
        (None, None) => Ok(value),
        _ => Err(()),
    }
}

fn normalize_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(address)),
        address => address,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver(values: &[&str]) -> TrustedClientIpResolver {
        let values = values
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>();
        TrustedClientIpResolver::from_cidrs(&values)
            .unwrap_or_else(|error| panic!("测试 CIDR 应有效: {error}"))
    }

    fn socket(value: &str) -> SocketAddr {
        value
            .parse()
            .unwrap_or_else(|error| panic!("测试 SocketAddr 应有效: {error}"))
    }

    fn ip(value: &str) -> IpAddr {
        value
            .parse()
            .unwrap_or_else(|error| panic!("测试 IP 应有效: {error}"))
    }

    #[test]
    fn direct_clients_cannot_spoof_forwarding_headers() {
        let resolution = resolver(&["10.0.0.0/8"]).resolve(
            Some(socket("203.0.113.9:4012")),
            Some("for=198.51.100.7"),
            Some("192.0.2.5"),
        );
        assert_eq!(resolution.ip, Some(ip("203.0.113.9")));
        assert_eq!(resolution.source, ResolutionSource::DirectPeer);
    }

    #[test]
    fn trusted_xff_chain_stops_at_nearest_untrusted_client() {
        let resolution = resolver(&["10.0.0.0/8"]).resolve(
            Some(socket("10.0.0.3:443")),
            None,
            Some("192.0.2.66, 198.51.100.8, 10.0.0.2"),
        );
        assert_eq!(resolution.ip, Some(ip("198.51.100.8")));
        assert_eq!(resolution.source, ResolutionSource::XForwardedFor);
    }

    #[test]
    fn forwarded_supports_ipv4_ipv6_and_proxy_ports() {
        let resolution = resolver(&["10.0.0.0/8", "2001:db8:1::/64"]).resolve(
            Some(socket("[2001:db8:1::9]:443")),
            Some("for=\"[2001:db8:2::7]:4711\";proto=https, for=10.0.0.2:8443"),
            None,
        );
        assert_eq!(resolution.ip, Some(ip("2001:db8:2::7")));
        assert_eq!(resolution.source, ResolutionSource::Forwarded);
    }

    #[test]
    fn malformed_or_ambiguous_headers_fall_back_to_trusted_peer() {
        let trusted_peer = Some(socket("10.0.0.3:443"));
        for forwarded in [
            "for=unknown",
            "for=_hidden",
            "for=192.0.2.1;for=192.0.2.2",
            "by=10.0.0.2",
            "for=\"192.0.2.1",
            "for=not-an-ip",
        ] {
            let resolution = resolver(&["10.0.0.0/8"]).resolve(
                trusted_peer,
                Some(forwarded),
                Some("198.51.100.7"),
            );
            assert_eq!(resolution.ip, Some(ip("10.0.0.3")), "{forwarded}");
            assert_eq!(
                resolution.source,
                ResolutionSource::InvalidForwardingHeader,
                "{forwarded}"
            );
        }
    }

    #[test]
    fn excessive_header_size_or_hops_falls_back_to_peer() {
        let oversized = format!("for={}", "1".repeat(MAX_FORWARDED_HEADER_BYTES));
        let too_many = std::iter::repeat("192.0.2.1")
            .take(MAX_FORWARDED_HOPS + 1)
            .collect::<Vec<_>>()
            .join(",");
        for (forwarded, xff) in [
            (Some(oversized.as_str()), None),
            (None, Some(too_many.as_str())),
        ] {
            let resolution =
                resolver(&["10.0.0.0/8"]).resolve(Some(socket("10.0.0.3:443")), forwarded, xff);
            assert_eq!(resolution.ip, Some(ip("10.0.0.3")));
            assert_eq!(resolution.source, ResolutionSource::InvalidForwardingHeader);
        }
    }

    #[test]
    fn empty_trust_list_ignores_headers_and_missing_peer_is_unknown() {
        let direct =
            resolver(&[]).resolve(Some(socket("127.0.0.1:8080")), None, Some("198.51.100.7"));
        assert_eq!(direct.ip, Some(ip("127.0.0.1")));
        assert_eq!(direct.source, ResolutionSource::DirectPeer);

        let missing = resolver(&["127.0.0.1/32"]).resolve(None, None, None);
        assert_eq!(missing.ip, None);
        assert_eq!(missing.identity(), "unknown");
        assert_eq!(missing.source, ResolutionSource::MissingPeer);
    }

    #[test]
    fn cidr_validation_rejects_unsafe_or_malformed_configuration() {
        for value in [
            "0.0.0.0/0",
            "::/0",
            "10.0.0.1",
            "10.0.0.0/33",
            "2001:db8::/129",
            "invalid/24",
        ] {
            assert!(
                TrustedClientIpResolver::from_cidrs(&[value.to_string()]).is_err(),
                "{value} 必须被拒绝"
            );
        }
        assert!(TrustedClientIpResolver::from_cidrs(&["10.1.2.3/8".to_string()]).is_ok());
    }

    #[test]
    fn ipv4_mapped_ipv6_is_normalized_before_trust_comparison() {
        let resolution = resolver(&["127.0.0.0/8"]).resolve(
            Some(socket("[::ffff:127.0.0.1]:443")),
            None,
            Some("198.51.100.7"),
        );
        assert_eq!(resolution.ip, Some(ip("198.51.100.7")));
    }
}
