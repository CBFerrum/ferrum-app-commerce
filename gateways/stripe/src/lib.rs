//! Stripe for Ferrum Commerce. Each payment request of the `stripe` method becomes a Stripe
//! Checkout Session, created with the request's id as the idempotency key, so a repeated event gets
//! the same session back. The shopper pays on Stripe's page; Stripe's signed webhook confirms the
//! payment, including methods that settle later, or reports it failed or expired. A schedule asks
//! Stripe about the sessions still open, so a lost webhook delays a confirmation by minutes, and
//! expires the sessions of requests that closed.
//!
//! Money taken that cannot be booked becomes a Payment Exception, and the refund a person approves
//! there goes out from the same schedule. The API key and the webhook secret are held by the host:
//! this plugin names them and never reads them.

use ferrum_plugin_sdk::{HostError, Request, Value, data, fetch_with, json, kv, log, now, plugin, serde_json, verify_hmac_sha256, warn};
use gateway_kit::{Refund, final_status, form, refused, report_exception, run_refunds, to_major, to_minor, unix};

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
        "schedule" => reply(reconcile()),
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

/// A call to Stripe (form-encoded) → its answer on success, its HTTP status and message otherwise.
fn call(method: &str, path: &str, body: Option<String>, idempotency: Option<&str>) -> Result<Value, (u64, String)> {
    let mut headers = json!({"accept": "application/json"});
    if body.is_some() {
        headers["content-type"] = json!("application/x-www-form-urlencoded");
    }
    if let Some(k) = idempotency {
        headers["idempotency-key"] = json!(k);
    }
    let r = fetch_with("secret_key", method, &format!("{API}{path}"), headers, body.map_or(Value::Null, Value::String))
        .map_err(|e| (0, format!("Stripe cannot be reached: {e}")))?;
    let status = r["status"].as_u64().unwrap_or(0);
    let answer: Value = serde_json::from_str(r["body"].as_str().unwrap_or_default()).unwrap_or(Value::Null);
    if (200..300).contains(&status) {
        Ok(answer)
    } else {
        Err((status, format!("Stripe answered HTTP {status}: {}", answer["error"]["message"].as_str().unwrap_or("no reason given"))))
    }
}

