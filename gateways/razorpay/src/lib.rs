//! Razorpay for Ferrum Commerce. Each payment request of the `razorpay` method becomes a Razorpay
//! Payment Link whose reference is the request's id, so a repeated event finds the link it made
//! instead of making another. The shopper pays on Razorpay's page; Razorpay's signed webhook
//! confirms the payment (or reports the link expired). A schedule asks Razorpay about the links
//! still open, so a lost webhook delays a confirmation by minutes, and cancels the links of
//! requests that closed.

use ferrum_plugin_sdk::{Request, Value, data, fetch, json, kv, log, now, plugin, serde_json, warn};
use gateway_kit::{basic, hmac_sha256_hex, refused, same, to_major, to_minor, unix};

plugin!(handle);

const NAME: &str = "razorpay";
const API: &str = "https://api.razorpay.com/v1";
/// Open links checked per schedule run (a run's host calls are limited).
const PER_RUN: usize = 25;
/// Razorpay wants a link's expiry at least 15 minutes ahead.
const MIN_EXPIRY_SECS: i64 = 16 * 60;

pub fn handle(req: Request) -> Value {
    match req.kind() {
        "event" => reply(on_event(&req)),
        "http" => webhook(&req),
        "schedule" => reply(reconcile(&req)),
        _ => json!({}),
    }
}

fn reply(r: Result<(), String>) -> Value {
    match r {
        Ok(()) => json!({}),
        Err(e) => json!({"error": e}),
    }
}

fn open_key(id: &str) -> String {
    format!("open:{id}")
}

fn auth(req: &Request) -> Result<String, String> {
    let (id, secret) = (req.config("key_id").as_str().unwrap_or_default(), req.config("key_secret").as_str().unwrap_or_default());
    if id.trim().is_empty() || secret.trim().is_empty() {
        return Err("set key_id and key_secret in the razorpay plugin's settings".into());
    }
    Ok(basic(id.trim(), secret.trim()))
}

