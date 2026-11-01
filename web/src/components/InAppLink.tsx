import { useNavigate } from "@solidjs/router";
import type { JSX } from "solid-js";

/** The router's navigate, or undefined where there is no router (a component rendered on its own). */
function optionalNavigate(): ((to: string) => void) | undefined {
  try {
    return useNavigate();
  } catch {
    return undefined;
  }
}

/** A link into the app: a plain <a href> (so it opens anywhere), moved by the router on a normal click. */
export function InAppLink(props: { href: string; class?: string; children: JSX.Element }) {
  const navigate = optionalNavigate();
  return (
    <a
      href={props.href}
      class={props.class}
      onClick={(e) => {
        if (!navigate || e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
        e.preventDefault();
        e.stopPropagation();
        navigate(props.href);
      }}
    >
      {props.children}
    </a>
  );
}
