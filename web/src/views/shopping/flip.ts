// FLIP animation for tiles that move between the to-buy grid and "Recently".
// Tiles carry `data-flip-id`; we measure every tile before a state change,
// let Solid update the DOM synchronously, then animate each tile from its old
// box to its new one with the Web Animations API (no inline style markup).
// New tiles pop in. Nothing animates under prefers-reduced-motion.

const DURATION_MS = 260;

function reducedMotion(): boolean {
  try {
    return window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
  } catch {
    return false;
  }
}

function tiles(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>("[data-flip-id]"));
}

/** Run `change` (which must update the DOM synchronously) and animate the tiles it moved. */
export function withFlip(root: HTMLElement | undefined, change: () => void): void {
  if (!root || reducedMotion() || typeof Element.prototype.animate !== "function") {
    change();
    return;
  }
  const before = new Map<string, DOMRect>();
  for (const el of tiles(root)) before.set(el.dataset.flipId ?? "", el.getBoundingClientRect());
  change();
  for (const el of tiles(root)) {
    const id = el.dataset.flipId ?? "";
    const old = before.get(id);
    if (!old) {
      el.animate([{ transform: "scale(0.6)", opacity: 0 }, { transform: "none", opacity: 1 }], {
        duration: DURATION_MS,
        easing: "cubic-bezier(.2,.8,.3,1.2)",
      });
      continue;
    }
    const now = el.getBoundingClientRect();
    const dx = old.left - now.left;
    const dy = old.top - now.top;
    if (Math.abs(dx) < 1 && Math.abs(dy) < 1) continue;
    const sx = now.width ? old.width / now.width : 1;
    const sy = now.height ? old.height / now.height : 1;
    el.animate(
      [
        { transform: `translate(${dx}px, ${dy}px) scale(${sx}, ${sy})`, transformOrigin: "top left" },
        { transform: "none", transformOrigin: "top left" },
      ],
      { duration: DURATION_MS, easing: "cubic-bezier(.2,.7,.2,1)" },
    );
  }
}
