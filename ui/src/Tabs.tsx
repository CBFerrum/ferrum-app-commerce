// The store's own screens, at the top of each.

import { NavLink } from "react-router";
import { useRights, useWaiting } from "./data";

export function Tabs() {
  const rights = useRights();
  const waiting = useWaiting();
  return (
    <nav className="cm-tabs" aria-label="Store">
      <NavLink to="/" end>
        Today
      </NavLink>
      <NavLink to="/orders">Orders</NavLink>
      {rights.seesMoney && (
        <NavLink to="/money">
          Money
          {!!waiting && (
            <span className="cm-count" title={`${waiting} waiting for a decision`}>
              <span className="sr-only">{waiting} waiting for a decision</span>
              <span aria-hidden="true">{waiting}</span>
            </span>
          )}
        </NavLink>
      )}
    </nav>
  );
}
