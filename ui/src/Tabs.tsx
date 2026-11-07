// The store's own screens, at the top of each.

import { NavLink } from "react-router";
import { useRights, useWaiting } from "./data";
import { t } from "@ferrum/client";

export function Tabs() {
  const rights = useRights();
  const waiting = useWaiting();
  return (
    <nav className="cm-tabs" aria-label={t("Store")}>
      <NavLink to="/" end>
        {t("Today")}
      </NavLink>
      <NavLink to="/orders">{t("Orders")}</NavLink>
      {rights.seesMoney && (
        <NavLink to="/money">
          {t("Money")}
          {!!waiting && (
            <span className="cm-count" title={`${waiting} waiting for a decision`}>
              <span className="sr-only">{t("{waiting} waiting for a decision", { waiting })}</span>
              <span aria-hidden="true">{waiting}</span>
            </span>
          )}
        </NavLink>
      )}
    </nav>
  );
}
