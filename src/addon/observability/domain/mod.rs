//! 前端诊断上报的领域机制。

mod rate_limit;

pub(super) use rate_limit::FrontendErrorRateLimiter;
