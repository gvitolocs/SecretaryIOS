import { describe, expect, it, vi } from 'vitest';
import { createVoice, isVoiceError, listen, type SpeechRecognitionLike } from '../src/voice';

class FakeRecognition implements SpeechRecognitionLike {
  static script: (r: FakeRecognition) => void = () => {};
  lang = '';
  interimResults = false;
  continuous = true;
  maxAlternatives = 0;
  onresult: SpeechRecognitionLike['onresult'] = null;
  onerror: SpeechRecognitionLike['onerror'] = null;
  onend: (() => void) | null = null;
  onspeechstart: (() => void) | null = null;
  start(): void {
    queueMicrotask(() => FakeRecognition.script(this));
  }
  stop(): void {
    this.onend?.();
  }
  abort(): void {
    this.onerror?.({ error: 'aborted' });
    this.onend?.();
  }
  result(text: string, isFinal: boolean): void {
    const r = Object.assign([{ transcript: text }], { isFinal });
    this.onresult?.({ resultIndex: 0, results: Object.assign([r], { length: 1 }) });
  }
}

describe('listen()', () => {
  it('rejects with not-supported when there is no Web Speech API', async () => {
    await expect(listen({ recognition: null })).rejects.toMatchObject({ code: 'not-supported' });
  });

  it('resolves the final transcript and reports interim text', async () => {
    const interim = vi.fn();
    const heard = vi.fn();
    FakeRecognition.script = (r) => {
      r.onspeechstart?.();
      r.result('ciao', false);
      r.result('ciao Lucina', true);
      r.onend?.();
    };
    const text = await listen({ recognition: FakeRecognition, lang: 'it-IT', interim: true, onInterim: interim, onSpeechStart: heard });
    expect(text).toBe('ciao Lucina');
    expect(interim).toHaveBeenCalledWith('ciao');
    expect(heard).toHaveBeenCalled();
  });

  it('maps permission errors to permission-denied', async () => {
    FakeRecognition.script = (r) => {
      r.onerror?.({ error: 'not-allowed' });
      r.onend?.();
    };
    const e = await listen({ recognition: FakeRecognition }).catch((x: unknown) => x);
    expect(isVoiceError(e, 'permission-denied')).toBe(true);
  });

  it('rejects with no-speech when nothing was heard', async () => {
    FakeRecognition.script = (r) => r.onend?.();
    await expect(listen({ recognition: FakeRecognition })).rejects.toMatchObject({ code: 'no-speech' });
  });

  it('can be aborted', async () => {
    FakeRecognition.script = () => {};
    const ac = new AbortController();
    const p = listen({ recognition: FakeRecognition, signal: ac.signal });
    ac.abort();
    await expect(p).rejects.toMatchObject({ code: 'aborted' });
  });
});

describe('createVoice().speak()', () => {
  it('POSTs JSON with the caller auth and returns the audio blob', async () => {
    const fetch = vi.fn(async () => new Response(new Blob([new Uint8Array([1, 2, 3])], { type: 'audio/mpeg' })));
    const voice = createVoice({
      ttsUrl: '/v1/tts',
      lang: 'it-IT',
      fetchInit: { credentials: 'include', headers: { 'X-Secretary-CSRF': '1' } },
      fetch,
    });
    const blob = await voice.speak('Buongiorno');
    expect(blob.size).toBe(3);
    const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe('/v1/tts');
    expect(init.method).toBe('POST');
    expect(init.credentials).toBe('include');
    const h = new Headers(init.headers);
    expect(h.get('X-Secretary-CSRF')).toBe('1');
    expect(h.get('Content-Type')).toBe('application/json');
    expect(JSON.parse(init.body as string)).toEqual({ text: 'Buongiorno', lang: 'it-IT' });
  });

  it('turns 401 into an unauthorized VoiceError and other failures into http', async () => {
    const v401 = createVoice({ fetch: async () => new Response('no', { status: 401 }) });
    await expect(v401.speak('x')).rejects.toMatchObject({ code: 'unauthorized', status: 401 });
    const v500 = createVoice({ fetch: async () => new Response('no', { status: 503 }) });
    await expect(v500.speak('x')).rejects.toMatchObject({ code: 'http', status: 503 });
    const vNet = createVoice({ fetch: async () => Promise.reject(new TypeError('offline')) });
    await expect(vNet.speak('x')).rejects.toMatchObject({ code: 'network' });
  });
});
