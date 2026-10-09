//! 启动期加载可替换邮件模板，动态值只做一次插值，HTML 值统一转义。

use anyhow::{bail, Context};
use std::collections::BTreeMap;
use std::path::Path;

const WRAPPER: &str = include_str!("email_templates/layout.html");

macro_rules! mail {
    ($name:literal, $($variable:literal),*) => {
        ($name, include_str!(concat!("email_templates/", $name, ".subject.txt")), include_str!(concat!("email_templates/", $name, ".txt")), include_str!(concat!("email_templates/", $name, ".html")), &[$($variable),*] as &[&str])
    };
}

const MAILS: &[(&str, &str, &str, &str, &[&str])] = &[
    mail!("registration", "code", "minutes"),
    mail!("change_email", "code", "minutes"),
    mail!("login", "code", "minutes"),
    mail!("mfa", "code", "minutes"),
    mail!("password_reset", "reset_url", "minutes"),
    mail!("new_device", "ip", "device", "occurred_at"),
    mail!(
        "feishu_pull_failure",
        "datasource_title",
        "failures",
        "last_error"
    ),
];

#[derive(Clone)]
pub(crate) struct EmailTemplates {
    layout: String,
    messages: BTreeMap<String, (String, String, String)>,
}

impl EmailTemplates {
    pub(crate) fn load(directory: Option<&Path>) -> anyhow::Result<Self> {
        if directory.is_some_and(|path| !path.is_dir()) {
            bail!("email.template_dir 必须指向存在的目录");
        }
        let layout = load_file(directory, "layout.html", WRAPPER)?;
        validate(
            &layout,
            &["system_name", "content"],
            &["system_name", "content"],
        )?;
        let mut messages = BTreeMap::new();
        for (kind, subject, plain, html, variables) in MAILS {
            let subject = load_file(directory, &format!("{kind}.subject.txt"), subject)?;
            let plain = load_file(directory, &format!("{kind}.txt"), plain)?;
            let html = load_file(directory, &format!("{kind}.html"), html)?;
            let mut allowed = variables.to_vec();
            allowed.push("system_name");
            validate(&subject, &allowed, &["system_name"])
                .with_context(|| format!("邮件主题模板 {kind} 无效"))?;
            validate(&plain, &allowed, &allowed)
                .with_context(|| format!("纯文本邮件模板 {kind} 无效"))?;
            validate(&html, &allowed, variables)
                .with_context(|| format!("HTML 邮件模板 {kind} 无效"))?;
            messages.insert((*kind).to_owned(), (subject, plain, html));
        }
        Ok(Self { layout, messages })
    }

    pub(crate) fn render(
        &self,
        kind: &str,
        system_name: &str,
        values: &[(&str, &str)],
    ) -> anyhow::Result<(String, String, String)> {
        let (subject, plain, html) = self.messages.get(kind).context("未知邮件模板类型")?;
        let mut values = values.to_vec();
        values.push(("system_name", system_name.trim()));
        let subject = interpolate(subject, &values, false)?
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>();
        let plain = interpolate(plain, &values, false)?;
        let content = interpolate(html, &values, true)?;
        // content 已完成动态值转义，外框只插入可信模板片段。
        let html = interpolate(
            &self.layout,
            &[
                ("system_name", &escape_html(system_name.trim())),
                ("content", &content),
            ],
            false,
        )?;
        Ok((subject.trim().to_owned(), plain, html))
    }
}

fn load_file(directory: Option<&Path>, name: &str, default: &str) -> anyhow::Result<String> {
    let Some(directory) = directory else {
        return Ok(default.to_owned());
    };
    match std::fs::read_to_string(directory.join(name)) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(default.to_owned()),
        Err(error) => Err(error).with_context(|| format!("无法读取邮件模板 {name}")),
    }
}

fn validate(template: &str, allowed: &[&str], required: &[&str]) -> anyhow::Result<()> {
    if template.trim().is_empty() {
        bail!("邮件模板不能为空");
    }
    let placeholders = allowed.iter().map(|name| (*name, "")).collect::<Vec<_>>();
    interpolate(template, &placeholders, false)?;
    for name in required {
        if !template.contains(&format!("{{{{{name}}}}}")) {
            bail!("邮件模板缺少必要变量 {name}");
        }
    }
    Ok(())
}

