//! What Ferrum Commerce's payment gateway plugins share: amounts in a currency's minor units,
//! webhook signatures, times, and telling a refusal from a passing failure.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ferrum_plugin_sdk::HostError;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Digits after the decimal point a currency's amounts carry at payment providers.
pub fn exponent(currency: &str) -> u32 {
    match currency.to_ascii_uppercase().as_str() {
        "BIF" | "CLP" | "DJF" | "GNF" | "JPY" | "KMF" | "KRW" | "MGA" | "PYG" | "RWF" | "UGX" | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
        "BHD" | "JOD" | "KWD" | "OMR" | "TND" => 3,
        _ => 2,
    }
}

/// `"1149.88"` INR → `114988`, exactly; amounts the currency cannot carry are refused.
pub fn to_minor(amount: &str, currency: &str) -> Result<i64, String> {
    let e = exponent(currency) as usize;
    let a = amount.trim();
    let (whole, frac) = a.split_once('.').unwrap_or((a, ""));
    let frac = frac.trim_end_matches('0');
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(frac) || frac.len() > e {
        return Err(format!("{amount} {currency} is not an amount the provider can take"));
    }
    match format!("{whole}{frac:0<e$}").parse::<i64>() {
        Ok(n) if n > 0 => Ok(n),
        Ok(_) => Err("the amount must be more than zero".into()),
        Err(_) => Err(format!("{amount} is too large")),
    }
}

/// `114988` INR → `"1149.88"`.
pub fn to_major(minor: i64, currency: &str) -> String {
    let e = exponent(currency);
    if e == 0 {
        return minor.to_string();
    }
    let p = 10u64.pow(e);
    let (sign, m) = (if minor < 0 { "-" } else { "" }, minor.unsigned_abs());
    format!("{sign}{}.{:0w$}", m / p, m % p, w = e as usize)
}

/// Lower-case hex of HMAC-SHA256(`key`, `message`).
pub fn hmac_sha256_hex(key: &str, message: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC takes any key length");
    mac.update(message.as_bytes());
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// Compares secrets without telling, by timing, how much of them matched.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// An `Authorization` header for HTTP Basic.
pub fn basic(user: &str, password: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
}

/// `application/x-www-form-urlencoded` of `pairs`.
pub fn form(pairs: &[(&str, String)]) -> String {
    let enc = |s: &str| -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
                b' ' => "+".into(),
                _ => format!("%{b:02X}"),
            })
            .collect()
    };
    pairs.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

/// Seconds since 1970 of a UTC time as Ferrum writes it (`2026-10-30T10:15:00.000Z`).
pub fn unix(t: &str) -> Option<i64> {
    let (date, time) = t.trim().split_once('T')?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = time.strip_suffix('Z').or_else(|| time.strip_suffix("+00:00"))?;
    let mut parts = time.splitn(3, ':');
    let (h, mi) = (parts.next()?.parse::<i64>().ok()?, parts.next()?.parse::<i64>().ok()?);
    let s = parts.next().unwrap_or("0").split('.').next()?.parse::<i64>().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // Days from the civil date (H. Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + h * 3_600 + mi * 60 + s)
}

/// Whether a host call was refused for good (record the outcome and stop) rather than failing
/// for now (let the provider retry).
pub fn refused(e: &HostError) -> bool {
    matches!(e.code.as_str(), "validation_failed" | "invalid" | "conflict" | "forbidden" | "not_found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minor_units_are_exact() {
        assert_eq!(to_minor("1149.88", "INR"), Ok(114_988));
        assert_eq!(to_minor("1149.8", "inr"), Ok(114_980));
        assert_eq!(to_minor("1149.800000", "INR"), Ok(114_980));
        assert_eq!(to_minor("1500", "JPY"), Ok(1_500));
        assert_eq!(to_minor("2.125", "KWD"), Ok(2_125));
        assert!(to_minor("1500.5", "JPY").is_err(), "yen have no fraction");
        assert!(to_minor("1.001", "INR").is_err());
        assert!(to_minor("-5", "INR").is_err() && to_minor("0", "INR").is_err() && to_minor("1e3", "INR").is_err());
        assert_eq!(
            (to_major(114_988, "INR"), to_major(5, "INR"), to_major(1_500, "JPY"), to_major(2_125, "KWD")),
            ("1149.88".into(), "0.05".into(), "1500".into(), "2.125".into())
        );
    }

    #[test]
    fn signatures_and_times() {
        // RFC 4231 test case 2.
        assert_eq!(hmac_sha256_hex("Jefe", "what do ya want for nothing?"), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        assert!(same("abc", "abc") && !same("abc", "abd") && !same("abc", "ab"));
        assert_eq!(basic("rzp_test_x", "s3cret"), "Basic cnpwX3Rlc3RfeDpzM2NyZXQ=");
        assert_eq!(form(&[("a b", "x&y=z".into()), ("line_items[0][quantity]", "1".into())]), "a+b=x%26y%3Dz&line_items%5B0%5D%5Bquantity%5D=1");
        assert_eq!(unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix("2026-10-30T10:15:00.000Z"), Some(1_793_355_300));
        assert_eq!(unix("2000-02-29T23:59:59+00:00"), Some(951_868_799));
        assert_eq!(unix("2026-10-30T10:15:00+05:30"), None, "only UTC");
        assert_eq!(unix("yesterday"), None);
    }
}
