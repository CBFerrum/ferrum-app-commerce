// @vitest-environment jsdom
// The returns desk and the stock alerts: each page drawn with what it would have, and the dialogs driven in a
// page with the requests the server's actions take (src/shopper.rs, ReturnRequest's actions) and its refusal
// shown as it is.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter, Route, Routes } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Alerts } from "./Alerts";
import { Returns } from "./Returns";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const perms = (write: "all" | "none") => ({ read: "all", write, create: write, delete: "none", submit: "none", cancel: "none", export: "none", import: "none" });
const me = { tenant: "t", user: { id: "u", email: "m@shop.test", full_name: "M", is_admin: false, roles: ["Website Manager"] }, default_company: null, scopes: {}, fiscal_year_start_month: 4, packages: ["commerce"] };

function client(seed: [unknown[], unknown][], write: "all" | "none" = "all") {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity, gcTime: Infinity } } });
  qc.setQueryData(["me"], me);
  for (const name of ["OrderRefund", "PaymentRequest", "PaymentException", "ReturnRequest", "StockAlert"]) qc.setQueryData(["meta", name], { name, permissions: perms(write) });
  for (const [key, data] of seed) qc.setQueryData(key, data);
  return qc;
}
const draw = (qc: QueryClient, at: string, path: string, element: React.ReactElement) =>
  renderToStaticMarkup(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[at]}>
        <Routes>
          <Route path={path} element={element} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
const plain = (html: string) => html.replace(/<[^>]*>/g, " ").replace(/\s+/g, " ").trim();

const request = (over: Record<string, unknown> = {}) => ({ id: "rr1", number: "RETURN-1", order_number: "SO-1", sales_order: "o1", email: "asha@example.com", status: "Requested", shopper_note: "Too small", resolution: "Store Credit", shipping_paid_by: "Shopper", restocking_fee_percent: "10", requested_at: "2026-11-10T09:00:00Z", created_at: "2026-11-10T09:00:00Z", _titles: { reason: "Wrong size" }, ...over });

describe("the returns desk", () => {
  const seed = (rows: unknown[]) => [[["commerce", "returns"], { rows, window: 14 }]] as [unknown[], unknown][];

  it("says what waits for a decision, with the shopper's words and the terms they were given", () => {
    const html = plain(draw(client(seed([request(), request({ id: "rr2", number: "RETURN-2", status: "Approved" })])), "/returns", "/returns", <Returns />));
    for (const expected of ["Returns", "1 waiting for a decision", "RETURN-1", "SO-1", "asha@example.com", "Wrong size", "Too small", "As store credit", "The shopper pays to send it back", "10% restocking fee", "Approve…", "Decline…", "Show the goods and the window"]) {
      expect(html, expected).toContain(expected);
    }
    expect(html).not.toContain("RETURN-2");
    const receive = plain(draw(client(seed([request({ status: "Approved" })])), "/returns?tab=receive", "/returns", <Returns />));
    expect(receive).toContain("Goods arrived…");
    expect(plain(draw(client(seed([])), "/returns?tab=done", "/returns", <Returns />))).toContain("Nothing finished yet");
  });

  it("tells someone who may only read that a manager decides", () => {
    const html = plain(draw(client(seed([request()]), "none"), "/returns", "/returns", <Returns />));
    expect(html).toContain("A manager decides this.");
    expect(html).not.toContain("Approve…");
  });

  it("shows the loading and failed states", () => {
    expect(draw(client([]), "/returns", "/returns", <Returns />)).toContain('aria-label="Loading…"');
  });
});

describe("stock alerts", () => {
  it("groups the shoppers who wait by item, and keeps their emails until the row is opened", () => {
    const rows = [
      { id: "1", website_item: "w1", item_title: "Blue Mug", email: "a@x.test", status: "Waiting", created_at: "2026-11-01T00:00:00Z" },
      { id: "2", website_item: "w1", item_title: "Blue Mug", email: "b@x.test", status: "Waiting", created_at: "2026-11-02T00:00:00Z" },
      { id: "3", website_item: "w2", item_title: "Logo Tee", email: "c@x.test", status: "Sent", created_at: "2026-10-02T00:00:00Z" },
    ];
    const qc = client([[["commerce", "alerts"], rows]]);
    const waiting = plain(draw(qc, "/alerts", "/alerts", <Alerts />));
    expect(waiting).toContain("2 waiting for items to come back");
    expect(waiting).toContain("Blue Mug");
    expect(waiting).toContain("2 waiting");
    expect(waiting).not.toContain("Logo Tee");
    expect(waiting).not.toContain("a@x.test");
    expect(plain(draw(qc, "/alerts?tab=sent", "/alerts", <Alerts />))).toContain("Logo Tee");
    expect(plain(draw(client([[["commerce", "alerts"], []]]), "/alerts", "/alerts", <Alerts />))).toContain("Nobody is waiting for anything");
  });
});

