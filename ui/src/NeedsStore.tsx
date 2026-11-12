// The store's own screens (the day, the orders, returns, money) work from Store Settings: its company, currency and
// price list. A workspace that has just installed Commerce has none, so these screens say so and lead to making it,
// rather than ask the server questions it can only refuse.

import { useQuery } from "@tanstack/react-query";
import { Link, Outlet } from "react-router";
import { t } from "@ferrum/client";
import { ListSkeleton, StatePanel } from "@ferrum/kit";
import { can, useEntityMeta } from "@ferrum/records";
import { list } from "./data";

export function NeedsStore() {
  // Settings the person cannot read, or a failed question, say nothing: the screens show as they did.
  const q = useQuery({ queryKey: ["commerce", "store-set-up"], queryFn: ({ signal }) => list<{ id: string }>("StoreSettings", { fields: "id", limit: 1 }, signal).then((r) => r.rows.length > 0), staleTime: 5_000 });
  const meta = useEntityMeta("StoreSettings").data;
  if (q.isPending) return <ListSkeleton rows={3} />;
  if (q.data !== false) return <Outlet />;
  const may = can(meta?.permissions, "create");
  return (
    <StatePanel
      kind="empty"
      title={t("The store is not set up yet")}
      action={may ? <Link className="btn btn-primary btn-md" to="/e/StoreSettings/new">{t("Set up the store")}</Link> : undefined}
    >
      {t("Store Settings name the store's company, currency, price list and warehouse. Until they are made there is nothing to show here and no storefront.")}{" "}
      {may ? "" : t("Ask an administrator to set the store up.")}
    </StatePanel>
  );
}
