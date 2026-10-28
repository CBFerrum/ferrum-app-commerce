//! Secrets the storefront hands out. Cart tokens are 256 random bits; only their SHA-256 is
//! stored. Order access tokens are an HMAC-SHA256 of the order id under a per-tenant key kept in
//! `commerce_secret` (outside every entity, so no API, export or audit trail can reveal it) and
//! are checked in constant time.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ferrum_kernel::{Error, Result, Tx};
use hmac::{Hmac, Mac};
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

async fn tenant_key(tx: &mut Tx, tenant: Uuid) -> Result<Vec<u8>> {
    let existing: Option<Vec<u8>> =
        sqlx::query_scalar("select secret from commerce_secret where tenant_id = $1").bind(tenant).fetch_optional(&mut **tx).await?;
    if let Some(k) = existing {
        return Ok(k);
    }
    let mut fresh = [0u8; 32];
    rand::rng().fill(&mut fresh);
    sqlx::query("insert into commerce_secret (tenant_id, secret) values ($1, $2) on conflict do nothing")
        .bind(tenant)
        .bind(&fresh[..])
        .execute(&mut **tx)
        .await?;
    Ok(sqlx::query_scalar("select secret from commerce_secret where tenant_id = $1").bind(tenant).fetch_one(&mut **tx).await?)
}

fn order_mac(key: &[u8], order: Uuid) -> Result<Hmac<Sha256>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(Error::internal)?;
    mac.update(b"ferrum-commerce-order:");
    mac.update(order.as_bytes());
    Ok(mac)
}

/// The access token of an order (the same every time, so checkout replays return it again).
pub async fn order_token(tx: &mut Tx, tenant: Uuid, order: Uuid) -> Result<String> {
    let key = tenant_key(tx, tenant).await?;
    Ok(URL_SAFE_NO_PAD.encode(order_mac(&key, order)?.finalize().into_bytes()))
}

pub async fn order_token_valid(tx: &mut Tx, tenant: Uuid, order: Uuid, token: &str) -> Result<bool> {
    let Ok(raw) = URL_SAFE_NO_PAD.decode(token.trim()) else { return Ok(false) };
    let key = tenant_key(tx, tenant).await?;
    Ok(order_mac(&key, order)?.verify_slice(&raw).is_ok())
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
        let tag = order_mac(&k1, o1).unwrap().finalize().into_bytes();
        assert!(order_mac(&k1, o1).unwrap().verify_slice(&tag).is_ok());
        assert!(order_mac(&k1, o2).unwrap().verify_slice(&tag).is_err(), "another order");
        assert!(order_mac(&k2, o1).unwrap().verify_slice(&tag).is_err(), "another tenant's key");
    }
}
