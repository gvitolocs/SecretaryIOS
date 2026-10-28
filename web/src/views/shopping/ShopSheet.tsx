import type { JSX } from "solid-js";
import { Sheet } from "../../components/page";
import { shop } from "../../state/shopping";

/**
 * The desktop dialog of a Shopping sheet: the page-system Sheet (centred
 * dialog, header with close, Escape and focus handling), with the body inside
 * a `.bring` scope. The dialog is portalled out of the page, so the `--b-*`
 * tokens and the list's language have to be set again inside it.
 */
export function ShopSheet(props: {
  open: boolean;
  onClose: () => void;
  title: JSX.Element;
  footer?: JSX.Element;
  children: JSX.Element;
}) {
  return (
    <Sheet open={props.open} onClose={props.onClose} title={props.title} footer={props.footer}>
      <div class="bring shopping-sheet-body" lang={shop.lang}>
        {props.children}
      </div>
    </Sheet>
  );
}
