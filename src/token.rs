//! Secrets the storefront hands out. Cart tokens are 256 random bits; only their SHA-256 is
//! stored. Order access tokens are an HMAC-SHA256 of the order id under a key derived from
//! Commerce's own key in the vault (`keys.rs`), kept nowhere, and are checked in constant time.

use crate::keys;
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ferrum_domain::util::id;
use ferrum_kernel::vault::Derived;
use ferrum_kernel::{Doc, DocHook, Error, HookCtx, Kernel, Result, Tx};
use rand::Rng;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// A new cart token and the hash stored on the cart.
pub fn new_cart_token() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let hash = hash(&token);
    (token, hash)
}

pub fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Shaped like a token we issue (43 URL-safe base64 characters): anything else is not looked up.
pub fn well_formed(token: &str) -> bool {
    token.len() == 43 && token.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn order_message(order: Uuid) -> Vec<u8> {
    [b"ferrum-commerce-order:".as_slice(), order.as_bytes()].concat()
}

/// The access token of an order, `v{n}.{mac}`: signed with the key for order links from version n of Commerce's key,
/// so the same every time while that version is the current one (checkout replays return it again). Before 0.11 it was
/// the old key's MAC alone (43 characters).
pub fn order_token(key: &Derived, order: Uuid) -> String {
    keys::signed(key.bytes(), key.version(), &order_message(order))
}

/// [`order_token`] with Commerce's key now. Inside a transaction, the vault answers on a second connection.
pub async fn issue(k: &Kernel, tenant: Uuid, order: Uuid) -> Result<String> {
    Ok(order_token(&keys::current(k, tenant, keys::ORDERS).await?, order))
}

/// [`issue`] for what goes in a mail or an event: none while the vault is closed (said in the log), so that shipping,
/// payments and refunds go on without the link rather than stop.
pub async fn for_mail(k: &Kernel, tenant: Uuid, order: Uuid) -> Result<Option<String>> {
    match issue(k, tenant, order).await {
        Ok(t) => Ok(Some(t)),
        Err(Error::Unavailable(why)) => {
            tracing::warn!(%order, "an order's link was left out of what is sent: {why}");
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// The key a presented token was signed with, to take before any transaction (the vault answers on a connection of its
/// own); none for a token from before 0.11, or none at all.
pub async fn presented(k: &Kernel, tenant: Uuid, token: Option<&str>) -> Result<Option<Derived>> {
    match token.and_then(|t| t.trim().split_once('.')).and_then(|(v, _)| keys::version(v)) {
        Some(v) => keys::at(k, tenant, keys::ORDERS, v).await,
        None => Ok(None),
    }
}

/// Whether `token` is `order`'s: signed with `key`, which [`presented`] gave for the version it names; or a token from
/// before 0.11, signed with the old key, for an order this build did not make (`commerce_order_signed`).
pub async fn order_token_valid(k: &Kernel, tx: &mut Tx, tenant: Uuid, order: Uuid, token: &str, key: Option<&Derived>) -> Result<bool> {
    let token = token.trim();
    match token.split_once('.') {
        Some((v, mac)) => {
            let (Some(v), Ok(mac)) = (keys::version(v), URL_SAFE_NO_PAD.decode(mac)) else { return Ok(false) };
            Ok(key.is_some_and(|key| key.version() == v && keys::mac_ok(key.bytes(), &order_message(order), &mac)))
        }
        None => {
            let Ok(mac) = URL_SAFE_NO_PAD.decode(token) else { return Ok(false) };
            let made_here: bool = sqlx::query_scalar("select exists (select 1 from commerce_order_signed where tenant_id = $1 and sales_order = $2)")
                .bind(tenant)
                .bind(order)
                .fetch_one(&mut **tx)
                .await?;
            // (In the caller's transaction: rare, an order from before 0.11 opened with the link it was sent.)
            Ok(!made_here && keys::old_ok(k, tenant, &order_message(order), &mac).await?)
        }
    }
}

/// Marks each sales order made from 0.11 on: the old key never opens it, whatever it is given (`keys.rs`).
pub struct MadeHere;

#[async_trait]
impl DocHook for MadeHere {
    fn name(&self) -> &'static str {
        "commerce.order_signed"
    }

    async fn after_save(&self, hc: &mut HookCtx<'_>, doc: &Doc, old: Option<&Doc>) -> Result<()> {
        if old.is_none()
            && let Some(order) = id(doc, "id")
        {
            sqlx::query("insert into commerce_order_signed (tenant_id, sales_order) values ($1, $2) on conflict do nothing")
                .bind(hc.ctx.tenant)
                .bind(order)
                .execute(&mut **hc.tx)
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cart_tokens_are_random_and_only_hashed() {
        let (a, ha) = new_cart_token();
        let (b, hb) = new_cart_token();
        assert_ne!(a, b);
        assert!(well_formed(&a) && well_formed(&b));
        assert_eq!(ha, hash(&a));
        assert_eq!(ha.len(), 64);
        assert_ne!(ha, hb);
        assert!(!well_formed("short") && !well_formed(&format!("{}!", &a[..42])));
    }

    #[test]
    fn order_macs_bind_key_and_order() {
        let (k1, k2) = ([7u8; 32], [8u8; 32]);
        let (o1, o2) = (Uuid::now_v7(), Uuid::now_v7());
        let token = keys::signed(&k1, 1, &order_message(o1));
        let (v, mac) = token.split_once('.').unwrap();
        let mac = URL_SAFE_NO_PAD.decode(mac).unwrap();
        assert_eq!(keys::version(v), Some(1));
        assert!(keys::mac_ok(&k1, &order_message(o1), &mac));
        assert!(!keys::mac_ok(&k1, &order_message(o2), &mac), "another order");
        assert!(!keys::mac_ok(&k2, &order_message(o1), &mac), "another tenant's key");
        // A token from before 0.11: the old key's MAC alone, 43 characters, no dot.
        let old = URL_SAFE_NO_PAD.encode(keys::mac(&k1, &order_message(o1)));
        assert!(old.len() == 43 && !old.contains('.'));
    }
}
