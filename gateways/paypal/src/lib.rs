//! PayPal for Ferrum Commerce. Each payment request of the `paypal` method becomes a PayPal order
//! (created with the request's id as PayPal's request id, so a repeated event gets the same order
//! back). The buyer approves it on PayPal's page; the order is then captured — on PayPal's webhook
//! for the approval, or by the schedule that asks PayPal about open orders — and the capture books
//! the payment. Webhooks are checked with PayPal's own verification call before anything is done.
//!
//! Money taken that cannot be booked becomes a Payment Exception, and the refund a person approves
//! there goes out from the same schedule. The client secret is held by the host; the short-lived
//! access token it buys is what this plugin keeps.

use ferrum_plugin_sdk::{Request, Value, data, fetch_with, json, kv, log, plugin, serde_json, warn};
use gateway_kit::{Refund, final_status, refunded_part, refunded_payment, refused, report_exception, run_refunds, to_minor};

plugin!(handle);

const NAME: &str = "paypal";
/// Open orders checked per schedule run (a run's host calls are limited).
const PER_RUN: usize = 20;
/// PayPal descriptions hold 127 characters.
const DESCRIPTION_MAX: usize = 127;

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

fn api_base(req: &Request) -> &'static str {
    if req.config("environment").as_str() == Some("sandbox") { "https://api-m.sandbox.paypal.com" } else { "https://api-m.paypal.com" }
}

/// A call to PayPal's REST API → its answer on success, its HTTP status and message otherwise.
fn call(req: &Request, method: &str, path: &str, body: Value, request_id: Option<&str>) -> Result<Value, (u64, String)> {
    // The host holds the access token as well as the secret that buys it (`oauth2` in the manifest).
    let mut headers = json!({"accept": "application/json", "prefer": "return=representation"});
    if let Some(id) = request_id {
        headers["paypal-request-id"] = json!(id);
    }
    let r =
        fetch_with("client_secret", method, &format!("{}{path}", api_base(req)), headers, body).map_err(|e| (0, format!("PayPal cannot be reached: {e}")))?;
    let status = r["status"].as_u64().unwrap_or(0);
    let answer: Value = serde_json::from_str(r["body"].as_str().unwrap_or_default()).unwrap_or(Value::Null);
    if (200..300).contains(&status) {
        Ok(answer)
    } else {
        let issue = answer["details"][0]["issue"].as_str().unwrap_or_default();
        Err((status, format!("PayPal answered HTTP {status}: {} {issue}", answer["message"].as_str().unwrap_or("no reason given"))))
    }
}

fn api(req: &Request, method: &str, path: &str, body: Value, request_id: Option<&str>) -> Result<Value, String> {
    call(req, method, path, body, request_id).map_err(|(_, e)| e)
}

