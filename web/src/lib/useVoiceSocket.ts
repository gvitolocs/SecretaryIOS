import { createSignal, onCleanup, type Accessor } from "solid-js";
import { voiceSocket, type SocketStatus, type VoiceSocket } from "./voiceSocket";

/**
 * Keep the voice socket open while the calling component is mounted (it closes 60 s after the last
 * voice UI goes). `status` follows the socket.
 */
export function useVoiceSocket(socket: VoiceSocket = voiceSocket()): { socket: VoiceSocket; status: Accessor<SocketStatus> } {
  const [status, setStatus] = createSignal<SocketStatus>(socket.status);
  const off = socket.onStatus(setStatus);
  const release = socket.acquire();
  setStatus(socket.status);
  onCleanup(() => {
    off();
    release();
  });
  return { socket, status };
}

/** `m:ss` for a recording length. */
export function formatClock(ms: number): string {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}
