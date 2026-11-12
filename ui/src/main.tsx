// Commerce's own interface, the store's admin (Ferrum docs/plan/apps-ui.md §13, U5) at /commerce/:
// the day on its home, the web-order desk at /orders (what waits for money, to be sent, to be
// invoiced) with each order whole at /o/{id}, the money that needs a person at /money (refunds, and
// payments a gateway took and could not book), and the store's records (catalog, categories,
// coupons, shipping, carts, payment requests) as lists and forms in the records frame, with its
// module in the sidebar under the platform's bar.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode, useEffect } from "react";
import { createRoot } from "react-dom/client";
import { createBrowserRouter, Link, RouterProvider, useRouteError } from "react-router";
import { addCatalogs, routeBase, shell, t, wordsReady } from "@ferrum/client";
import { ConfirmHost, EmptyState, ErrorState, Toaster } from "@ferrum/kit";
import { RequireAuth, setFrameMode } from "@ferrum/records/frame";
import { CommerceHome } from "./Home";
import "./styles.css";

// Its own words in the page's language (src/locales), fetched as the page starts; it is drawn once they are in.
void addCatalogs({ ar: () => import("./locales/ar.json"), hi: () => import("./locales/hi.json") });

/** A page of the records frame, loaded when first opened. */
const page = (name: "FormView" | "ListView" | "ReportView" | "AppSettingsPage" | "ScreenPage") => () =>
  import("./records").then((m) => ({ Component: m[name] }));

/** Commerce's package: the sidebar shows its module. */
const COMMERCE_PACKAGES = ["commerce"];

const queryClient = new QueryClient({
  defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false, staleTime: 30_000 } },
});

function RouteError() {
  return <ErrorState error={useRouteError()} onRetry={() => window.location.reload()} />;
}

function NotFound() {
  return (
    <EmptyState title={t("Page not found")}>
      <Link to="/">{t("Go to the store's home")}</Link>
    </EmptyState>
  );
}

/** Every page tells the shell it started. */
function Started() {
  useEffect(() => shell().ready(), []);
  return null;
}

setFrameMode("app", COMMERCE_PACKAGES);

const router = createBrowserRouter(
  [
    {
      path: "/",
      element: (
        <>
          <Started />
          <RequireAuth />
        </>
      ),
      errorElement: <RouteError />,
      children: [
        { index: true, element: <CommerceHome /> },
        { path: "orders", lazy: () => import("./Desk").then((m) => ({ Component: m.Desk })) },
        { path: "o/:id", lazy: () => import("./Order").then((m) => ({ Component: m.OrderRoute })) },
        { path: "returns", lazy: () => import("./Returns").then((m) => ({ Component: m.Returns })) },
        { path: "alerts", lazy: () => import("./Alerts").then((m) => ({ Component: m.Alerts })) },
        { path: "variants", lazy: () => import("./Variants").then((m) => ({ Component: m.Variants })) },
        { path: "variants/:id", lazy: () => import("./Variants").then((m) => ({ Component: m.Variants })) },
        { path: "words", lazy: () => import("./StoreWords").then((m) => ({ Component: m.Words })) },
        { path: "money", lazy: () => import("./Money").then((m) => ({ Component: m.Money })) },
        { path: "e/:entity", lazy: page("ListView") },
        { path: "e/:entity/:id", lazy: page("FormView") },
        { path: "r/:name", lazy: page("ReportView") },
        { path: "apps/:app/settings", lazy: page("AppSettingsPage") },
        { path: "x/:app/:page", lazy: page("ScreenPage") },
        { path: "*", element: <NotFound /> },
      ],
    },
  ],
  { basename: routeBase("commerce") },
);

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
