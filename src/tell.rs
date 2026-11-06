//! What the shopper is told by e-mail, through the server's mailer ([`Kernel::queue_mail`]): their
//! order, that it has shipped, and money sent back. The events beside these stay for webhooks and
//! mailers of the workspace's own. A mail the mailer refuses is logged; it never stops the order.

use crate::pages::esc;
use ferrum_kernel::{Error, Kernel, Result, Tx};
use uuid::Uuid;

/// Queues one mail to the shopper: its lines as paragraphs, then the order's link when the store
/// has its address.
pub async fn shopper(k: &Kernel, tx: &mut Tx, tenant: Uuid, to: Option<&str>, subject: &str, lines: &[String], link: Option<&str>) -> Result<()> {
    let Some(to) = to.map(str::trim).filter(|t| t.contains('@')) else {
        return Ok(());
    };
    let mut html: String = lines.iter().map(|l| format!("<p>{}</p>", esc(l))).collect();
    if let Some(link) = link {
        html.push_str(&format!(r#"<p><a href="{}">Follow your order</a></p>"#, esc(link)));
    }
    match k.queue_mail(tx, tenant, to, subject, &html).await {
        Err(e @ (Error::Validation(_) | Error::Invalid(_) | Error::Unavailable(_))) => {
            tracing::warn!(%tenant, "a mail to a shopper was not queued: {e}");
            Ok(())
        }
        done => done,
    }
}
