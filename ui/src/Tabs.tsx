// The store's own screens, at the top of each.

import { NavLink } from "react-router";
import { useReturnsWaiting, useRights, useWaiting } from "./data";
import { t } from "@ferrum/client";

export function Tabs() {
  const rights = useRights();
  const waiting = useWaiting();
  const returns = useReturnsWaiting();
  return (
    <nav className="cm-tabs" aria-label={t("Store")}>
      <NavLink to="/" end>
        {t("Today")}
      </NavLink>
      <NavLink to="/orders">{t("Orders")}</NavLink>
      {rights.seesReturns && (
        <NavLink to="/returns">
          {t("Returns")}
          {!!returns && (
            <span className="cm-count" title={t("{waiting} waiting for a decision", { waiting: returns })}>
              <span className="sr-only">{t("{waiting} waiting for a decision", { waiting: returns })}</span>
              <span aria-hidden="true">{returns}</span>
            </span>
          )}
        </NavLink>
      )}
      {rights.seesReturns && <NavLink to="/alerts">{t("Stock alerts")}</NavLink>}
      {rights.seesMoney && (
        <NavLink to="/money">
          {t("Money")}
          {!!waiting && (
            <span className="cm-count" title={t("{waiting} waiting for a decision", { waiting })}>
              <span className="sr-only">{t("{waiting} waiting for a decision", { waiting })}</span>
              <span aria-hidden="true">{waiting}</span>
            </span>
          )}
        </NavLink>
      )}
    </nav>
  );
}