fn interpolate(template: &str, values: &[(&str, &str)], html: bool) -> anyhow::Result<String> {
    let mut rendered = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        rendered.push_str(&rest[..start]);
        let tail = &rest[start + 2..];
        let end = tail.find("}}").context("邮件模板变量未闭合")?;
        let name = &tail[..end];
        let value = values
            .iter()
            .find(|(key, _)| *key == name)
            .context("邮件模板包含未知或缺失变量")?
            .1;
        rendered.push_str(&if html {
            escape_html(value)
        } else {
            value.to_owned()
        });
        rest = &tail[end + 2..];
    }
    rendered.push_str(rest);
    Ok(rendered)
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_code_mail_uses_brand_style_and_escapes_values() -> anyhow::Result<()> {
        let templates = EmailTemplates::load(None)?;
        let (subject, text, html) = templates.render(
            "registration",
            "业务<&>平台\n",
            &[("code", "123456"), ("minutes", "10")],
        )?;
        assert!(subject.starts_with("业务<&>平台 注册"));
        assert!(!subject.contains('\n'));
        assert!(text.contains("123456"));
        assert!(text.contains("业务<&>平台"));
        assert!(html.contains("业务&lt;&amp;&gt;平台"));
        assert!(html.contains("#0b1530"));
        assert!(html.contains("123456"));
        assert!(!html.contains("{{system_name}}"));
        Ok(())
    }

    #[test]
    fn all_mail_types_preserve_their_operational_values() -> anyhow::Result<()> {
        let templates = EmailTemplates::load(None)?;
        for kind in ["registration", "change_email", "login", "mfa"] {
            let (_, text, html) =
                templates.render(kind, "测试平台", &[("code", "654321"), ("minutes", "3")])?;
            assert!(text.contains("654321") && html.contains("654321"), "{kind}");
            assert!(text.contains("3") && html.contains("3"), "{kind}");
        }
        let (_, text, html) = templates.render(
            "password_reset",
            "测试平台",
            &[
                ("reset_url", "https://example.com/reset?token=abc&x=1"),
                ("minutes", "5"),
            ],
        )?;
        assert!(text.contains("token=abc&x=1"));
        assert!(html.contains("token=abc&amp;x=1"));
        let (_, text, html) = templates.render(
            "new_device",
            "测试平台",
            &[
                ("ip", "127.0.0.1"),
                ("device", "<script>{{code}}</script>"),
                ("occurred_at", "1234567890"),
            ],
        )?;
        assert!(text.contains("<script>{{code}}</script>"));
        assert!(html.contains("&lt;script&gt;{{code}}&lt;/script&gt;"));
        assert!(html.contains("1234567890"));
        let (_, text, html) = templates.render(
            "feishu_pull_failure",
            "测试平台",
            &[
                ("datasource_title", "费用表"),
                ("failures", "7"),
                ("last_error", "1254302 <Permission denied>"),
            ],
        )?;
        assert!(text.contains("费用表") && text.contains("7") && text.contains("1254302"));
        assert!(html.contains("&lt;Permission denied&gt;") && html.contains("体检"));
        assert!(!html.contains("非本人"));
        Ok(())
    }

    #[test]
    fn rejects_missing_runtime_variables() -> anyhow::Result<()> {
        let templates = EmailTemplates::load(None)?;
        assert!(templates
            .render("registration", "平台", &[("minutes", "10")])
            .is_err());
        Ok(())
    }

    #[test]
    fn custom_files_override_defaults_and_invalid_files_fail_closed() -> anyhow::Result<()> {
        let directory = std::env::temp_dir().join(format!("yang-email-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let result = (|| -> anyhow::Result<()> {
            let custom = "定制 {{system_name}} 验证码 {{code}}，{{minutes}} 分钟有效";
            std::fs::write(directory.join("registration.html"), custom)?;
            let templates = EmailTemplates::load(Some(&directory))?;
            let (_, plain, html) = templates.render(
                "registration",
                "定制平台",
                &[("code", "123456"), ("minutes", "10")],
            )?;
            assert!(html.contains("定制 定制平台 验证码 123456"));
            assert!(html.contains("#0b1530"));
            assert!(plain.contains("请不要把验证码告诉任何人"));
            std::fs::write(directory.join("registration.html"), "{{unknown}}")?;
            assert!(EmailTemplates::load(Some(&directory)).is_err());
            std::fs::write(directory.join("registration.html"), "{{code}}")?;
            assert!(EmailTemplates::load(Some(&directory)).is_err());
            std::fs::write(directory.join("registration.html"), "{{code")?;
            assert!(EmailTemplates::load(Some(&directory)).is_err());
            std::fs::write(directory.join("registration.html"), " ")?;
            assert!(EmailTemplates::load(Some(&directory)).is_err());
            Ok(())
        })();
        std::fs::remove_dir_all(&directory)?;
        result?;
        assert!(EmailTemplates::load(Some(&directory)).is_err());
        Ok(())
    }

    #[test]
    fn alert_subject_cannot_inject_headers() -> anyhow::Result<()> {
        let templates = EmailTemplates::load(None)?;
        let (subject, _, _) = templates.render(
            "feishu_pull_failure",
            "平台\r\nBcc: bad@example.com",
            &[
                ("datasource_title", "表\r\nSubject: forged"),
                ("failures", "3"),
                ("last_error", "超时"),
            ],
        )?;
        assert!(!subject.chars().any(char::is_control));
        Ok(())
    }
}
