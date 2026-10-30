//! Stripe for Ferrum Commerce. Each payment request of the `stripe` method becomes a Stripe
//! Checkout Session, created with the request's id as the idempotency key, so a repeated event gets
//! the same session back. The shopper pays on Stripe's page; Stripe's signed webhook confirms the
//! payment, including methods that settle later, or reports it failed or expired. A schedule asks
//! Stripe about the sessions still open, so a lost webhook delays a confirmation by minutes, and
//! expires the sessions of requests that closed.

use ferrum_plugin_sdk::{Request, Value, data, fetch, json, kv, log, now, plugin, serde_json, warn};
use gateway_kit::{form, hmac_sha256_hex, refused, same, to_major, to_minor, unix};

plugin!(handle);

const NAME: &str = "stripe";
const API: &str = "https://api.stripe.com/v1";
/// Open sessions checked per schedule run (a run's host calls are limited).
const PER_RUN: usize = 25;
/// How old a signed webhook may be (Stripe's own default).
const TOLERANCE_SECS: i64 = 300;

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

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

fn secret_key(req: &Request) -> Result<String, String> {
    let key = req.config("secret_key").as_str().unwrap_or_default().trim();
    if key.is_empty() {
        return Err("set secret_key in the stripe plugin's settings".into());
    }
    Ok(key.to_string())
}

/// A call to Stripe (form-encoded) → its answer on success, its error message otherwise.
fn api(method: &str, path: &str, key: &str, body: Option<String>, idempotency: Option<&str>) -> Result<Value, String> {
    let mut headers = json!({"authorization": format!("Bearer {key}"), "accept": "application/json"});
    if body.is_some() {
        headers["content-type"] = json!("application/x-www-form-urlencoded");
    }
    if let Some(k) = idempotency {
        headers["idempotency-key"] = json!(k);
    }
    let r = fetch(method, &format!("{API}{path}"), headers, body.map_or(Value::Null, Value::String)).map_err(|e| format!("Stripe cannot be reached: {e}"))?;
    let status = r["status"].as_u64().unwrap_or(0);
    let answer: Value = serde_json::from_str(r["body"].as_str().unwrap_or_default()).unwrap_or(Value::Null);
    if (200..300).contains(&status) {
        Ok(answer)
    } else {
        Err(format!("Stripe answered HTTP {status}: {}", answer["error"]["message"].as_str().unwrap_or("no reason given")))
    }
}

