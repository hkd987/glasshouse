//! Checks the `[mail]` section (NV-3): what a device would refuse, the
//! console refuses to sign.

use crate::document::{MailServer, MailService, PolicyDocument};

/// Whether `host` is a host name: letters, digits, dots and dashes, no
/// scheme, user, port or path.
fn is_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

fn server(name: &str, server: Option<&MailServer>, errors: &mut Vec<String>) {
    let Some(server) = server else {
        errors.push(format!(
            "mail.{name}: an IMAP service needs its {name} server"
        ));
        return;
    };
    if !is_host(&server.host) {
        errors.push(format!(
            "mail.{name}.host {:?}: a host name such as mail.example.com, with no scheme, port or path",
            server.host
        ));
    }
    if server.port == 0 {
        errors.push(format!("mail.{name}.port: 1 to 65535"));
    }
}

pub(crate) fn validate(doc: &PolicyDocument, errors: &mut Vec<String>) {
    let Some(mail) = &doc.mail else {
        return;
    };
    match mail.service {
        MailService::Imap => {
            server("incoming", mail.incoming.as_ref(), errors);
            server("outgoing", mail.outgoing.as_ref(), errors);
        }
        MailService::Microsoft365 | MailService::GoogleWorkspace => {
            if !mail.tenant.as_deref().is_some_and(is_host) {
                errors.push(format!(
                    "mail.tenant: {} needs the org's domain, such as acme.com",
                    mail.service.title()
                ));
            }
        }
    }
    if let Some(template) = &mail.address_template {
        let shaped = template.matches("{login}").count() == 1
            && template
                .split_once('@')
                .is_some_and(|(local, domain)| local.contains("{login}") && is_host(domain))
            && !template
                .chars()
                .any(|c| c.is_whitespace() || "<>,;\"".contains(c));
        if !shaped {
            errors.push(format!(
                "mail.address_template {template:?}: like {{login}}@acme.com"
            ));
        }
    }
}

/// `template`'s address for `login`.
pub fn address_for(template: &str, login: &str) -> String {
    template.replace("{login}", login)
}

#[cfg(test)]
mod tests {
    use crate::{DEFAULT_POLICY, Policy};

    fn with(mail: &str) -> Result<Policy, String> {
        Policy::from_toml(&format!("{DEFAULT_POLICY}\n{mail}")).map_err(|e| e.to_string())
    }

    #[test]
    fn nv3_mail_settings_check_hosts_ports_and_the_address_template() {
        let ok = with(
            r#"
[mail]
service = "imap"
address_template = "{login}@acme.example"
incoming = { host = "imap.acme.example", port = 993, security = "tls" }
outgoing = { host = "smtp.acme.example", port = 587, security = "starttls" }
"#,
        )
        .unwrap();
        let mail = ok.mail().unwrap();
        assert_eq!(mail.outgoing.as_ref().unwrap().port, 587);
        assert_eq!(
            super::address_for(mail.address_template.as_deref().unwrap(), "dana"),
            "dana@acme.example"
        );
        assert!(
            with(
                r#"[mail]
service = "microsoft-365"
tenant = "acme.com"
"#
            )
            .is_ok()
        );
        assert!(Policy::from_toml(DEFAULT_POLICY).unwrap().mail().is_none());

        for (bad, says) in [
            (r#"service = "imap""#, "incoming server"),
            (
                r#"service = "imap"
incoming = { host = "https://imap.acme.example", port = 993, security = "tls" }
outgoing = { host = "smtp.acme.example", port = 587, security = "starttls" }"#,
                "mail.incoming.host",
            ),
            (
                r#"service = "imap"
incoming = { host = "imap.acme.example", port = 0, security = "tls" }
outgoing = { host = "smtp.acme.example", port = 587, security = "starttls" }"#,
                "mail.incoming.port",
            ),
            (r#"service = "google-workspace""#, "mail.tenant"),
            (
                r#"service = "microsoft-365"
tenant = "acme.com"
address_template = "dana@acme.com""#,
                "address_template",
            ),
        ] {
            let refused = with(&format!("[mail]\n{bad}\n")).unwrap_err();
            assert!(refused.contains(says), "{bad}: {refused}");
        }
        // No plain connections: there's nothing to name one with.
        assert!(
            with(
                r#"[mail]
service = "imap"
incoming = { host = "imap.acme.example", port = 143, security = "none" }
outgoing = { host = "smtp.acme.example", port = 25, security = "starttls" }
"#
            )
            .is_err()
        );
    }
}