/// A new payment request: its PayPal order, handed to the storefront.
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
    let back = text(&doc, "return_url");
    // PayPal takes amounts as decimal text; the currency must be able to carry them.
    if !back.starts_with("https://") || to_minor(text(&doc, "amount"), text(&doc, "currency_code")).is_err() {
        warn(&format!("paypal: payment request {id} cannot be paid through PayPal: it needs an https return URL and an amount the currency can carry"));
        return Ok(());
    }
    let order = api(req, "POST", "/v2/checkout/orders", order_request(&doc, &id, back), Some(&format!("ferrum-payment-request-{id}")))?;
    let link =
        order["links"].as_array().and_then(|l| l.iter().find(|l| matches!(text(l, "rel"), "payer-action" | "approve"))).map(|l| text(l, "href").to_string());
    let Some(link) = link else { return Err(format!("PayPal's order {} has no approval link", text(&order, "id"))) };
    kv::set(&open_key(&id), json!(text(&order, "id"))).map_err(|e| e.to_string())?;
    match data::action("PaymentRequest", &id, "set_gateway_details", json!({"payment_url": link, "gateway_reference": text(&order, "id")})) {
        Ok(_) => Ok(()),
        // Closed meanwhile: the schedule forgets the order (PayPal lets unapproved orders lapse).
        Err(e) if refused(&e) => {
            log(&format!("paypal: payment request {id} closed before its order was set: {e}"));
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

fn order_request(doc: &Value, id: &str, back: &str) -> Value {
    let order = text(doc, "order_number");
    let description: String = Some(text(doc, "description"))
        .filter(|d| !d.is_empty())
        .map_or_else(|| format!("Order {order}"), str::to_string)
        .chars()
        .take(DESCRIPTION_MAX)
        .collect();
    json!({
        "intent": "CAPTURE",
        "purchase_units": [{
            "reference_id": id,
            "custom_id": id,
            "invoice_id": order,
            "description": description,
            "amount": {"currency_code": text(doc, "currency_code").to_ascii_uppercase(), "value": text(doc, "amount")},
        }],
        "payment_source": {"paypal": {"experience_context": {"return_url": back, "cancel_url": back, "user_action": "PAY_NOW", "shipping_preference": "NO_SHIPPING"}}},
    })
}

/// The payment request an order or capture was made for.
fn request_of(resource: &Value) -> &str {
    let unit = &resource["purchase_units"][0];
    [text(resource, "custom_id"), text(unit, "custom_id"), text(unit, "reference_id")].into_iter().find(|s| !s.is_empty()).unwrap_or_default()
}

/// Books a completed capture; a refusal (request closed, amount changed) is final, and the money is
/// reported for a person to decide on.
fn settle(id: &str, capture: &Value) -> Result<(), String> {
    let params =
        json!({"amount": text(&capture["amount"], "value"), "currency": text(&capture["amount"], "currency_code"), "gateway_reference": text(capture, "id")});
    match data::action("PaymentRequest", id, "mark_paid", params) {
        Ok(_) => {}
        Err(e) if refused(&e) => report(id, capture, &format!("it could not be booked: {}", e.message))?,
        Err(e) => return Err(format!("booking capture {} for request {id}: {e}", text(capture, "id"))),
    }
    kv::delete(&open_key(id)).map_err(|e| e.to_string())
}

fn report(id: &str, capture: &Value, reason: &str) -> Result<(), String> {
    report_exception(NAME, id, text(capture, "id"), text(&capture["amount"], "value"), text(&capture["amount"], "currency_code"), reason)
}

fn fail(id: &str, reference: &str, reason: &str) -> Result<(), String> {
    match data::action("PaymentRequest", id, "mark_failed", json!({"reason": reason, "gateway_reference": reference})) {
        Ok(_) => {}
        Err(e) if refused(&e) => log(&format!("paypal: request {id} not marked failed: {e}")),
        Err(e) => return Err(e.to_string()),
    }
    kv::delete(&open_key(id)).map_err(|e| e.to_string())
}

/// Captures an approved order (once, whatever the retries) and books the capture.
fn capture(req: &Request, id: &str, order_id: &str) -> Result<(), String> {
    let order = api(req, "POST", &format!("/v2/checkout/orders/{order_id}/capture"), json!({}), Some(&format!("ferrum-capture-{order_id}")))?;
    let captured = &order["purchase_units"][0]["payments"]["captures"][0];
    match text(captured, "status") {
        "COMPLETED" => settle(id, captured),
        // Held for review or an echeck: PAYMENT.CAPTURE.COMPLETED follows.
        "PENDING" => Ok(()),
        other => fail(id, order_id, &format!("PayPal did not capture the payment ({other})")),
    }
}

/// PayPal's webhook, at `/api/public/{tenant}/paypal/webhook`, verified with PayPal before use.
fn webhook(req: &Request) -> Value {
    let respond = |status: u16, message: &str| json!({"status": status, "json": {"message": message}});
    if req.str("path") != "/webhook" {
        return respond(404, "not found");
    }
    if req.str("method") != "POST" {
        return respond(405, "POST only");
    }
    let webhook_id = req.config("webhook_id").as_str().unwrap_or_default().trim();
    if webhook_id.is_empty() {
        return respond(503, "set webhook_id in the paypal plugin's settings");
    }
    // PayPal refuses every verification for an ID of another shape: say so instead.
    if !webhook_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        warn("paypal webhook: webhook_id in the plugin's settings is not a PayPal webhook ID (letters and digits only)");
        return respond(503, "webhook_id in the paypal plugin's settings is not a PayPal webhook ID");
    }
    let Ok(event) = serde_json::from_str::<Value>(req.str("body")) else { return respond(400, "not JSON") };
    let h = req.get("headers");
    let check = json!({
        "auth_algo": h["paypal-auth-algo"], "cert_url": h["paypal-cert-url"], "transmission_id": h["paypal-transmission-id"],
        "transmission_sig": h["paypal-transmission-sig"], "transmission_time": h["paypal-transmission-time"], "webhook_id": webhook_id, "webhook_event": event,
    });
    match api(req, "POST", "/v1/notifications/verify-webhook-signature", check, None) {
        Ok(v) if text(&v, "verification_status") == "SUCCESS" => {}
        Ok(_) => return respond(401, "bad signature"),
        Err(e) => {
            log(&format!("paypal webhook: cannot verify: {e}"));
            return respond(500, "try again");
        }
    }
    let resource = &event["resource"];
    let outcome = match text(&event, "event_type") {
        "CHECKOUT.ORDER.APPROVED" => capture(req, request_of(resource), text(resource, "id")),
        "PAYMENT.CAPTURE.COMPLETED" => settle(request_of(resource), resource),
        "PAYMENT.CAPTURE.DENIED" | "PAYMENT.CAPTURE.DECLINED" => fail(request_of(resource), text(resource, "id"), "PayPal declined the payment"),
        _ => Ok(()),
    };
    match outcome {
        Ok(()) => respond(200, "ok"),
        Err(e) => {
            log(&format!("paypal webhook: {e}"));
            respond(500, "try again")
        }
    }
}

/// Every few minutes: asks PayPal about the orders still open, and carries out approved refunds.
fn reconcile(req: &Request) -> Result<(), String> {
    if req.config("client_id").as_str().unwrap_or_default().trim().is_empty() {
        return Ok(());
    }
    for entry in kv::list("open:").map_err(|e| e.to_string())?.iter().take(PER_RUN) {
        let id = text(entry, "key").trim_start_matches("open:");
        if let Err(e) = check(req, id, entry["value"].as_str().unwrap_or_default()) {
            log(&format!("paypal: checking payment request {id}: {e}"));
        }
    }
    run_refunds(NAME, |exception| refund(req, exception))
}

/// Refunds a capture in full. The exception's id is PayPal's request id, so repeating it refunds once.
fn refund(req: &Request, row: &Value) -> Refund {
    let (id, capture) = (text(row, "id"), refunded_payment(row));
    let body = match refunded_part(row) {
        Ok(Some((amount, _))) => json!({"amount": {"value": amount, "currency_code": text(row, "currency_code")}}),
        Ok(None) => json!({}),
        Err(e) => return Refund::Refused(e),
    };
    match call(req, "POST", &format!("/v2/payments/captures/{capture}/refund"), body, Some(&format!("ferrum-refund-{id}"))) {
        Ok(r) => Refund::Done(text(&r, "id").to_string()),
        Err((status, e)) if final_status(status) => Refund::Refused(e),
        Err((_, e)) => Refund::Later(e),
    }
}

fn check(req: &Request, id: &str, order_id: &str) -> Result<(), String> {
    let status = match data::get("PaymentRequest", id) {
        Ok(doc) => text(&doc, "status").to_string(),
        Err(e) if e.code == "not_found" => "Gone".into(),
        Err(e) => return Err(e.to_string()),
    };
    let order = api(req, "GET", &format!("/v2/checkout/orders/{order_id}"), Value::Null, None)?;
    let open = matches!(status.as_str(), "Initiated" | "Failed");
    match (open, text(&order, "status")) {
        (true, "APPROVED") => capture(req, id, order_id),
        (true, "COMPLETED") => {
            match order["purchase_units"][0]["payments"]["captures"].as_array().and_then(|c| c.iter().find(|c| text(c, "status") == "COMPLETED")) {
                Some(c) => settle(id, c),
                None => Ok(()),
            }
        }
        (true, "VOIDED") => fail(id, order_id, "the PayPal order was voided"),
        (true, _) => Ok(()),
        // The request is done with: an order approved after it closed is not captured (nothing to
        // refund); one already completed is.
        (false, order_status) => {
            if order_status == "COMPLETED" && status != "Paid" {
                let captures = order["purchase_units"][0]["payments"]["captures"].as_array();
                match captures.and_then(|c| c.iter().find(|c| text(c, "status") == "COMPLETED")) {
                    Some(c) => report(id, c, &format!("it was paid after the request closed ({status})"))?,
                    None => warn(&format!("paypal: order {order_id} was paid after request {id} closed ({status}), and shows no capture to refund")),
                }
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

    const ID: &str = "01a18a00-0000-7000-8000-000000000003";

    fn host(answer: impl Fn(&str, &Value) -> Answer + 'static) -> Calls {
        let calls: Calls = Rc::default();
        let seen = calls.clone();
        testing::host(move |op, args| {
            seen.borrow_mut().push((op.to_string(), args.clone()));
            match op {
                "time.now" => Ok(json!("2026-10-30T10:15:00.000Z")),
                _ => answer(op, args),
            }
        });
        calls
    }

    fn http(status: u16, body: Value) -> Answer {
        Ok(json!({"status": status, "headers": {}, "body": body.to_string()}))
    }

    fn config() -> Value {
        json!({"client_id": "client-1", "webhook_id": "8PT597110X687430LKGECATA", "environment": "sandbox"})
    }

    fn url(a: &Value) -> &str {
        a["url"].as_str().unwrap_or_default()
    }

    #[test]
    fn a_request_gets_an_order() {
        let calls = host(|op, _| match op {
            "http.fetch" => http(
                201,
                json!({"id": "5O190127TN364715T", "status": "PAYER_ACTION_REQUIRED",
                "links": [{"rel": "self", "href": "https://api-m.sandbox.paypal.com/v2/checkout/orders/5O1"}, {"rel": "payer-action", "href": "https://www.sandbox.paypal.com/checkoutnow?token=5O1"}]}),
            ),
            _ => Ok(Value::Null),
        });
        let req = Request(json!({"kind": "event", "topic": "PaymentRequest.submitted", "config": config(), "event": {"id": ID, "doc": {
            "id": ID, "gateway": "paypal", "status": "Initiated", "amount": "49.90", "currency_code": "USD", "order_number": "SO-9", "return_url": "https://shop.example.test/done"}}}));
        assert_eq!(handle(req), json!({}));
        let c = calls.borrow();
        let create = c.iter().find(|(op, _)| op == "http.fetch").unwrap();
        assert_eq!(url(&create.1), "https://api-m.sandbox.paypal.com/v2/checkout/orders");
        assert_eq!((&create.1["auth"], &create.1["headers"]["paypal-request-id"]), (&json!("client_secret"), &json!(format!("ferrum-payment-request-{ID}"))));
        let unit = &create.1["body"]["purchase_units"][0];
        assert_eq!((&unit["custom_id"], &unit["amount"]), (&json!(ID), &json!({"currency_code": "USD", "value": "49.90"})));
        let set = c.iter().find(|(op, _)| op == "data.action").unwrap();
        assert_eq!(set.1["params"], json!({"payment_url": "https://www.sandbox.paypal.com/checkoutnow?token=5O1", "gateway_reference": "5O190127TN364715T"}));
    }

    fn webhook_request(event: Value) -> Request {
        Request(json!({"kind": "http", "method": "POST", "path": "/webhook", "config": config(), "body": event.to_string(),
            "headers": {"paypal-auth-algo": "SHA256withRSA", "paypal-cert-url": "https://api.paypal.com/cert", "paypal-transmission-id": "t1",
                        "paypal-transmission-sig": "sig", "paypal-transmission-time": "2026-10-30T10:14:00Z"}}))
    }

    #[test]
    fn a_webhook_id_of_another_shape_is_reported_not_sent() {
        let calls = host(|op, a| match op {
            "http.fetch" if url(a).ends_with("verify-webhook-signature") => http(200, json!({"verification_status": "SUCCESS"})),
            _ => Ok(Value::Null),
        });
        let denied = json!({"event_type": "PAYMENT.CAPTURE.DENIED", "resource": {"id": "C1", "custom_id": ID}});
        let mut wrong = webhook_request(denied.clone());
        wrong.0["config"]["webhook_id"] = json!("WH-1");
        assert_eq!(handle(wrong)["status"], json!(503));
        assert!(!calls.borrow().iter().any(|(op, _)| op == "http.fetch"), "nothing sent to PayPal");
        let mut pasted = webhook_request(denied);
        pasted.0["config"]["webhook_id"] = json!(" 8PT597110X687430LKGECATA\n");
        assert_eq!(handle(pasted)["status"], json!(200));
        let c = calls.borrow();
        let verify = c.iter().find(|(op, a)| op == "http.fetch" && url(a).ends_with("verify-webhook-signature")).unwrap();
        assert_eq!(verify.1["body"]["webhook_id"], json!("8PT597110X687430LKGECATA"), "pasted with spaces: trimmed");
    }

    #[test]
    fn an_approved_order_is_captured_and_booked_once_verified() {
        let approved = json!({"event_type": "CHECKOUT.ORDER.APPROVED", "resource": {"id": "5O1", "status": "APPROVED", "purchase_units": [{"custom_id": ID, "reference_id": ID}]}});
        // Not verified: nothing happens.
        let calls = host(|op, a| match op {
            "http.fetch" if url(a).ends_with("verify-webhook-signature") => http(200, json!({"verification_status": "FAILURE"})),
            _ => Ok(Value::Null),
        });
        assert_eq!(handle(webhook_request(approved.clone()))["status"], json!(401));
        assert!(!calls.borrow().iter().any(|(op, _)| op == "data.action"));

        let calls = host(|op, a| match op {
            "http.fetch" if url(a).ends_with("verify-webhook-signature") => http(200, json!({"verification_status": "SUCCESS"})),
            "http.fetch" if url(a).ends_with("/capture") => http(
                201,
                json!({"id": "5O1", "status": "COMPLETED", "purchase_units": [{"payments": {"captures": [
                {"id": "3C679366HH908993F", "status": "COMPLETED", "amount": {"currency_code": "USD", "value": "49.90"}}]}}]}),
            ),
            _ => Ok(Value::Null),
        });
        assert_eq!(handle(webhook_request(approved))["status"], json!(200));
        let c = calls.borrow();
        let verify = c.iter().find(|(op, a)| op == "http.fetch" && url(a).ends_with("verify-webhook-signature")).unwrap();
        assert_eq!((&verify.1["body"]["webhook_id"], &verify.1["body"]["transmission_id"]), (&json!("8PT597110X687430LKGECATA"), &json!("t1")));
        let capture = c.iter().find(|(op, a)| op == "http.fetch" && url(a).ends_with("/capture")).unwrap();
        assert_eq!(capture.1["headers"]["paypal-request-id"], json!("ferrum-capture-5O1"), "one capture, however often PayPal tells");
        let paid = c.iter().find(|(op, _)| op == "data.action").unwrap();
        assert_eq!(
            (&paid.1["action"], &paid.1["params"]),
            (&json!("mark_paid"), &json!({"amount": "49.90", "currency": "USD", "gateway_reference": "3C679366HH908993F"}))
        );
    }

    #[test]
    fn the_schedule_captures_what_a_lost_webhook_left() {
        let calls = host(|op, a| match op {
            "kv.list" => Ok(json!([{"key": format!("open:{ID}"), "value": "5O1"}, {"key": "open:gone", "value": "5O2"}])),
            "data.get" if a["id"] == json!(ID) => Ok(json!({"status": "Initiated"})),
            "data.get" => Err(("not_found".into(), "payment request".into())),
            "http.fetch" if url(a).ends_with("/orders/5O1") => http(200, json!({"id": "5O1", "status": "APPROVED"})),
            "http.fetch" if url(a).ends_with("/orders/5O2") => http(200, json!({"id": "5O2", "status": "CREATED"})),
            "http.fetch" if url(a).ends_with("/capture") => http(
                201,
                json!({"purchase_units": [{"payments": {"captures": [
                {"id": "CAP-1", "status": "COMPLETED", "amount": {"currency_code": "USD", "value": "49.90"}}]}}]}),
            ),
            _ => Ok(Value::Null),
        });
        handle(Request(json!({"kind": "schedule", "name": "reconcile", "config": config()})));
        let c = calls.borrow();
        assert!(c.iter().any(|(op, a)| op == "data.action" && a["action"] == "mark_paid" && a["params"]["gateway_reference"] == "CAP-1"));
        assert_eq!(c.iter().filter(|(op, _)| op == "kv.delete").count(), 2, "booked, and forgotten for the request that is gone");
    }

    #[test]
    fn the_host_signs_every_call_and_this_plugin_holds_no_token() {
        let calls = host(|op, _| match op {
            "http.fetch" => http(200, json!({"id": "ORDER-1", "status": "CREATED"})),
            _ => Ok(Value::Null),
        });
        let req = Request(json!({"kind": "schedule", "config": config()}));
        call(&req, "GET", "/v2/checkout/orders/ORDER-1", Value::Null, None).unwrap();
        let c = calls.borrow();
        let sent = c.iter().find(|(op, _)| op == "http.fetch").unwrap();
        assert_eq!(sent.1["auth"], json!("client_secret"), "the host signs the call in");
        assert_eq!(sent.1["headers"].get("authorization"), None);
        assert!(!c.iter().any(|(op, _)| op == "kv.set"), "nothing kept here");
    }

    #[test]
    fn money_that_cannot_be_booked_is_reported_and_an_approved_refund_goes_out_once() {
        let calls = host(|op, a| match op {
            "data.action" if a["action"] == "mark_paid" => Err(("invalid".into(), "this payment request has expired".into())),
            _ => Ok(Value::Null),
        });
        settle(ID, &json!({"id": "CAP-9", "amount": {"currency_code": "USD", "value": "49.90"}})).unwrap();
        let c = calls.borrow();
        let report = c.iter().find(|(op, a)| op == "data.action" && a["action"] == "report_exception").expect("a person is told");
        assert_eq!((&report.1["params"]["gateway_reference"], &report.1["params"]["amount"]), (&json!("CAP-9"), &json!("49.90")));
        drop(c);

        let calls = host(|op, a| match op {
            "http.fetch" if url(a).ends_with("/captures/CAP-9/refund") => http(201, json!({"id": "RF-1"})),
            "http.fetch" if url(a).ends_with("/captures/CAP-0/refund") => http(422, json!({"message": "already refunded"})),
            "http.fetch" => http(500, json!({})),
            _ => Ok(Value::Null),
        });
        let req = Request(json!({"kind": "schedule", "config": config()}));
        assert_eq!(refund(&req, &json!({"id": "x-1", "gateway_reference": "CAP-9"})), Refund::Done("RF-1".into()));
        assert!(matches!(refund(&req, &json!({"id": "x-2", "gateway_reference": "CAP-0"})), Refund::Refused(_)));
        assert!(matches!(refund(&req, &json!({"id": "x-3", "gateway_reference": "CAP-5"})), Refund::Later(_)));
        assert_eq!(calls.borrow().iter().find(|(op, _)| op == "http.fetch").unwrap().1["headers"]["paypal-request-id"], json!("ferrum-refund-x-1"));
    }

    #[test]
    fn an_order_refund_gives_back_the_part_asked_for() {
        let calls = host(|op, _| match op {
            "http.fetch" => http(201, json!({"id": "RF-2"})),
            _ => Ok(Value::Null),
        });
        let req = Request(json!({"kind": "schedule", "config": config()}));
        let row = json!({"id": "r-1", "payment_reference": "CAP-7", "amount": "12.50", "currency_code": "USD"});
        assert_eq!(refund(&req, &row), Refund::Done("RF-2".into()));
        let c = calls.borrow();
        let sent = c.iter().find(|(op, _)| op == "http.fetch").unwrap();
        assert!(url(&sent.1).ends_with("/captures/CAP-7/refund"));
        assert_eq!(sent.1["body"], json!({"amount": {"value": "12.50", "currency_code": "USD"}}));
    }

    /// Against PayPal's sandbox when PAYPAL_SANDBOX_CLIENT_ID and PAYPAL_SANDBOX_CLIENT_SECRET are
    /// set: a real access token and order with its approval link (unapproved orders lapse).
    #[test]
    fn paypal_sandbox() {
        use gateway_kit::sandbox;
        let (Some(client), Some(secret)) = (sandbox::key("PAYPAL_SANDBOX_CLIENT_ID"), sandbox::key("PAYPAL_SANDBOX_CLIENT_SECRET")) else {
            return eprintln!("skipped: set PAYPAL_SANDBOX_CLIENT_ID and PAYPAL_SANDBOX_CLIENT_SECRET to test against PayPal's sandbox");
        };
        let calls = sandbox::host();
        sandbox::client_credentials("client_secret", "https://api-m.sandbox.paypal.com/v1/oauth2/token", &client, &secret);
        let id = sandbox::fresh_id();
        let req = Request(json!({"kind": "event", "topic": "PaymentRequest.submitted",
            "config": {"client_id": client, "webhook_id": "", "environment": "sandbox"},
            "event": {"id": id, "doc": {"id": id, "gateway": "paypal", "status": "Initiated", "amount": "49.90", "currency_code": "USD",
                                        "order_number": "SO-SANDBOX", "return_url": "https://example.com/orders/done"}}}));
        assert_eq!(handle(req), json!({}));
        let details = sandbox::gateway_details(&calls);
        assert!(details["payment_url"].as_str().unwrap().starts_with("https://www.sandbox.paypal.com/"), "{details}");
    }

    /// Against PayPal's own API definitions (paypal/paypal-rest-api-specifications) served by a
    /// validating mock when PAYPAL_ORDERS_MOCK and PAYPAL_WEBHOOKS_MOCK are set (CI starts Prism with
    /// them): the order, its capture, a look at it and a webhook's verification must each be a
    /// request the definitions accept. The definitions leave OAuth out, so any token will do.
    #[test]
    fn paypal_definitions_accept_every_call() {
        use gateway_kit::sandbox;
        let (Some(orders), Some(webhooks)) = (sandbox::key("PAYPAL_ORDERS_MOCK"), sandbox::key("PAYPAL_WEBHOOKS_MOCK")) else {
            return eprintln!("skipped: set PAYPAL_ORDERS_MOCK and PAYPAL_WEBHOOKS_MOCK to check the calls against PayPal's API definitions");
        };
        const BASE: &str = "https://api-m.sandbox.paypal.com";
        sandbox::sign_in_as("client_secret", "Bearer A21-mock");
        let calls: Calls = Rc::default();
        let seen = calls.clone();
        testing::host(move |op, args| {
            seen.borrow_mut().push((op.to_string(), args.clone()));
            match op {
                "http.fetch" => {
                    let path = url(args).strip_prefix(BASE).unwrap_or_default();
                    let mock = match path {
                        p if p.starts_with("/v2/checkout/") => &orders,
                        p if p.starts_with("/v1/notifications/") => &webhooks,
                        _ => return Err(("unexpected".into(), format!("no PayPal definition covers {}", url(args)))),
                    };
                    let mut sent = args.clone();
                    sent["url"] = json!(format!("{}{path}", mock.trim_end_matches('/')));
                    sandbox::fetch(&sent)
                }
                "data.get" => Ok(json!({"status": "Initiated"})),
                "time.now" => Ok(json!(sandbox::utc_in(0))),
                _ => Ok(Value::Null),
            }
        });
        let submitted = || {
            Request(json!({"kind": "event", "topic": "PaymentRequest.submitted", "config": config(), "event": {"id": ID, "doc": {
                "id": ID, "gateway": "paypal", "status": "Initiated", "amount": "49.90", "currency_code": "USD", "order_number": "SO-9",
                "return_url": "https://shop.example.test/done"}}}))
        };
        assert_eq!(handle(submitted()), json!({}));
        let details = sandbox::gateway_details(&calls);
        let order = details["gateway_reference"].as_str().unwrap().to_string();
        assert!(!order.is_empty() && details["payment_url"].as_str().unwrap().starts_with("https://"), "{details}");
        capture(&submitted(), ID, &order).unwrap();
        check(&submitted(), ID, &order).unwrap();
        let approved = json!({"id": "WH-1", "event_type": "CHECKOUT.ORDER.APPROVED", "resource": {"id": order, "status": "APPROVED",
            "purchase_units": [{"custom_id": ID, "reference_id": ID}]}});
        assert_ne!(handle(webhook_request(approved))["status"], json!(500), "a webhook call was refused");
        let sent: Vec<String> = calls
            .borrow()
            .iter()
            .filter(|(op, _)| op == "http.fetch")
            .map(|(_, a)| format!("{} {}", a["method"].as_str().unwrap_or_default(), url(a).trim_start_matches(BASE)))
            .collect();
        for call in [
            "POST /v2/checkout/orders".to_string(),
            format!("POST /v2/checkout/orders/{order}/capture"),
            format!("GET /v2/checkout/orders/{order}"),
            "POST /v1/notifications/verify-webhook-signature".to_string(),
        ] {
            assert!(sent.contains(&call), "{call} was not checked: {sent:?}");
        }
    }
}
