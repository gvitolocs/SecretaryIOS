import { request } from "./client";
import type { Sandbox } from "./computer";
import type { DeskCodec } from "../lib/deskWire";

// LucinaDesk tickets (lucinadesk repo docs/CONTRACT.md §1). A ticket is single use, lives 30 s to
// connect, and is bound to this device, the sandbox and the mode. It goes in the WebSocket's
// subprotocol list, never in a URL, and is never logged. Only the owner's devices get one; any
// other answer means "use the Cua viewer".

export interface DeskTicket {
  ticket: string;
  protocol: string;
  wsPath: string;
  expiresInSecs: number;
  sandbox: Sandbox;
  mode: "watch" | "control";
  /** The home-network door, when the request came from the house. */
  lan: { url: string } | null;
}

export const deskApi = {
  /** `codecs`: what this browser decodes, best first (a hint the gateway passes to the host). */
  ticket: (sandbox: Sandbox, mode: "watch" | "control", codecs: DeskCodec[]) =>
    request<DeskTicket>("POST", "/v1/computer/desk", { body: { sandbox, mode, codecs }, timeoutMs: 10_000 }),
};