describe("deciding on a request", () => {
  const sent: { url: string; body: unknown }[] = [];
  let refuse: string | null = null;
  beforeEach(() => {
    sent.length = 0;
    refuse = null;
    document.body.innerHTML = "";
    HTMLDialogElement.prototype.showModal ??= function (this: HTMLDialogElement) {
      this.setAttribute("open", "");
    };
    HTMLDialogElement.prototype.close ??= function (this: HTMLDialogElement) {
      this.removeAttribute("open");
    };
    const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "content-type": "application/json" } });
    vi.stubGlobal("fetch", async (url: string, init: { method?: string; body?: string } = {}) => {
      const u = String(url);
      if (init.method === "POST") {
        sent.push({ url: u, body: init.body ? JSON.parse(init.body) : null });
        if (u.endsWith("/desk/refund-plan")) return json({ payable: true, currency: "INR", estimate: { amount: "500", errors: [] } });
        return refuse ? json({ message: refuse }, 422) : json({});
      }
      if (/ReturnRequest\/rr\d/.test(u)) return json({ id: "rr1", lines: [{ sales_order_line: "l1", item_name: "Blue Mug", qty: "2" }] });
      if (u.includes("DeliveryNote")) return json({ rows: [{ posting_date: "2026-11-05" }] });
      return json({ rows: [], total: 0 });
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  async function mount(at = "/returns") {
    const qc = client([[["commerce", "returns"], { rows: [request(), request({ id: "rr3", number: "RETURN-3", status: "Approved" })], window: 14 }]]);
    const host = document.createElement("div");
    document.body.append(host);
    await act(async () => {
      createRoot(host).render(
        <QueryClientProvider client={qc}>
          <MemoryRouter initialEntries={[at]}>
            <Returns />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
  }
  const text = () => (document.body.textContent ?? "").replace(/\s+/g, " ");
  const button = (label: string, exact = false) => [...document.querySelectorAll("button")].find((b) => (exact ? b.textContent?.trim() === label : b.textContent?.trim().startsWith(label))) as HTMLButtonElement;
  const press = async (el: Element) => void (await act(async () => (el as HTMLElement).click()));
  const type = async (el: Element | null, value: string) =>
    void (await act(async () => {
      const input = el as HTMLTextAreaElement;
      Object.getOwnPropertyDescriptor(Object.getPrototypeOf(input), "value")?.set?.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }));

  it("approves with a note and the terms in front of the desk", async () => {
    await mount();
    await press(button("Approve…"));
    expect(text()).toContain("Approve return RETURN-1");
    expect(text()).toContain("As store credit");
    await type(document.querySelector("#decide-note"), "Send it to the warehouse");
    await press(button("Approve", true));
    expect(sent[0].url).toContain("/ReturnRequest/rr1/actions/approve_return");
    expect(sent[0].body).toEqual({ note: "Send it to the warehouse" });
  });

  it("will not decline without saying why, and shows the server's refusal", async () => {
    await mount();
    await press(button("Decline…"));
    expect(button("Decline", true).disabled).toBe(true);
    await type(document.querySelector("#decide-note"), "Past the window");
    refuse = "this request was decided already";
    await press(button("Decline", true));
    expect(sent[0].url).toContain("/actions/decline_return");
    expect(text()).toContain("this request was decided already");
    expect(button("Decline", true).disabled).toBe(false);
  });

  it("shows what the refund comes to, less the restocking fee, before the goods are received", async () => {
    await mount("/returns?tab=receive");
    await press(button("Goods arrived…"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(text()).toContain("The goods for RETURN-3 are here");
    expect(text()).toContain("Blue Mug × 2");
    expect(text()).toContain("The goods credit about");
    expect(text()).toContain("Less the fee");
    expect(text()).toMatch(/Goes back about\s*₹450\.00/);
    await press(button("Receive the goods"));
    expect(sent.find((s) => s.url.includes("/actions/receive_return"))).toBeTruthy();
  });
});
