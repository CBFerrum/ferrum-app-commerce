// @vitest-environment jsdom
// The app's real route table, mounted, on a workspace with no Store Settings (as a fresh install: the walk's) and one with them.
// Without them the store's screens say the store is not set up and lead to setting it up, and ask the server nothing it
// would refuse; with them the day's sales are asked for and shown. Records (new and existing) of an entity with a screen of
// its own and of ones without must leave the loading state. Answers in src/fixtures are copied from a running server
// (GET /api/meta, /api/meta/<Entity>, /api/auth/me, and the 422 the sales report gave a workspace with no default company).

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setFrameMode } from "@ferrum/records/frame";
import index from "./fixtures/meta-index.json";
import me from "./fixtures/me.json";
import refused from "./fixtures/web-sales-422.json";
import { COMMERCE_PACKAGES, routes } from "./routes";

const ENTITIES = ["WebsiteItem", "CouponCode", "StoreSettings", "SalesOrder"];
const metas = import.meta.glob("./fixtures/meta-*.json", { eager: true, import: "default" }) as Record<string, unknown>;
const answers: Record<string, unknown> = { "/api/auth/me": me, "/api/meta": index };
for (const e of ENTITIES) {
  answers[`/api/meta/${e}`] = metas[`./fixtures/meta-${e}.json`];
  answers[`/api/v1/${e}/r1`] = { id: "r1", version: 1, docstatus: 0, title: "r1" };
}
const today = new Date().toISOString().slice(0, 10);
const asked: string[] = [];
let settings: object[] = [];

beforeEach(() => {
  asked.length = 0;
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  (globalThis as { matchMedia?: unknown }).matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const path = new URL(String(input instanceof Request ? input.url : input), "http://localhost").pathname;
      asked.push(path);
      const json = (b: unknown, status = 200) => new Response(JSON.stringify(b), { status, headers: { "content-type": "application/json" } });
      if (path === "/api/v1/StoreSettings") return json({ rows: settings, total: settings.length });
      // What the server did with no company to go on, and what it does once Store Settings name one.
      if (path === "/api/reports/web_sales") return settings.length ? json({ rows: [{ day: today, orders: 2, revenue: "1500" }] }) : json(refused, 422);
      const body = answers[path] ?? (path.startsWith("/api/v1/") ? { rows: [], total: 0 } : undefined);
      return json(body ?? { error: "not found" }, body === undefined ? 404 : 200);
    }),
  );
  setFrameMode("app", COMMERCE_PACKAGES);
});
afterEach(() => vi.unstubAllGlobals());

async function open(path: string) {
  const host = document.createElement("div");
  document.body.append(host);
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createMemoryRouter(routes, { initialEntries: [path] });
  const root = createRoot(host);
  await act(async () => {
    root.render(
      <QueryClientProvider client={qc}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );
  });
  for (let i = 0; i < 40; i++) await act(async () => void (await new Promise((r) => setTimeout(r, 25))));
  const text = host.textContent ?? "";
  const link = host.querySelector('a[href="/e/StoreSettings/new"]');
  await act(async () => root.unmount());
  host.remove();
  return { text, link: !!link };
}

describe("a workspace with no Store Settings", () => {
  for (const path of ["/", "/orders", "/returns", "/alerts", "/variants", "/words", "/money"]) {
    it(`${path} says the store is not set up, leads to setting it up, and shows no failed request`, async () => {
      settings = [];
      const { text, link } = await open(path);
      expect(text).toContain("The store is not set up yet");
      expect(link).toBe(true);
      expect(text).not.toMatch(/is required|Please correct|cannot be shown/i);
      expect(asked).not.toContain("/api/reports/web_sales");
    });
  }

  it("still opens the Store Settings form and the store's other records", async () => {
    settings = [];
    for (const path of ["/e/StoreSettings/new", "/e/WebsiteItem/new", "/e/CouponCode/new"]) {
      const { text } = await open(path);
      expect(text).not.toContain("The store is not set up yet");
      expect(text).not.toMatch(/Loading|Opening (the|a|an) /);
    }
  });
});

describe("a workspace with Store Settings", () => {
  it("shows the day's sales on the Today page, the company being the store's", async () => {
    settings = [{ id: "s1" }];
    const { text } = await open("/");
    expect(text).not.toContain("The store is not set up yet");
    expect(text).not.toMatch(/Sales cannot be shown/);
    expect(text).toContain("1,500");
    // The page names no company: the server takes the store's.
    expect(asked).toContain("/api/reports/web_sales");
  });

  it("says a refusal as a plain sentence, without the validation wording", async () => {
    settings = [{ id: "s1" }];
    // The server refuses the report as it did the walk's workspace.
    answers["/api/reports/web_sales"] = undefined;
    const real = globalThis.fetch;
    vi.stubGlobal("fetch", async (i: RequestInfo | URL, o?: RequestInit) => {
      const path = new URL(String(i instanceof Request ? i.url : i), "http://localhost").pathname;
      return path === "/api/reports/web_sales" ? new Response(JSON.stringify(refused), { status: 422, headers: { "content-type": "application/json" } }) : real(i, o);
    });
    const { text } = await open("/");
    expect(text).toContain("Sales cannot be shown (is required)");
    expect(text).not.toContain("Please correct the highlighted fields");
  });
});

describe("Commerce's records as mounted", () => {
  for (const path of ["/e/WebsiteItem/new", "/e/WebsiteItem/r1", "/e/CouponCode/new", "/e/StoreSettings/r1", "/o/r1"]) {
    it(`${path} leaves the loading state`, async () => {
      settings = [{ id: "s1" }];
      const { text } = await open(path);
      expect(text.trim().length).toBeGreaterThan(0);
      expect(text).not.toMatch(/Loading|Opening (the|a|an) /);
    });
  }
});
