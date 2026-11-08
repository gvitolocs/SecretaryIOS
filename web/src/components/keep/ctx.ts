import { createContext, useContext, type Accessor } from "solid-js";
import type { KeepTheme } from "../../lib/keepColors";
import type { KeepView } from "../../lib/keepFilter";

/** The element popovers mount into: inside .kp, so they inherit Keep's theme variables. */
export const KeepRootContext = createContext<() => HTMLElement | undefined>(() => undefined);
export const useKeepRoot = () => useContext(KeepRootContext);

export interface KeepCtx {
  view: Accessor<KeepView>;
  theme: Accessor<KeepTheme>;
  open: (id: string) => void;
  goLabel: (tag: string) => void;
}

const fallback: KeepCtx = {
  view: () => ({ kind: "notes" }),
  theme: () => "dark",
  open: () => undefined,
  goLabel: () => undefined,
};
export const KeepContext = createContext<KeepCtx>(fallback);
export const useKeep = () => useContext(KeepContext);
