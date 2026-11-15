//! Commerce's keys, from its own key in the vault (Ferrum docs/operations.md, "The vault"). An order's access token,
//! which a shopper's links and the order's page carry, is signed with a key derived for that purpose (`order-links`)
//! when needed and kept nowhere. It names the version of Commerce's key it came from, so that a new version
//! (`ferrum task vault_rotate level:owner … owner:commerce`) leaves the tokens already sent working.
//!
//! Until 0.11 a key kept unsealed in `commerce_secret` signed them, where whoever could read the database could forge the
//! token of any order. It signs nothing any more. An order's token never expires and is in mail already sent, so the old
//! key moves into the vault, sealed, as Commerce's secret `old-signing-key`, and keeps checking the tokens of orders
//! made before (or by the build before, during an upgrade or after a rollback): never those of an order this build made
//! (`commerce_order_signed`), whatever it is given. It moves the first time an old token is checked, or at Commerce's
//! next upkeep, once: under its row's lock, taken without waiting. A sandbox never takes it, so a link of the live
//! workspace does not open the copy. The previous build reads `commerce_secret` still; its rows go in the release after
//! this one.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use ferrum_kernel::vault::{Derived, New, Rule, Scope, Vault, Zeroizing};
use ferrum_kernel::{Error, Kernel, Result};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::RwLock;
use uuid::Uuid;

/// Orders' access tokens.
pub const ORDERS: &str = "order-links";
/// The key that signed them until 0.11, sealed in the vault: an order's token never expires.
const OLD: &str = "old-signing-key";

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

/// Whether `tag` is `message` signed by the old key: moved into the vault first, if it has not been yet. Never in a
/// sandbox.
pub(crate) async fn old_ok(k: &Kernel, tenant: Uuid, message: &[u8], tag: &[u8]) -> Result<bool> {
    let v = handle()?;
    let scope = Scope::Workspace(tenant);
    for first in [true, false] {
        match v.verify(k, scope, OLD, message, tag).await {
            Ok(good) => return Ok(good),
            Err(Error::NotFound(_)) if first => {
                if !move_old(k, &v, tenant).await? {
                    return Ok(false);
                }
            }
            Err(Error::NotFound(_) | Error::Forbidden(_)) => return Ok(false),
            Err(e) => return Err(unavailable(e)),
        }
    }
    Ok(false)
}

/// What `commerce_secret` holds for a workspace: its key, and when it stopped signing (now, if it has not yet).
type Row = (Vec<u8>, DateTime<Utc>);

/// Moves the workspace's old key into the vault, once: whether the vault holds it now. One
/// replica at a time: the row is locked without waiting, and one that finds it locked lets go of its connection and
/// asks again, so that no replica holds a connection while it waits for the one moving it, which needs a second one.
async fn move_old(k: &Kernel, v: &Vault, tenant: Uuid) -> Result<bool> {
    let scope = Scope::Workspace(tenant);
    let sandbox = k.outward(tenant).await.is_err();
    for _ in 0..50 {
        let mut tx = k.db.tenant_tx(tenant).await?;
        let row: std::result::Result<Option<Row>, sqlx::Error> =
            sqlx::query_as("select secret, coalesce(retired_at, now()) from commerce_secret where tenant_id = $1 for update nowait")
                .bind(tenant)
                .fetch_optional(&mut *tx)
                .await;
        let (secret, retired) = match row {
            Ok(Some(row)) => row,
            Ok(None) => return Ok(false),
            // Another replica is moving it.
            Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("55P03") => {
                drop(tx);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
            // The table is gone (a later release dropped it): nothing to move.
            Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("42P01") => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let secret = Zeroizing::new(secret);
        let held = if sandbox {
            false
        } else if v.describe(k, scope, OLD).await.map_err(unavailable)?.is_some() {
            true
        } else {
            v.set(k, scope, OLD, New::Value(secret), Rule::default()).await.map_err(unavailable)?;
            true
        };
        sqlx::query("update commerce_secret set retired_at = $2 where tenant_id = $1 and retired_at is null")
            .bind(tenant)
            .bind(retired)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(held);
    }
    Err(Error::Unavailable("Commerce's old key is being moved into the vault: try again in a moment".into()))
}

/// Upkeep: the old keys of workspaces where none has moved yet, so that every one is in the vault before the release
/// after this one removes `commerce_secret`'s rows. Nothing while the vault is closed.
pub(crate) async fn move_waiting(k: &Kernel) -> Result<()> {
    let Ok(v) = handle() else { return Ok(()) };
    let waiting: Vec<Uuid> = {
        let mut tx = k.db.system_tx().await?;
        let found = sqlx::query_scalar("select tenant_id from commerce_secret where retired_at is null order by tenant_id limit 200").fetch_all(&mut *tx).await;
        tx.commit().await?;
        match found {
            Ok(found) => found,
            Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("42P01") => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    };
    for tenant in waiting {
        match move_old(k, &v, tenant).await {
            Ok(_) => {}
            Err(Error::Unavailable(_)) => return Ok(()),
            Err(e) => tracing::warn!(%tenant, "moving Commerce's old key into the vault: {e}"),
        }
    }
    Ok(())
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
