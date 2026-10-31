// Commerce's own interface (Ferrum docs/plan/apps-ui.md), built with Ferrum-Frontend's packages and
// libraries from the sibling checkout: ../../ferrum/web, where CI checks it out too.
import { ferrumApp } from "../../ferrum/web/packages/build/vite.mjs";

export default ferrumApp(import.meta.url);
