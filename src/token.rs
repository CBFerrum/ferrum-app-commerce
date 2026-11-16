//! Secrets the storefront hands out. Cart tokens are 256 random bits; only their SHA-256 is
//! stored. Order access tokens are an HMAC-SHA256 of the order id under a key derived from
//! Commerce's own key in the vault (`keys.rs`), kept nowhere, and are checked in constant time.

use crate::keys;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ferrum_kernel::vault::Derived;
use ferrum_kernel::{Error, Kernel, Result};
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
/// so the same every time while that version is the current one (checkout replays return it again).
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

/// A token as it reads: the version of Commerce's key that signed it and its MAC; none unless well formed.
fn read(token: &str) -> Option<(i32, Vec<u8>)> {
    let (v, mac) = token.trim().split_once('.')?;
    Some((keys::version(v)?, URL_SAFE_NO_PAD.decode(mac).ok()?))
}

/// The key a presented token was signed with, to take before any transaction (the vault answers on a connection of its
/// own); none without a well-formed token, or once its version is gone.
pub async fn presented(k: &Kernel, tenant: Uuid, token: Option<&str>) -> Result<Option<Derived>> {
    match token.and_then(read) {
        Some((v, _)) => keys::at(k, tenant, keys::ORDERS, v).await,
        None => Ok(None),
    }
}

/// Whether `token` is `order`'s: signed with `key`, which [`presented`] gave for the version it names (in constant time).
pub fn order_token_valid(order: Uuid, token: &str, key: Option<&Derived>) -> bool {
    read(token).is_some_and(|(v, mac)| key.is_some_and(|key| key.version() == v && keys::mac_ok(key.bytes(), &order_message(order), &mac)))
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
        let (v, mac) = read(&token).expect("well formed");
        assert_eq!(v, 1);
        assert!(keys::mac_ok(&k1, &order_message(o1), &mac));
        assert!(!keys::mac_ok(&k1, &order_message(o2), &mac), "another order");
        assert!(!keys::mac_ok(&k2, &order_message(o1), &mac), "another tenant's key");
        // One that names no version (the MAC alone) is refused like anything else malformed.
        let bare = URL_SAFE_NO_PAD.encode(keys::mac(&k1, &order_message(o1)));
        for bad in [bare.as_str(), "v0.abc", "x1.abc", "v1.not base64", ""] {
            assert!(read(bad).is_none(), "{bad}");
        }
    }
}
