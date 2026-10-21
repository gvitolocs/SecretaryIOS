/**
 * Voice helpers: text-to-speech through the Secretary server (`POST /v1/tts`) and speech
 * recognition through the Web Speech API. Failures surface as typed `VoiceError`s.
 */

export type VoiceErrorCode =
  | 'not-supported'
  | 'permission-denied'
  | 'no-speech'
  | 'aborted'
  | 'audio-capture'
  | 'network'
  | 'language-not-supported'
  | 'unauthorized'
  | 'http'
  | 'unknown';

export class VoiceError extends Error {
  override readonly name = 'VoiceError';

  constructor(
    readonly code: VoiceErrorCode,
    message: string,
    readonly status?: number,
    override readonly cause?: unknown,
  ) {
    super(message);
  }
}

export function isVoiceError(e: unknown, code?: VoiceErrorCode): e is VoiceError {
  return e instanceof VoiceError && (code === undefined || e.code === code);
}

// --- Minimal Web Speech API typings (not in lib.dom for all TS versions) ---
export interface SpeechRecognitionAlternativeLike {
  transcript: string;
}
export interface SpeechRecognitionResultLike {
  readonly isFinal: boolean;
  readonly length: number;
  [index: number]: SpeechRecognitionAlternativeLike;
}
export interface SpeechRecognitionEventLike {
  readonly resultIndex: number;
  readonly results: { readonly length: number; [index: number]: SpeechRecognitionResultLike };
}
export interface SpeechRecognitionErrorEventLike {
  readonly error: string;
  readonly message?: string;
}
export interface SpeechRecognitionLike {
  lang: string;
  interimResults: boolean;
  continuous: boolean;
  maxAlternatives: number;
  onresult: ((e: SpeechRecognitionEventLike) => void) | null;
  onerror: ((e: SpeechRecognitionErrorEventLike) => void) | null;
  onend: (() => void) | null;
  onspeechstart: (() => void) | null;
  start(): void;
  stop(): void;
  abort(): void;
}
export type SpeechRecognitionCtor = new () => SpeechRecognitionLike;

export function getSpeechRecognition(scope: unknown = globalThis): SpeechRecognitionCtor | null {
  const g = scope as { SpeechRecognition?: SpeechRecognitionCtor; webkitSpeechRecognition?: SpeechRecognitionCtor };
  try {
    return g.SpeechRecognition ?? g.webkitSpeechRecognition ?? null;
  } catch {
    return null;
  }
}

export function isListenSupported(): boolean {
  return getSpeechRecognition() !== null;
}

export interface ListenOptions {
  lang?: 'it-IT' | 'en-US' | (string & {});
  /** Report interim transcripts through `onInterim`. */
  interim?: boolean;
  onInterim?: (text: string) => void;
  /** Fired when the recogniser hears speech (use it for `stage.nod()`). */
  onSpeechStart?: () => void;
  signal?: AbortSignal;
  /** Gives up (rejects with `no-speech`) after this many ms. Default 15000. */
  timeoutMs?: number;
  /** Override the recogniser constructor (tests, native bridges). */
  recognition?: SpeechRecognitionCtor | null;
}

function mapRecognitionError(code: string, message?: string): VoiceError {
  switch (code) {
    case 'not-allowed':
    case 'service-not-allowed':
      return new VoiceError('permission-denied', 'Microphone or speech recognition permission was denied.');
    case 'no-speech':
      return new VoiceError('no-speech', 'No speech was detected.');
    case 'aborted':
      return new VoiceError('aborted', 'Listening was aborted.');
    case 'audio-capture':
      return new VoiceError('audio-capture', 'No microphone is available.');
    case 'network':
      return new VoiceError('network', 'Speech recognition needs a network connection.');
    case 'language-not-supported':
      return new VoiceError('language-not-supported', 'This language is not supported for speech recognition.');
    default:
      return new VoiceError('unknown', message || `Speech recognition failed (${code}).`);
  }
}

