// Commerce's route table, apart from the page that mounts it so a test can mount the real thing.

import { useEffect } from "react";
import { Link, type RouteObject, useRouteError } from "react-router";
import { shell, t } from "@ferrum/client";
import { EmptyState, ErrorState } from "@ferrum/kit";
import { RequireAuth } from "@ferrum/records/frame";
import { CommerceHome } from "./Home";
import { NeedsStore } from "./NeedsStore";

/** A page of the records frame, loaded when first opened. */
const page = (name: "FormView" | "ListView" | "ReportView" | "AppSettingsPage" | "ScreenPage") => () =>
  import("./records").then((m) => ({ Component: m[name] }));

/** Commerce's package: the sidebar shows its module. */
export const COMMERCE_PACKAGES = ["commerce"];

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

export const routes: RouteObject[] = [
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
        { element: <NeedsStore />, children: [{ index: true, element: <CommerceHome /> }] },
        // The store's own screens need the store: until Store Settings exist they say so and lead to making them.
        {
          element: <NeedsStore />,
          children: [
        { path: "orders", lazy: () => import("./Desk").then((m) => ({ Component: m.Desk })) },
        { path: "o/:id", lazy: () => import("./Order").then((m) => ({ Component: m.OrderRoute })) },
        { path: "returns", lazy: () => import("./Returns").then((m) => ({ Component: m.Returns })) },
        { path: "alerts", lazy: () => import("./Alerts").then((m) => ({ Component: m.Alerts })) },
        { path: "variants", lazy: () => import("./Variants").then((m) => ({ Component: m.Variants })) },
        { path: "variants/:id", lazy: () => import("./Variants").then((m) => ({ Component: m.Variants })) },
        { path: "words", lazy: () => import("./StoreWords").then((m) => ({ Component: m.Words })) },
        { path: "money", lazy: () => import("./Money").then((m) => ({ Component: m.Money })) },
          ],
        },
        { path: "e/:entity", lazy: page("ListView") },
        { path: "e/:entity/:id", lazy: page("FormView") },
        { path: "r/:name", lazy: page("ReportView") },
        { path: "apps/:app/settings", lazy: page("AppSettingsPage") },
        { path: "x/:app/:page", lazy: page("ScreenPage") },
        { path: "*", element: <NotFound /> },
      ],
    },
];
