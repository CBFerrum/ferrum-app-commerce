//! What the shopper is told: message templates the package declares (a workspace edits their wording and
//! languages), sent in the transaction of what happened. The events beside them stay for webhooks and
//! mailers of the workspace's own. A message that cannot be sent is logged; it never stops the order.

use ferrum_kernel::{Error, Kernel, Result, Tx};
use uuid::Uuid;

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
    secret: Vec<String>,
) -> Result<()> {
    let Some(to) = to.map(str::trim).filter(|t| t.contains('@')) else { return Ok(()) };
    let m = ferrum_kernel::messages::Message {
        template: template.into(),
        entity: Some("SalesOrder".into()),
        record: Some(so),
        to: vec![ferrum_kernel::messages::Recipient::address(to)],
        values,
        secret,
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