fn api(method: &str, path: &str, body: Option<String>, idempotency: Option<&str>) -> Result<Value, String> {
    call(method, path, body, idempotency).map_err(|(_, e)| e)
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
    let back = Some(text(&doc, "return_url"))
        .filter(|u| u.starts_with("https://"))
        .or_else(|| req.config("success_url").as_str().filter(|u| u.starts_with("https://")));
    let (Some(back), Ok(amount)) = (back, to_minor(text(&doc, "amount"), text(&doc, "currency_code"))) else {
        warn(&format!(
            "stripe: payment request {id} cannot be paid through Stripe: it needs an https return URL (or success_url in the plugin's settings) and an amount the currency can carry"
        ));
        return Ok(());
    };
    let session = api("POST", "/checkout/sessions", Some(session_form(&doc, &id, amount, back)), Some(&format!("ferrum-payment-request-{id}")))?;
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

/// What a session took: the payment's reference, its amount and its currency.
fn taken(session: &Value) -> (&str, String, String) {
    let currency = text(session, "currency").to_ascii_uppercase();
    let payment = Some(text(session, "payment_intent")).filter(|p| !p.is_empty()).unwrap_or_else(|| text(session, "id"));
    (payment, to_major(session["amount_total"].as_i64().unwrap_or_default(), &currency), currency)
}

/// Books a paid session; a refusal (request closed, amount changed) is final, and the money is
/// reported for a person to decide on.
fn settle(session: &Value) -> Result<(), String> {
    let id = request_of(session);
    let (payment, amount, currency) = taken(session);
    let params = json!({"amount": amount, "currency": currency, "gateway_reference": payment});
    match data::action("PaymentRequest", id, "mark_paid", params) {
        Ok(_) => {}
        Err(e) if refused(&e) => report_exception(NAME, id, payment, &amount, &currency, &format!("it could not be booked: {}", e.message))?,
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

/// Whether `header` (`t=…,v1=…[,v1=…]`) signs `body` with the webhook secret, within the tolerance
/// of `now`.
fn signed(header: &str, body: &str, now: i64) -> Result<bool, HostError> {
    let (mut t, mut sigs) = (None, Vec::new());
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => t = v.parse::<i64>().ok(),
            Some(("v1", v)) => sigs.push(v),
            _ => {}
        }
    }
    let Some(t) = t.filter(|t| (now - t).abs() <= TOLERANCE_SECS) else { return Ok(false) };
    let message = format!("{t}.{body}");
    for sig in sigs {
        if verify_hmac_sha256("webhook_secret", &message, sig)? {
            return Ok(true);
        }
    }
    Ok(false)
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
    let body = req.str("body");
    let Some(now) = unix(&now()) else { return respond(503, "no clock") };
    match signed(req.get("headers")["stripe-signature"].as_str().unwrap_or_default(), body, now) {
        Ok(true) => {}
        Ok(false) => return respond(401, "bad or stale signature"),
        // Unset, or the host cannot say: Stripe tries again.
        Err(e) => {
            log(&format!("stripe webhook: {e}"));
            return respond(503, "set webhook_secret in the stripe plugin's settings");
        }
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

/// Every few minutes: asks Stripe about the sessions still open, and carries out approved refunds.
fn reconcile() -> Result<(), String> {
    for entry in kv::list("open:").map_err(|e| e.to_string())?.iter().take(PER_RUN) {
        let id = text(entry, "key").trim_start_matches("open:");
        if let Err(e) = check(id, entry["value"].as_str().unwrap_or_default()) {
            log(&format!("stripe: checking payment request {id}: {e}"));
        }
    }
    run_refunds(NAME, refund)
}

/// Refunds a payment in full. The exception's id is the idempotency key, so repeating it refunds once.
fn refund(exception: &Value) -> Refund {
    let (id, payment) = (text(exception, "id"), text(exception, "gateway_reference"));
    if !payment.starts_with("pi_") {
        return Refund::Refused(format!("{payment} is not a payment Stripe can refund"));
    }
    let body = form(&[("payment_intent", payment.to_string()), ("metadata[ferrum_exception]", id.to_string())]);
    match call("POST", "/refunds", Some(body), Some(&format!("ferrum-refund-{id}"))) {
        Ok(r) => Refund::Done(text(&r, "id").to_string()),
        Err((status, e)) if final_status(status) => Refund::Refused(e),
        Err((_, e)) => Refund::Later(e),
    }
}

fn check(id: &str, session_id: &str) -> Result<(), String> {
    let status = match data::get("PaymentRequest", id) {
        Ok(doc) => text(&doc, "status").to_string(),
        Err(e) if e.code == "not_found" => "Gone".into(),
        Err(e) => return Err(e.to_string()),
    };
    let session = api("GET", &format!("/checkout/sessions/{session_id}"), None, None)?;
    let paid = text(&session, "payment_status") == "paid";
    match (status.as_str(), text(&session, "status")) {
        ("Initiated" | "Failed", _) if paid => settle(&session),
        ("Initiated" | "Failed", "expired") => fail(&session, "the Stripe checkout expired"),
        ("Initiated" | "Failed", _) => Ok(()),
        // The request is done with (paid, expired, cancelled, gone): so is its session.
        (_, session_status) => {
            if session_status == "open" {
                api("POST", &format!("/checkout/sessions/{session_id}/expire"), Some(String::new()), None)?;
            }
            if paid && status != "Paid" {
                let (payment, amount, currency) = taken(&session);
                report_exception(NAME, id, payment, &amount, &currency, &format!("it was paid after the request closed ({status})"))?;
            }
            kv::delete(&open_key(id)).map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrum_plugin_sdk::testing;
    use gateway_kit::{hmac_sha256_hex, same};
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
            match op {
                "time.now" => Ok(json!(NOW)),
                "crypto.verify_hmac_sha256" => {
                    assert_eq!(args["credential"], json!("webhook_secret"));
                    Ok(json!({"valid": same(&hmac_sha256_hex("whsec_abc", args["message"].as_str().unwrap()), args["signature"].as_str().unwrap())}))
                }
                _ => answer(op, args),
            }
        });
        calls
    }

    fn http(status: u16, body: Value) -> Answer {
        Ok(json!({"status": status, "headers": {}, "body": body.to_string()}))
    }

    fn config() -> Value {
        json!({"success_url": ""})
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
        assert_eq!(fetch["auth"], json!("secret_key"), "the host signs the call in");
        assert_eq!(fetch["headers"].get("authorization"), None, "with a key this plugin never holds");
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

    #[test]
    fn money_that_cannot_be_booked_is_reported_not_just_logged() {
        let calls = host(|op, a| match op {
            "data.action" if a["action"] == "mark_paid" => Err(("invalid".into(), "this payment request has expired".into())),
            _ => Ok(Value::Null),
        });
        let event = completed("paid");
        assert_eq!(handle(webhook_request(&event, signature(&event, NOW_UNIX, "whsec_abc")))["status"], json!(200));
        let c = calls.borrow();
        let report = c.iter().find(|(op, a)| op == "data.action" && a["action"] == "report_exception").expect("a person is told");
        assert_eq!((&report.1["id"], &report.1["params"]["gateway_reference"], &report.1["params"]["amount"]), (&json!(ID), &json!("pi_42"), &json!("49.90")));
        assert!(report.1["params"]["reason"].as_str().unwrap().contains("expired"));
        assert_eq!(c.iter().filter(|(op, _)| op == "kv.delete").count(), 1, "and the session is done with");
        drop(c);

        // Told it cannot be reported for now: Stripe sends the webhook again.
        host(|op, _| if op == "data.action" { Err(("unavailable".into(), "database pool exhausted".into())) } else { Ok(Value::Null) });
        assert_eq!(handle(webhook_request(&event, signature(&event, NOW_UNIX, "whsec_abc")))["status"], json!(500));
    }

    #[test]
    fn a_session_paid_after_its_request_closed_is_reported_too() {
        let calls = host(|op, _| match op {
            "data.get" => Ok(json!({"status": "Cancelled"})),
            "http.fetch" => http(
                200,
                json!({"id": "cs_late", "status": "complete", "payment_status": "paid", "client_reference_id": ID,
                                             "amount_total": 4990, "currency": "usd", "payment_intent": "pi_late"}),
            ),
            _ => Ok(Value::Null),
        });
        check(ID, "cs_late").unwrap();
        let c = calls.borrow();
        let report = c.iter().find(|(op, a)| op == "data.action" && a["action"] == "report_exception").expect("reported");
        assert_eq!(report.1["params"]["gateway_reference"], json!("pi_late"));
    }

    #[test]
    fn approved_refunds_go_out_once_and_report_back() {
        let exception = |id: &str, payment: &str| json!({"id": id, "gateway_reference": payment, "gateway": "stripe", "status": "Refund Approved"});
        let due = json!({"rows": [exception("x-1", "pi_ok"), exception("x-2", "pi_no"), exception("x-3", "pi_later"), exception("x-4", "cs_never_paid")]});
        let calls = host(move |op, a| match op {
            "kv.list" => Ok(json!([])),
            "data.query" => {
                assert_eq!(a["filters"], json!({"gateway": "stripe", "status": "Refund Approved"}), "only what a person approved");
                Ok(due.clone())
            }
            "http.fetch" if a["body"].as_str().unwrap().contains("pi_ok") => http(200, json!({"id": "re_1"})),
            "http.fetch" if a["body"].as_str().unwrap().contains("pi_no") => http(400, json!({"error": {"message": "already refunded"}})),
            "http.fetch" => http(503, json!({})),
            _ => Ok(Value::Null),
        });
        assert_eq!(handle(Request(json!({"kind": "schedule", "name": "reconcile", "config": config()}))), json!({}));
        let c = calls.borrow();
        let told: Vec<(String, String)> =
            c.iter().filter(|(op, _)| op == "data.action").map(|(_, a)| (a["id"].as_str().unwrap().into(), a["action"].as_str().unwrap().into())).collect();
        let expect = |id: &str, action: &str| (id.to_string(), action.to_string());
        assert_eq!(told, [expect("x-1", "refund_done"), expect("x-2", "refund_failed"), expect("x-4", "refund_failed")], "x-3 waits for the next run");
        let sent = c.iter().filter(|(op, _)| op == "http.fetch").collect::<Vec<_>>();
        assert_eq!(sent.len(), 3, "a session with no payment behind it is not sent");
        assert_eq!(sent[0].1["headers"]["idempotency-key"], json!("ferrum-refund-x-1"), "repeating it refunds once");
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
        sandbox::sign_in_as("secret_key", format!("Bearer {key}"));
        let mut req = submitted("https://example.com/orders/done");
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
        sandbox::sign_in_as("secret_key", "Bearer sk_test_123");
        assert_eq!(handle(submitted("https://example.com/orders/done")), json!({}));
        let details = sandbox::gateway_details(&calls);
        let session = details["gateway_reference"].as_str().unwrap().to_string();
        assert!(session.starts_with("cs_") && details["payment_url"].as_str().unwrap().starts_with("https://checkout.stripe.com/"), "{details}");
        check(ID, &session).unwrap();
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
        assert!(matches!(refund(&json!({"id": ID, "gateway_reference": "pi_123"})), Refund::Done(_)), "a refund is a call Stripe accepts");
    }
}