/** Listens for one utterance and resolves with the final transcript. */
export function listen(opts: ListenOptions = {}): Promise<string> {
  const Ctor = opts.recognition === undefined ? getSpeechRecognition() : opts.recognition;
  if (!Ctor) {
    return Promise.reject(new VoiceError('not-supported', 'Speech recognition is not supported in this browser.'));
  }
  if (opts.signal?.aborted) return Promise.reject(new VoiceError('aborted', 'Listening was aborted.'));

  return new Promise<string>((resolve, reject) => {
    let rec: SpeechRecognitionLike;
    try {
      rec = new Ctor();
    } catch (e) {
      reject(new VoiceError('not-supported', 'Speech recognition could not be started.', undefined, e));
      return;
    }
    let finalText = '';
    let interimText = '';
    let settled = false;
    let failure: VoiceError | null = null;
    const timer = setTimeout(() => {
      failure = failure ?? new VoiceError('no-speech', 'No speech was detected in time.');
      try {
        rec.abort();
      } catch {
        /* ignore */
      }
      finish();
    }, opts.timeoutMs ?? 15000);

    const onAbort = (): void => {
      failure = new VoiceError('aborted', 'Listening was aborted.');
      try {
        rec.abort();
      } catch {
        /* ignore */
      }
      finish();
    };
    opts.signal?.addEventListener('abort', onAbort, { once: true });

    function finish(): void {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      opts.signal?.removeEventListener('abort', onAbort);
      if (failure) {
        reject(failure);
        return;
      }
      const text = (finalText || interimText).trim();
      if (text) resolve(text);
      else reject(new VoiceError('no-speech', 'No speech was detected.'));
    }

    rec.lang = opts.lang ?? 'it-IT';
    rec.interimResults = opts.interim ?? false;
    rec.continuous = false;
    rec.maxAlternatives = 1;
    rec.onspeechstart = () => opts.onSpeechStart?.();
    rec.onresult = (e) => {
      let fin = '';
      let int = '';
      for (let i = 0; i < e.results.length; i++) {
        const r = e.results[i];
        const alt = r?.[0];
        if (!r || !alt) continue;
        if (r.isFinal) fin += alt.transcript;
        else int += alt.transcript;
      }
      finalText = fin;
      interimText = int;
      if (int && opts.interim) opts.onInterim?.((fin + int).trim());
    };
    rec.onerror = (e) => {
      // "no-speech" after we already have text is not an error.
      if (e.error === 'no-speech' && (finalText || interimText)) return;
      failure = mapRecognitionError(e.error, e.message);
    };
    rec.onend = () => finish();
    try {
      rec.start();
    } catch (e) {
      failure = new VoiceError('unknown', 'Speech recognition could not be started.', undefined, e);
      finish();
    }
  });
}

export interface VoiceOptions {
  /** TTS endpoint. Default `/v1/tts`. */
  ttsUrl?: string;
  /** Merged into every request: credentials, Authorization header, etc. */
  fetchInit?: RequestInit;
  /** Default language for TTS and recognition. */
  lang?: 'it-IT' | 'en-US' | (string & {});
  /** Custom fetch (tests). */
  fetch?: typeof fetch;
}

export interface Voice {
  /** POSTs `{text, lang}` and resolves with the audio (audio/mpeg). */
  speak(text: string, opts?: { lang?: string; signal?: AbortSignal }): Promise<Blob>;
  listen(opts?: ListenOptions): Promise<string>;
  readonly listenSupported: boolean;
}

function mergeHeaders(base: HeadersInit | undefined, extra: Record<string, string>): Headers {
  const h = new Headers(base);
  for (const [k, v] of Object.entries(extra)) if (!h.has(k)) h.set(k, v);
  return h;
}

export function createVoice(opts: VoiceOptions = {}): Voice {
  const ttsUrl = opts.ttsUrl ?? '/v1/tts';
  const doFetch = opts.fetch ?? ((...a: Parameters<typeof fetch>) => globalThis.fetch(...a));
  return {
    async speak(text, o = {}) {
      const lang = o.lang ?? opts.lang;
      const body: { text: string; lang?: string } = { text };
      if (lang) body.lang = lang;
      let res: Response;
      try {
        res = await doFetch(ttsUrl, {
          ...opts.fetchInit,
          method: 'POST',
          headers: mergeHeaders(opts.fetchInit?.headers, {
            'Content-Type': 'application/json',
            Accept: 'audio/mpeg',
          }),
          body: JSON.stringify(body),
          signal: o.signal ?? opts.fetchInit?.signal ?? null,
        });
      } catch (e) {
        if ((e as { name?: string })?.name === 'AbortError') throw new VoiceError('aborted', 'TTS request aborted.', undefined, e);
        throw new VoiceError('network', 'TTS request failed.', undefined, e);
      }
      if (res.status === 401 || res.status === 403) {
        throw new VoiceError('unauthorized', `TTS request was not authorised (${res.status}).`, res.status);
      }
      if (!res.ok) throw new VoiceError('http', `TTS request failed (${res.status}).`, res.status);
      return res.blob();
    },
    listen(o = {}) {
      const l = o.lang ?? opts.lang;
      return listen(l ? { ...o, lang: l } : o);
    },
    get listenSupported() {
      return isListenSupported();
    },
  };
}