/// A call to Razorpay → its answer on success, its error description otherwise.
fn api(method: &str, path: &str, auth: &str, body: Value) -> Result<Value, String> {
    let r = fetch(method, &format!("{API}{path}"), json!({"authorization": auth, "accept": "application/json"}), body)
        .map_err(|e| format!("Razorpay cannot be reached: {e}"))?;
    let status = r["status"].as_u64().unwrap_or(0);
    let answer: Value = serde_json::from_str(r["body"].as_str().unwrap_or_default()).unwrap_or(Value::Null);
    if (200..300).contains(&status) {
        Ok(answer)
    } else {
        Err(format!("Razorpay answered HTTP {status}: {}", answer["error"]["description"].as_str().unwrap_or("no reason given")))
    }
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

/// A new payment request: its link, made (or found again) and handed to the storefront.
fn on_event(req: &Request) -> Result<(), String> {
    if req.str("topic") != "PaymentRequest.submitted" {
        return Ok(());
    }
    let event = req.get("event");
    let id = text(event, "id").to_string();
    let doc = if event["doc"].is_object() { event["doc"].clone() } else { data::get("PaymentRequest", &id).map_err(|e| e.to_string())? };
    if text(&doc, "gateway") != NAME || text(&doc, "status") != "Initiated" {
        return Ok(());
    }
    let auth = auth(req)?;
    let found = api("GET", &format!("/payment_links?reference_id={id}"), &auth, Value::Null)?;
    let link = match found["payment_links"].as_array().and_then(|l| l.first()) {
        Some(link) => link.clone(),
        None => {
            let amount = match to_minor(text(&doc, "amount"), text(&doc, "currency_code")) {
                Ok(a) => a,
                Err(why) => {
                    warn(&format!("razorpay: payment request {id} cannot be paid through Razorpay: {why}"));
                    return Ok(());
                }
            };
            api("POST", "/payment_links", &auth, link_request(&doc, &id, amount))?
        }
    };
    kv::set(&open_key(&id), json!(text(&link, "id"))).map_err(|e| e.to_string())?;
    let details = json!({"payment_url": text(&link, "short_url"), "gateway_reference": text(&link, "id")});
    match data::action("PaymentRequest", &id, "set_gateway_details", details) {
        Ok(_) => Ok(()),
        // Closed meanwhile: the schedule cancels the link.
        Err(e) if refused(&e) => {
            log(&format!("razorpay: payment request {id} closed before its link was set: {e}"));
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

fn link_request(doc: &Value, id: &str, amount: i64) -> Value {
    let order = text(doc, "order_number");
    let description: String =
        Some(text(doc, "description")).filter(|d| !d.is_empty()).map_or_else(|| format!("Order {order}"), str::to_string).chars().take(2048).collect();
    let mut customer = serde_json::Map::new();
    for (key, field) in [("name", "payer_name"), ("email", "payer_email")] {
        if !text(doc, field).is_empty() {
            customer.insert(key.into(), json!(text(doc, field)));
        }
    }
    let mut body = json!({
        "amount": amount,
        "currency": text(doc, "currency_code").to_ascii_uppercase(),
        "accept_partial": false,
        "reference_id": id,
        "description": description,
        "customer": customer,
        "notify": {"sms": false, "email": false},
        "reminder_enable": false,
        "notes": {"payment_request": id, "order": order},
    });
    if text(doc, "return_url").starts_with("https://") {
        body["callback_url"] = json!(text(doc, "return_url"));
        body["callback_method"] = json!("get");
    }
    if let (Some(expires), Some(now)) = (unix(text(doc, "expires_at")), unix(&now()))
        && expires - now >= MIN_EXPIRY_SECS
    {
        body["expire_by"] = json!(expires);
    }
    body
}

/// Books the payment; a refusal (request closed, amount changed) is final: the payment is to be
/// refunded in Razorpay.
fn settle(id: &str, minor: i64, currency: &str, payment: &str) -> Result<(), String> {
    let params = json!({"amount": to_major(minor, currency), "currency": currency.to_ascii_uppercase(), "gateway_reference": payment});
    match data::action("PaymentRequest", id, "mark_paid", params) {
        Ok(_) => {}
        Err(e) if refused(&e) => warn(&format!("razorpay: payment {payment} for request {id} was not booked ({e}); refund it in Razorpay")),
        Err(e) => return Err(format!("booking payment {payment} for request {id}: {e}")),
    }
    kv::delete(&open_key(id)).map_err(|e| e.to_string())
}

fn fail(id: &str, link: &str, reason: &str) -> Result<(), String> {
    match data::action("PaymentRequest", id, "mark_failed", json!({"reason": reason, "gateway_reference": link})) {
        Ok(_) => {}
        Err(e) if refused(&e) => log(&format!("razorpay: request {id} not marked failed: {e}")),
        Err(e) => return Err(e.to_string()),
    }
    kv::delete(&open_key(id)).map_err(|e| e.to_string())
}

/// The payment request a link was made for.
fn request_of(link: &Value) -> &str {
    Some(text(&link["notes"], "payment_request")).filter(|s| !s.is_empty()).unwrap_or_else(|| text(link, "reference_id"))
}

/// Razorpay's webhook, at `/api/public/{tenant}/razorpay/webhook`, signed with the webhook secret.
fn webhook(req: &Request) -> Value {
    let respond = |status: u16, message: &str| json!({"status": status, "json": {"message": message}});
    if req.str("path") != "/webhook" {
        return respond(404, "not found");
    }
    if req.str("method") != "POST" {
        return respond(405, "POST only");
    }
    let secret = req.config("webhook_secret").as_str().unwrap_or_default();
    if secret.is_empty() {
        return respond(503, "set webhook_secret in the razorpay plugin's settings");
    }
    let body = req.str("body");
    if !same(&hmac_sha256_hex(secret, body), req.get("headers")["x-razorpay-signature"].as_str().unwrap_or_default()) {
        return respond(401, "bad signature");
    }
    let Ok(event) = serde_json::from_str::<Value>(body) else { return respond(400, "not JSON") };
    let link = &event["payload"]["payment_link"]["entity"];
    let outcome = match text(&event, "event") {
        "payment_link.paid" => {
            let payment = &event["payload"]["payment"]["entity"];
            settle(request_of(link), payment["amount"].as_i64().unwrap_or_default(), text(payment, "currency"), text(payment, "id"))
        }
        "payment_link.expired" => fail(request_of(link), text(link, "id"), "the Razorpay payment link expired"),
        "payment_link.cancelled" => fail(request_of(link), text(link, "id"), "the Razorpay payment link was cancelled"),
        _ => Ok(()),
    };
    match outcome {
        Ok(()) => respond(200, "ok"),
        Err(e) => {
            log(&format!("razorpay webhook: {e}"));
            respond(500, "try again")
        }
    }
}

/// Every few minutes: asks Razorpay about the links still open.
fn reconcile(req: &Request) -> Result<(), String> {
    let Ok(auth) = auth(req) else { return Ok(()) };
    for entry in kv::list("open:").map_err(|e| e.to_string())?.iter().take(PER_RUN) {
        let id = text(entry, "key").trim_start_matches("open:");
        if let Err(e) = check(&auth, id, entry["value"].as_str().unwrap_or_default()) {
            log(&format!("razorpay: checking payment request {id}: {e}"));
        }
    }
    Ok(())
}

fn check(auth: &str, id: &str, link_id: &str) -> Result<(), String> {
    let status = match data::get("PaymentRequest", id) {
        Ok(doc) => text(&doc, "status").to_string(),
        Err(e) if e.code == "not_found" => "Gone".into(),
        Err(e) => return Err(e.to_string()),
    };
    let link = api("GET", &format!("/payment_links/{link_id}"), auth, Value::Null)?;
    let captured = link["payments"].as_array().and_then(|p| p.iter().find(|p| text(p, "status") == "captured")).cloned();
    match (status.as_str(), text(&link, "status")) {
        ("Initiated" | "Failed", "paid") => match captured {
            Some(p) => settle(id, p["amount"].as_i64().unwrap_or_default(), text(&link, "currency"), text(&p, "payment_id")),
            None => Ok(()),
        },
        ("Initiated" | "Failed", "expired") => fail(id, link_id, "the Razorpay payment link expired"),
        ("Initiated" | "Failed", "cancelled") => fail(id, link_id, "the Razorpay payment link was cancelled"),
        ("Initiated" | "Failed", _) => Ok(()),
        // The request is done with (paid, expired, cancelled, gone): so is its link.
        (_, link_status) => {
            if matches!(link_status, "created" | "partially_paid") {
                api("POST", &format!("/payment_links/{link_id}/cancel"), auth, json!({}))?;
            }
            if let Some(p) = captured.filter(|_| status != "Paid") {
                warn(&format!("razorpay: payment {} reached request {id} after it closed ({status}); refund it in Razorpay", text(&p, "payment_id")));
            }
            kv::delete(&open_key(id)).map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrum_plugin_sdk::testing;
    use std::cell::RefCell;
    use std::rc::Rc;

    type Calls = Rc<RefCell<Vec<(String, Value)>>>;
    type Answer = Result<Value, (String, String)>;

    const ID: &str = "01a18a00-0000-7000-8000-000000000001";

    /// A fake host recording every call; `answer` replies.
    fn host(answer: impl Fn(&str, &Value) -> Answer + 'static) -> Calls {
        let calls: Calls = Rc::default();
        let seen = calls.clone();
        testing::host(move |op, args| {
            seen.borrow_mut().push((op.to_string(), args.clone()));
            answer(op, args)
        });
        calls
    }

    fn http(status: u16, body: Value) -> Answer {
        Ok(json!({"status": status, "headers": {}, "body": body.to_string()}))
    }

    fn config() -> Value {
        json!({"key_id": "rzp_test_key", "key_secret": "secret", "webhook_secret": "whsec"})
    }

    fn submitted(gateway: &str) -> Request {
        Request(json!({"kind": "event", "topic": "PaymentRequest.submitted", "config": config(), "event": {"id": ID, "doc": {
            "id": ID, "number": "PR-0001", "gateway": gateway, "status": "Initiated", "amount": "1149.88", "currency_code": "INR",
            "order_number": "SO-0042", "payer_name": "Asha", "payer_email": "asha@example.test", "return_url": "https://shop.example.test/thanks",
            "expires_at": "2026-10-30T11:15:00.000Z"}}}))
    }

    fn ops(calls: &Calls) -> Vec<String> {
        calls.borrow().iter().map(|(op, a)| format!("{op} {}", a["method"].as_str().or(a["action"].as_str()).unwrap_or_default())).collect()
    }

    #[test]
    fn a_request_gets_a_link_once() {
        let calls = host(|op, a| match (op, a["method"].as_str(), a["url"].as_str().unwrap_or_default()) {
            ("http.fetch", Some("GET"), url) if url.contains("reference_id") => http(200, json!({"count": 0, "payment_links": []})),
            ("http.fetch", Some("POST"), _) => http(200, json!({"id": "plink_1", "short_url": "https://rzp.io/i/abc", "status": "created"})),
            ("time.now", ..) => Ok(json!("2026-10-30T10:15:00.000Z")),
            _ => Ok(Value::Null),
        });
        assert_eq!(handle(submitted("razorpay")), json!({}));
        assert_eq!(ops(&calls), ["http.fetch GET", "time.now ", "http.fetch POST", "kv.set ", "data.action set_gateway_details"]);
        let c = calls.borrow();
        assert_eq!(c[0].1["headers"]["authorization"], json!("Basic cnpwX3Rlc3Rfa2V5OnNlY3JldA=="));
        let body = &c[2].1["body"];
        assert_eq!((&body["amount"], &body["currency"], &body["reference_id"]), (&json!(114_988), &json!("INR"), &json!(ID)));
        assert_eq!((&body["callback_url"], &body["expire_by"]), (&json!("https://shop.example.test/thanks"), &json!(1_793_358_900)));
        assert_eq!(c[4].1["params"], json!({"payment_url": "https://rzp.io/i/abc", "gateway_reference": "plink_1"}));
        drop(c);

        // Delivered again: the link it made is found, not made twice.
        let calls = host(|op, a| match (op, a["method"].as_str()) {
            ("http.fetch", Some("GET")) => http(200, json!({"count": 1, "payment_links": [{"id": "plink_1", "short_url": "https://rzp.io/i/abc"}]})),
            _ => Ok(Value::Null),
        });
        handle(submitted("razorpay"));
        assert_eq!(ops(&calls), ["http.fetch GET", "kv.set ", "data.action set_gateway_details"]);
    }

    #[test]
    fn other_gateways_and_unreachable_razorpay() {
        let calls = host(|_, _| Ok(Value::Null));
        assert_eq!(handle(submitted("stripe")), json!({}));
        assert!(calls.borrow().is_empty(), "not ours");
        host(|op, _| if op == "http.fetch" { Err(("unavailable".into(), "timed out".into())) } else { Ok(Value::Null) });
        assert!(handle(submitted("razorpay"))["error"].as_str().unwrap().contains("cannot be reached"), "the event is retried");
    }

    fn webhook_request(body: &Value, secret: &str) -> Request {
        let raw = body.to_string();
        Request(json!({"kind": "http", "method": "POST", "path": "/webhook", "config": config(), "body": raw,
                       "headers": {"x-razorpay-signature": hmac_sha256_hex(secret, &raw)}}))
    }

    fn paid_event() -> Value {
        json!({"event": "payment_link.paid", "payload": {
            "payment_link": {"entity": {"id": "plink_1", "reference_id": ID, "notes": {"payment_request": ID}, "status": "paid"}},
            "payment": {"entity": {"id": "pay_9", "amount": 114_988, "currency": "INR", "status": "captured"}}}})
    }

    #[test]
    fn signed_webhooks_book_payments() {
        let calls = host(|_, _| Ok(Value::Null));
        assert_eq!(handle(webhook_request(&paid_event(), "wrong-secret"))["status"], json!(401));
        assert!(calls.borrow().is_empty());
        assert_eq!(handle(webhook_request(&paid_event(), "whsec"))["status"], json!(200));
        let c = calls.borrow();
        assert_eq!((c[0].0.as_str(), &c[0].1["action"]), ("data.action", &json!("mark_paid")));
        assert_eq!(c[0].1["params"], json!({"amount": "1149.88", "currency": "INR", "gateway_reference": "pay_9"}));
        assert_eq!((c[1].0.as_str(), &c[1].1["key"]), ("kv.delete", &json!(format!("open:{ID}"))));
        drop(c);

        // Refused for good (the request expired): answered, logged for a refund; failing for now: retried.
        let calls = host(|op, _| if op == "data.action" { Err(("invalid".into(), "this payment request has expired".into())) } else { Ok(Value::Null) });
        assert_eq!(handle(webhook_request(&paid_event(), "whsec"))["status"], json!(200));
        assert!(calls.borrow().iter().any(|(op, a)| op == "log" && a["level"] == "warn" && a["message"].as_str().unwrap().contains("refund")));
        host(|op, _| if op == "data.action" { Err(("unavailable".into(), "database pool exhausted".into())) } else { Ok(Value::Null) });
        assert_eq!(handle(webhook_request(&paid_event(), "whsec"))["status"], json!(500));

        let calls = host(|_, _| Ok(Value::Null));
        let expired = json!({"event": "payment_link.expired", "payload": {"payment_link": {"entity": {"id": "plink_1", "reference_id": ID}}}});
        assert_eq!(handle(webhook_request(&expired, "whsec"))["status"], json!(200));
        assert_eq!(calls.borrow()[0].1["action"], json!("mark_failed"));
    }

    #[test]
    fn the_schedule_settles_missed_payments_and_closes_stale_links() {
        let calls = host(|op, a| match op {
            "kv.list" => Ok(json!([{"key": format!("open:{ID}"), "value": "plink_1"}, {"key": "open:closed-one", "value": "plink_2"}])),
            "data.get" if a["id"] == json!(ID) => Ok(json!({"status": "Initiated"})),
            "data.get" => Ok(json!({"status": "Expired"})),
            "http.fetch" if a["url"].as_str().unwrap().ends_with("plink_1") => http(
                200,
                json!({"id": "plink_1", "status": "paid", "currency": "INR", "payments": [{"payment_id": "pay_9", "amount": 114_988, "status": "captured"}]}),
            ),
            "http.fetch" if a["url"].as_str().unwrap().ends_with("plink_2") => http(200, json!({"id": "plink_2", "status": "created"})),
            "http.fetch" => http(200, json!({"id": "plink_2", "status": "cancelled"})),
            _ => Ok(Value::Null),
        });
        assert_eq!(handle(Request(json!({"kind": "schedule", "name": "reconcile", "config": config()}))), json!({}));
        let c = calls.borrow();
        let paid = c.iter().find(|(op, a)| op == "data.action" && a["action"] == "mark_paid").expect("the missed payment is booked");
        assert_eq!(paid.1["params"]["amount"], json!("1149.88"));
        assert!(c.iter().any(|(op, a)| op == "http.fetch" && a["url"].as_str().unwrap().ends_with("plink_2/cancel")), "a closed request's link is cancelled");
        assert_eq!(c.iter().filter(|(op, _)| op == "kv.delete").count(), 2);
    }
}
