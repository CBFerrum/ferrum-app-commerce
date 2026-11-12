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

/// Sends one of the package's message templates to the shopper of an order, in the caller's transaction,
/// with what it names as values. A template the workspace does not have yet (an app updated but its
/// templates not), an address that cannot be reached or a channel that is not set up is logged: telling
/// the shopper never stops what happened to the order.
pub async fn message(
    k: &Kernel,
    tx: &mut Tx,
    ctx: &ferrum_kernel::Ctx,
    template: &str,
    so: Uuid,
    to: Option<&str>,
    values: serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    let Some(to) = to.map(str::trim).filter(|t| t.contains('@')) else { return Ok(()) };
    let m = ferrum_kernel::messages::Message {
        template: template.into(),
        entity: Some("SalesOrder".into()),
        record: Some(so),
        to: vec![ferrum_kernel::messages::Recipient::address(to)],
        values,
        ..Default::default()
    };
    match ferrum_kernel::messages::send(k, tx, ctx, &m).await {
        Err(e @ (Error::Validation(_) | Error::Invalid(_) | Error::Unavailable(_) | Error::NotFound(_))) => {
            tracing::warn!(%so, template, "a message to a shopper was not sent: {e}");
            Ok(())
        }
        done => done.map(|_| ()),
    }
}

/// The address a shopper follows an order at, when the store has its own address.
pub async fn order_link(tx: &mut Tx, tenant: Uuid, store_url: Option<&str>, number: &str, so: Uuid) -> Result<Option<String>> {
    match store_url {
        Some(url) => Ok(Some(crate::orders::tracking_url(url, number, &crate::token::order_token(tx, tenant, so).await?))),
        None => Ok(None),
    }
}