/// A new payment request: its checkout session, handed to the storefront.
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
    let key = secret_key(req)?;
    let back = Some(text(&doc, "return_url"))
        .filter(|u| u.starts_with("https://"))
        .or_else(|| req.config("success_url").as_str().filter(|u| u.starts_with("https://")));
    let (Some(back), Ok(amount)) = (back, to_minor(text(&doc, "amount"), text(&doc, "currency_code"))) else {
        warn(&format!(
            "stripe: payment request {id} cannot be paid through Stripe: it needs an https return URL (or success_url in the plugin's settings) and an amount the currency can carry"
        ));
        return Ok(());
    };
    let session = api("POST", "/checkout/sessions", &key, Some(session_form(&doc, &id, amount, back)), Some(&format!("ferrum-payment-request-{id}")))?;
    kv::set(&open_key(&id), json!(text(&session, "id"))).map_err(|e| e.to_string())?;
    let details = json!({"payment_url": text(&session, "url"), "gateway_reference": text(&session, "id")});
    match data::action("PaymentRequest", &id, "set_gateway_details", details) {
        Ok(_) => Ok(()),
        // Closed meanwhile: the schedule expires the session.
        Err(e) if refused(&e) => {
            log(&format!("stripe: payment request {id} closed before its session was set: {e}"));
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// The session's parameters: nothing that changes between deliveries of the same event, so the
/// idempotency key returns the same session.
fn session_form(doc: &Value, id: &str, amount: i64, back: &str) -> String {
    let order = text(doc, "order_number");
    let name: String =
        Some(text(doc, "description")).filter(|d| !d.is_empty()).map_or_else(|| format!("Order {order}"), str::to_string).chars().take(250).collect();
    let mut pairs = vec![
        ("mode", "payment".to_string()),
        ("client_reference_id", id.to_string()),
        ("line_items[0][quantity]", "1".into()),
        ("line_items[0][price_data][currency]", text(doc, "currency_code").to_ascii_lowercase()),
        ("line_items[0][price_data][unit_amount]", amount.to_string()),
        ("line_items[0][price_data][product_data][name]", name),
        ("success_url", back.to_string()),
        ("cancel_url", back.to_string()),
        ("metadata[payment_request]", id.to_string()),
        ("metadata[order]", order.to_string()),
        ("payment_intent_data[metadata][payment_request]", id.to_string()),
    ];
    if !text(doc, "payer_email").is_empty() {
        pairs.push(("customer_email", text(doc, "payer_email").to_string()));
    }
    form(&pairs)
}

/// The payment request a session was made for.
fn request_of(session: &Value) -> &str {
    Some(text(session, "client_reference_id")).filter(|s| !s.is_empty()).unwrap_or_else(|| text(&session["metadata"], "payment_request"))
}

/// Books a paid session; a refusal (request closed, amount changed) is final: the payment is to
/// be refunded in Stripe.
fn settle(session: &Value) -> Result<(), String> {
    let id = request_of(session);
    let currency = text(session, "currency").to_ascii_uppercase();
    let payment = Some(text(session, "payment_intent")).filter(|p| !p.is_empty()).unwrap_or_else(|| text(session, "id"));
    let params =
        json!({"amount": to_major(session["amount_total"].as_i64().unwrap_or_default(), &currency), "currency": currency, "gateway_reference": payment});
    match data::action("PaymentRequest", id, "mark_paid", params) {
        Ok(_) => {}
        Err(e) if refused(&e) => warn(&format!("stripe: payment {payment} for request {id} was not booked ({e}); refund it in Stripe")),
        Err(e) => return Err(format!("booking payment {payment} for request {id}: {e}")),
    }
    kv::delete(&open_key(id)).map_err(|e| e.to_string())
}

fn fail(session: &Value, reason: &str) -> Result<(), String> {
    let id = request_of(session);
    match data::action("PaymentRequest", id, "mark_failed", json!({"reason": reason, "gateway_reference": text(session, "id")})) {
        Ok(_) => {}
        Err(e) if refused(&e) => log(&format!("stripe: request {id} not marked failed: {e}")),
        Err(e) => return Err(e.to_string()),
    }
    kv::delete(&open_key(id)).map_err(|e| e.to_string())
}

/// Whether `header` (`t=…,v1=…[,v1=…]`) signs `body` with `secret`, within the tolerance of `now`.
fn signed(header: &str, body: &str, secret: &str, now: i64) -> bool {
    let (mut t, mut sigs) = (None, Vec::new());
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => t = v.parse::<i64>().ok(),
            Some(("v1", v)) => sigs.push(v),
            _ => {}
        }
    }
    let Some(t) = t else { return false };
    let expected = hmac_sha256_hex(secret, &format!("{t}.{body}"));
    (now - t).abs() <= TOLERANCE_SECS && sigs.iter().any(|s| same(s, &expected))
}

/// Stripe's webhook, at `/api/public/{tenant}/stripe/webhook`, signed with the endpoint's secret.
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
        return respond(503, "set webhook_secret in the stripe plugin's settings");
    }
    let body = req.str("body");
    let Some(now) = unix(&now()) else { return respond(503, "no clock") };
    if !signed(req.get("headers")["stripe-signature"].as_str().unwrap_or_default(), body, secret, now) {
        return respond(401, "bad or stale signature");
    }
    let Ok(event) = serde_json::from_str::<Value>(body) else { return respond(400, "not JSON") };
    let session = &event["data"]["object"];
    let outcome = match text(&event, "type") {
        // Card payments are paid at completion; bank debits and vouchers settle later.
        "checkout.session.completed" if text(session, "payment_status") == "paid" => settle(session),
        "checkout.session.async_payment_succeeded" => settle(session),
        "checkout.session.async_payment_failed" => fail(session, "the payment failed at Stripe"),
        "checkout.session.expired" => fail(session, "the Stripe checkout expired"),
        _ => Ok(()),
    };
    match outcome {
        Ok(()) => respond(200, "ok"),
        Err(e) => {
            log(&format!("stripe webhook: {e}"));
            respond(500, "try again")
        }
    }
}

