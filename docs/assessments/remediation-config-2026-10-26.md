# 配置与 TOTP 密钥域修复记录（2026-10-26）

范围：P03 / DI-04 / DI-05 / DI-06。只修改本地源码、示例与文档，未部署、未提交、未读取生产配置或凭据。

## 问题与修正

| 发现 | 修正 |
|---|---|
| DI-04：配置允许 `security.totp.digits=6..=8`，但校验器固定六位 | `validate_totp_settings` 收窄为仅接受 6；错误消息含 `security.totp.digits`，不回显密钥 |
| DI-06：TOTP AEAD 密钥未与 Token/Step-up/全部邮箱验证码域隔离 | `Settings::validate` 复用 `validate_verification_secret` 覆盖 Token/Step-up 的 active+retiring，并逐项比较 `email.verification` / `email.change` / `email.mfa` / `email.login` |
| DI-06：secret 目录缺少 TOTP 与三类邮箱验证码绑定 | `SECRET_BINDINGS` 新增 `security_totp_aead_key`、`email_change_secret`、`email_mfa_secret`、`email_login_secret`；未扩充环境变量白名单 |
| DI-05：轮换文档把“不可解密”误写成用户下次登录可自行重新 setup | `CONFIGURATION.md`、示例配置与 `TotpSettings` 注释改为：密钥是配置原文 UTF-8 的 SHA-256（不是截取前 32 位）；更换会使存量密文无法解密、用户无法登录；旧密钥须独立受控备份；当前无无缝轮换/多 keyring 工具 |

同时移除把已退役 Step-up 流程写成当前应用已装配防线的错误表述：`README.md`、`docs/contracts/AUTHZ_GRANTS.md`、`docs/contracts/AUDIT.md`、`docs/architecture/account-system-roadmap.md`（顶部说明）、`frontend/AGENTS.md`、`deploy/config.cloud.example.toml` 及账户 Action 模块注释均改为“框架能力存在、当前应用未装配”。没有新增 Step-up 产品流程、UI 或环境变量。

## 验证

- `cargo test --lib config:: --locked -j 1`：**79 passed / 0 failed**，退出 0。[最终日志](../../target/infrastructure-remediation/config-green-final.log)
- 摘除式变异（每项按原始字节恢复并核对 SHA-256）：
  - 恢复 `digits` 为 `6..=8`：`totp_configuration_only_accepts_the_supported_six_digits` 断言失败；
  - 删除 TOTP 跨域隔离块：`totp_key_is_isolated_from_all_authentication_secret_domains` 断言失败；
  - 逐条删除 4 个 secret 绑定：`totp_and_email_secret_bindings_override_nested_file_values` 分别断言失败；
  - 恢复后 `totp_and_email_secret_files_reject_invalid_contents_without_echoing_them` 通过。
  - 逐项记录：[config-mutations.log](../../target/infrastructure-remediation/config-mutations.log)
- `rustfmt --edition 2021 --check src/config/mod.rs src/config/source.rs` 退出 0；行尾未归一化。

## 边界

- 当前没有多 keyring/无缝轮换工具；直接更换 `security.totp.aead_key` 会让已存 `users.totp_secret` 无法解密，必须保留旧密钥的独立受控备份，并在维护窗口重加密或恢复旧密钥。
- 新 secret 只支持 secret 目录，不支持环境变量；`security_totp_aead_key` 等文件仍受单行、非空、UTF-8、≤64 KiB 约束。
- 本轮只覆盖配置层与文档；最终 full / integration / 严格 Clippy / 冷缓存 MSRV 由稳定快照总门禁统一验证，尚未宣称生产可用。
