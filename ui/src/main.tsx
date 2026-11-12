// Commerce's own interface, the store's admin (Ferrum docs/plan/apps-ui.md §13, U5) at /commerce/:
// the day on its home, the web-order desk at /orders (what waits for money, to be sent, to be
// invoiced) with each order whole at /o/{id}, the money that needs a person at /money (refunds, and
// payments a gateway took and could not book), and the store's records (catalog, categories,
// coupons, shipping, carts, payment requests) as lists and forms in the records frame, with its
// module in the sidebar under the platform's bar.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createBrowserRouter, RouterProvider } from "react-router";
import { addCatalogs, routeBase, wordsReady } from "@ferrum/client";
import { ConfirmHost, Toaster } from "@ferrum/kit";
import { setFrameMode } from "@ferrum/records/frame";
import { COMMERCE_PACKAGES, routes } from "./routes";
import "./styles.css";

// Its own words in the page's language (src/locales), fetched as the page starts; it is drawn once they are in.
void addCatalogs({ ar: () => import("./locales/ar.json"), hi: () => import("./locales/hi.json") });

const queryClient = new QueryClient({
  defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false, staleTime: 30_000 } },
});

setFrameMode("app", COMMERCE_PACKAGES);

const router = createBrowserRouter(routes, { basename: routeBase("commerce") });

// Drawn once the page's words are in (its language's catalogs, fetched as the page starts).
void wordsReady().then(() =>
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
        <Toaster />
        <ConfirmHost />
      </QueryClientProvider>
    </StrictMode>,
  ),
);