/// Every few minutes: asks Stripe about the sessions still open.
fn reconcile(req: &Request) -> Result<(), String> {
    let Ok(key) = secret_key(req) else { return Ok(()) };
    for entry in kv::list("open:").map_err(|e| e.to_string())?.iter().take(PER_RUN) {
        let id = text(entry, "key").trim_start_matches("open:");
        if let Err(e) = check(&key, id, entry["value"].as_str().unwrap_or_default()) {
            log(&format!("stripe: checking payment request {id}: {e}"));
        }
    }
    Ok(())
}

fn check(key: &str, id: &str, session_id: &str) -> Result<(), String> {
    let status = match data::get("PaymentRequest", id) {
        Ok(doc) => text(&doc, "status").to_string(),
        Err(e) if e.code == "not_found" => "Gone".into(),
        Err(e) => return Err(e.to_string()),
    };
    let session = api("GET", &format!("/checkout/sessions/{session_id}"), key, None, None)?;
    let paid = text(&session, "payment_status") == "paid";
    match (status.as_str(), text(&session, "status")) {
        ("Initiated" | "Failed", _) if paid => settle(&session),
        ("Initiated" | "Failed", "expired") => fail(&session, "the Stripe checkout expired"),
        ("Initiated" | "Failed", _) => Ok(()),
        // The request is done with (paid, expired, cancelled, gone): so is its session.
        (_, session_status) => {
            if session_status == "open" {
                api("POST", &format!("/checkout/sessions/{session_id}/expire"), key, Some(String::new()), None)?;
            }
            if paid && status != "Paid" {
                warn(&format!("stripe: session {session_id} was paid after request {id} closed ({status}); refund it in Stripe"));
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

    const ID: &str = "01a18a00-0000-7000-8000-000000000002";
    const NOW: &str = "2026-10-30T10:15:00.000Z";
    const NOW_UNIX: i64 = 1_793_355_300;

    fn host(answer: impl Fn(&str, &Value) -> Answer + 'static) -> Calls {
        let calls: Calls = Rc::default();
        let seen = calls.clone();
        testing::host(move |op, args| {
            seen.borrow_mut().push((op.to_string(), args.clone()));
            if op == "time.now" {
                return Ok(json!(NOW));
            }
            answer(op, args)
        });
        calls
    }

    fn http(status: u16, body: Value) -> Answer {
        Ok(json!({"status": status, "headers": {}, "body": body.to_string()}))
    }

    fn config() -> Value {
        json!({"secret_key": "sk_test_123", "webhook_secret": "whsec_abc", "success_url": ""})
    }

    fn submitted(return_url: &str) -> Request {
        Request(json!({"kind": "event", "topic": "PaymentRequest.submitted", "config": config(), "event": {"id": ID, "doc": {
            "id": ID, "gateway": "stripe", "status": "Initiated", "amount": "49.9", "currency_code": "USD", "order_number": "SO-7",
            "payer_email": "sam@example.test", "return_url": return_url}}}))
    }

    #[test]
    fn a_request_gets_a_session() {
        let calls = host(|op, _| match op {
            "http.fetch" => http(200, json!({"id": "cs_test_1", "url": "https://checkout.stripe.com/c/pay/cs_test_1"})),
            _ => Ok(Value::Null),
        });
        assert_eq!(handle(submitted("https://shop.example.test/done")), json!({}));
        let c = calls.borrow();
        let fetch = &c[0].1;
        assert_eq!((&fetch["method"], &fetch["url"]), (&json!("POST"), &json!("https://api.stripe.com/v1/checkout/sessions")));
        assert_eq!(fetch["headers"]["idempotency-key"], json!(format!("ferrum-payment-request-{ID}")));
        assert_eq!(fetch["headers"]["authorization"], json!("Bearer sk_test_123"));
        let body = fetch["body"].as_str().unwrap();
        for part in [
            "line_items%5B0%5D%5Bprice_data%5D%5Bunit_amount%5D=4990",
            "line_items%5B0%5D%5Bprice_data%5D%5Bcurrency%5D=usd",
            &format!("client_reference_id={ID}"),
            "success_url=https%3A%2F%2Fshop.example.test%2Fdone",
            "customer_email=sam%40example.test",
        ] {
            assert!(body.contains(part), "{part} in {body}");
        }
        assert_eq!(c[2].1["params"], json!({"payment_url": "https://checkout.stripe.com/c/pay/cs_test_1", "gateway_reference": "cs_test_1"}));
        drop(c);

        // No https page to come back to: refused with a log line, not retried.
        let calls = host(|_, _| Ok(Value::Null));
        assert_eq!(handle(submitted("http://shop.local/done")), json!({}));
        assert_eq!(calls.borrow().iter().map(|(op, _)| op.as_str()).collect::<Vec<_>>(), ["log"]);
    }

    fn webhook_request(event: &Value, header: String) -> Request {
        Request(json!({"kind": "http", "method": "POST", "path": "/webhook", "config": config(), "body": event.to_string(),
                       "headers": {"stripe-signature": header}}))
    }

    fn signature(event: &Value, t: i64, secret: &str) -> String {
        format!("t={t},v1={}", hmac_sha256_hex(secret, &format!("{t}.{event}")))
    }

    fn completed(payment_status: &str) -> Value {
        json!({"type": "checkout.session.completed", "data": {"object": {"id": "cs_test_1", "client_reference_id": ID, "payment_status": payment_status,
               "amount_total": 4990, "currency": "usd", "payment_intent": "pi_42"}}})
    }

    #[test]
    fn signed_fresh_webhooks_book_payments() {
        let calls = host(|_, _| Ok(Value::Null));
        let event = completed("paid");
        assert_eq!(handle(webhook_request(&event, signature(&event, NOW_UNIX, "whsec_wrong")))["status"], json!(401));
        assert_eq!(handle(webhook_request(&event, signature(&event, NOW_UNIX - 3600, "whsec_abc")))["status"], json!(401), "stale");
        assert!(!calls.borrow().iter().any(|(op, _)| op == "data.action"));
        assert_eq!(handle(webhook_request(&event, format!("v1=bogus,{}", signature(&event, NOW_UNIX - 10, "whsec_abc"))))["status"], json!(200));
        let c = calls.borrow();
        let paid = c.iter().find(|(op, _)| op == "data.action").unwrap();
        assert_eq!(paid.1["params"], json!({"amount": "49.90", "currency": "USD", "gateway_reference": "pi_42"}));
        drop(c);

        // A bank debit completes unpaid: nothing booked until it succeeds.
        let calls = host(|_, _| Ok(Value::Null));
        let pending = completed("unpaid");
        assert_eq!(handle(webhook_request(&pending, signature(&pending, NOW_UNIX, "whsec_abc")))["status"], json!(200));
        assert!(!calls.borrow().iter().any(|(op, _)| op == "data.action"));
        let failed = json!({"type": "checkout.session.async_payment_failed", "data": {"object": {"id": "cs_test_1", "client_reference_id": ID}}});
        handle(webhook_request(&failed, signature(&failed, NOW_UNIX, "whsec_abc")));
        assert!(calls.borrow().iter().any(|(op, a)| op == "data.action" && a["action"] == "mark_failed"));
    }

    #[test]
    fn the_schedule_settles_and_expires() {
        let calls = host(|op, a| match op {
            "kv.list" => Ok(json!([{"key": format!("open:{ID}"), "value": "cs_paid"}, {"key": "open:other", "value": "cs_open"}])),
            "data.get" if a["id"] == json!(ID) => Ok(json!({"status": "Initiated"})),
            "data.get" => Ok(json!({"status": "Cancelled"})),
            "http.fetch" if a["url"].as_str().unwrap().ends_with("cs_paid") => http(
                200,
                json!({"id": "cs_paid", "status": "complete", "payment_status": "paid",
                "client_reference_id": ID, "amount_total": 4990, "currency": "usd", "payment_intent": "pi_42"}),
            ),
            "http.fetch" if a["url"].as_str().unwrap().ends_with("cs_open") => {
                http(200, json!({"id": "cs_open", "status": "open", "payment_status": "unpaid"}))
            }
            "http.fetch" => http(200, json!({"id": "cs_open", "status": "expired"})),
            _ => Ok(Value::Null),
        });
        handle(Request(json!({"kind": "schedule", "name": "reconcile", "config": config()})));
        let c = calls.borrow();
        assert!(c.iter().any(|(op, a)| op == "data.action" && a["action"] == "mark_paid"));
        assert!(c.iter().any(|(op, a)| op == "http.fetch" && a["url"].as_str().unwrap().ends_with("cs_open/expire")));
        assert_eq!(c.iter().filter(|(op, _)| op == "kv.delete").count(), 2);
    }

    /// Against Stripe's test mode when STRIPE_TEST_SECRET_KEY (`sk_test_…`) is set: a real
    /// Checkout Session for a payment request, expired again afterwards.
    #[test]
    fn stripe_test_mode() {
        use gateway_kit::sandbox;
        let Some(key) = sandbox::key("STRIPE_TEST_SECRET_KEY") else {
            return eprintln!("skipped: set STRIPE_TEST_SECRET_KEY to test against Stripe's test mode");
        };
        assert!(key.starts_with("sk_test_") || key.starts_with("rk_test_"), "test-mode keys only");
        let calls = sandbox::host();
        let id = sandbox::fresh_id();
        let mut req = submitted("https://example.com/orders/done");
        req.0["config"]["secret_key"] = json!(key);
        req.0["event"]["id"] = json!(id);
        req.0["event"]["doc"]["id"] = json!(id);
        assert_eq!(handle(req), json!({}));
        let details = sandbox::gateway_details(&calls);
        assert!(details["payment_url"].as_str().unwrap().starts_with("https://checkout.stripe.com/"), "{details}");
        let session = details["gateway_reference"].as_str().unwrap();
        let expired = sandbox::fetch(&json!({"method": "POST", "url": format!("{API}/checkout/sessions/{session}/expire"),
            "headers": {"authorization": format!("Bearer {key}")}, "body": ""}))
        .unwrap();
        assert_eq!(expired["status"], json!(200), "{expired}");
    }

    /// Against stripe-mock when STRIPE_MOCK_URL is set (CI starts it): Stripe's own mock, built from
    /// its API definition, refuses unknown or malformed parameters as Stripe does. The session is
    /// made, then looked up and expired as the schedule does for a request that is done with.
    #[test]
    fn stripe_mock_accepts_every_call() {
        use gateway_kit::sandbox;
        let Some(mock) = sandbox::key("STRIPE_MOCK_URL") else { return eprintln!("skipped: set STRIPE_MOCK_URL to check the calls against stripe-mock") };
        let calls = sandbox::host_via("https://api.stripe.com", mock.trim_end_matches('/'));
        assert_eq!(handle(submitted("https://example.com/orders/done")), json!({}));
        let details = sandbox::gateway_details(&calls);
        let session = details["gateway_reference"].as_str().unwrap().to_string();
        assert!(session.starts_with("cs_") && details["payment_url"].as_str().unwrap().starts_with("https://checkout.stripe.com/"), "{details}");
        check("sk_test_123", ID, &session).unwrap();
        let sent: Vec<String> = calls
            .borrow()
            .iter()
            .filter(|(op, _)| op == "http.fetch")
            .map(|(_, a)| format!("{} {}", a["method"].as_str().unwrap(), a["url"].as_str().unwrap()))
            .collect();
        assert_eq!(
            sent,
            [
                format!("POST {API}/checkout/sessions"),
                format!("GET {API}/checkout/sessions/{session}"),
                format!("POST {API}/checkout/sessions/{session}/expire")
            ]
        );
    }
}
