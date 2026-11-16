// Commerce's own interface, the store's admin (Ferrum docs/plan/apps-ui.md §13, U5) at /commerce/:
// the day on its home, the web-order desk at /orders (what waits for money, to be sent, to be
// invoiced) with each order whole at /o/{id}, the money that needs a person at /money (refunds, and
// payments a gateway took and could not book), and the store's records (catalog, categories,
// coupons, shipping, carts, payment requests) as lists and forms in the records frame, with its
// module in the sidebar under the platform's bar.

import { mountApp, setFrameMode } from "@ferrum/records/frame";
import { COMMERCE_PACKAGES, routes } from "./routes";
import "./styles.css";

setFrameMode("app", COMMERCE_PACKAGES);

mountApp({
  route: "commerce",
  catalogs: { ar: () => import("./locales/ar.json"), hi: () => import("./locales/hi.json") },
  routes,
});
