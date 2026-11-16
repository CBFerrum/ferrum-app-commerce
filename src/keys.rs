//! Commerce's keys, from its own key in the vault (Ferrum docs/operations.md, "The vault"). An order's access token,
//! which a shopper's links and the order's page carry, is signed with a key derived for that purpose (`order-links`)
//! when needed and kept nowhere. It names the version of Commerce's key it came from, so that a new version
//! (`ferrum task vault_rotate level:owner … owner:commerce`) leaves the tokens already sent working. What names no
//! version is refused like anything else malformed.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ferrum_kernel::vault::{Derived, Scope, Vault};
use ferrum_kernel::{Error, Kernel, Result};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::RwLock;
use uuid::Uuid;

/// Orders' access tokens.
pub const ORDERS: &str = "order-links";

/// Commerce's handle on the vault, from its registration.
static VAULT: RwLock<Option<Vault>> = RwLock::new(None);

/// Keeps the handle the registration gave Commerce.
// ponytail: the last registration's handle serves the whole process (`ferrum start` sets the server up twice and serves
// with the second); keep it in what is registered if one process ever serves two kernels at once.
pub(crate) fn remember(v: Vault) {
    *VAULT.write().unwrap_or_else(|e| e.into_inner()) = Some(v);
}

fn handle() -> Result<Vault> {
    VAULT.read().unwrap_or_else(|e| e.into_inner()).clone().ok_or_else(|| Error::Unavailable("Commerce was registered without its vault handle".into()))
}

/// What a person or a caller is told when the vault cannot be used; why goes to the log.
fn unavailable(e: Error) -> Error {
    match e {
        Error::Unavailable(why) | Error::Retryable(why) => {
            // The start's log and the doctor say why the vault is closed; this is the same reason, for each use.
            tracing::debug!("Commerce's keys: {why}");
            Error::Unavailable("Commerce cannot sign or check order links right now: its keys are unavailable".into())
        }
        e => e,
    }
}

/// The key for `purpose` in the workspace now, with the version it came from.
pub(crate) async fn current(k: &Kernel, tenant: Uuid, purpose: &str) -> Result<Derived> {
    handle()?.derive(k, Scope::Workspace(tenant), purpose).await.map_err(unavailable)
}

/// The key for `purpose` from version `version` of Commerce's key; none once that version is gone.
pub(crate) async fn at(k: &Kernel, tenant: Uuid, purpose: &str, version: i32) -> Result<Option<Derived>> {
    match handle()?.derive_at(k, Scope::Workspace(tenant), purpose, version).await {
        Ok(d) => Ok(Some(d)),
        Err(Error::NotFound(_)) => Ok(None),
        Err(e) => Err(unavailable(e)),
    }
}

/// HMAC-SHA256 of `message` under `key`.
pub(crate) fn mac(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes keys of any length");
    m.update(message);
    m.finalize().into_bytes().to_vec()
}

/// Whether `tag` is [`mac`] of `message` under `key` (in constant time).
pub(crate) fn mac_ok(key: &[u8], message: &[u8], tag: &[u8]) -> bool {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes keys of any length");
    m.update(message);
    m.verify_slice(tag).is_ok()
}

/// What a signature says as text: `v{version}.{mac}`.
pub(crate) fn signed(key: &[u8], version: i32, message: &[u8]) -> String {
    format!("v{version}.{}", URL_SAFE_NO_PAD.encode(mac(key, message)))
}

/// The version a signature names (`v3`), if it names one.
pub(crate) fn version(part: &str) -> Option<i32> {
    part.strip_prefix('v')?.parse().ok().filter(|v| *v > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_name_their_version_and_macs_check_in_constant_time() {
        assert_eq!((version("v3"), version("v0"), version("3"), version("vx")), (Some(3), None, None, None));
        let tag = mac(b"key", b"message");
        assert!(mac_ok(b"key", b"message", &tag) && !mac_ok(b"key", b"massage", &tag) && !mac_ok(b"other", b"message", &tag));
        assert_eq!(signed(b"key", 2, b"message"), format!("v2.{}", URL_SAFE_NO_PAD.encode(&tag)));
    }
}
