import { createLucinaStage, createVoice, isVoiceError, synthSpeechWav, type Expression, type LucinaStage, type StageState } from '../src/index';

const $ = <T extends HTMLElement>(sel: string): T => {
  const el = document.querySelector<T>(sel);
  if (!el) throw new Error(`missing ${sel}`);
  return el;
};

const logEl = $<HTMLDivElement>('#log');
function log(msg: string): void {
  logEl.textContent = `${new Date().toLocaleTimeString()}  ${msg}\n${logEl.textContent ?? ''}`.slice(0, 4000);
}
const toastEl = $<HTMLDivElement>('#toast');
let toastTimer = 0;
function toast(msg: string): void {
  toastEl.textContent = msg;
  toastEl.style.opacity = '1';
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (toastEl.style.opacity = '0'), 1400);
}

async function pickModel(): Promise<{ modelUrl?: string; manifestUrl?: string }> {
  const q = new URLSearchParams(location.search);
  if (q.has('placeholder')) return {};
  if (q.get('model')) {
    const r: { modelUrl?: string; manifestUrl?: string } = { modelUrl: q.get('model')! };
    if (q.get('manifest')) r.manifestUrl = q.get('manifest')!;
    return r;
  }
  try {
    const res = await fetch('/dev-assets/lucina.glb', { method: 'HEAD' });
    if (res.ok) {
      const man = await fetch('/dev-assets/manifest.json', { method: 'HEAD' });
      return man.ok ? { modelUrl: '/dev-assets/lucina.glb', manifestUrl: '/dev-assets/manifest.json' } : { modelUrl: '/dev-assets/lucina.glb' };
    }
  } catch {
    /* no dev server assets */
  }
  return {};
}

async function main(): Promise<void> {
  const q = new URLSearchParams(location.search);
  const canvas = $<HTMLCanvasElement>('#stage');
  const model = await pickModel();
  const framingParam = q.get('framing') === 'full' ? 'full' : 'bust';
  const stage: LucinaStage = await createLucinaStage(canvas, {
    ...model,
    framing: framingParam,
    fetchInit: { credentials: 'include' },
    ...(q.has('reduced') ? { reducedMotion: true } : {}),
    // ?env (optionally ?env=high|medium|low and ?theme=light): the stage set from src/scene/.
    ...(q.has('env')
      ? {
          environment: {
            quality: (['low', 'medium', 'high'].includes(q.get('env') ?? '') ? q.get('env') : 'auto') as 'auto',
            theme: q.get('theme') === 'light' ? 'light' : 'dark',
          },
        }
      : {}),
    onError: (e) => log(`model error: ${String(e)}`),
  });
  (window as unknown as { stage: LucinaStage }).stage = stage;
  // The demo always starts muted (lip-sync still runs); `?unmuted` opts in to sound.
  // Audio only ever plays after an explicit click on "speak test".
  stage.setMuted(!q.has('unmuted'));
  // Dev helper: saves the current frame (transparent PNG) through the dev server's /dev-capture.
  (window as unknown as { capture: (name: string) => Promise<string> }).capture = async (name) => {
    const { renderer, scene, camera } = stage.three;
    renderer.render(scene, camera);
    const url = canvas.toDataURL('image/png');
    const res = await fetch(`/dev-capture?name=${encodeURIComponent(name)}`, { method: 'POST', body: url });
    return res.text();
  };
  log(`loaded ${stage.info.placeholder ? 'placeholder' : model.modelUrl} ext=[${stage.info.extensions.join(', ')}]`);
  log(`bones: ${JSON.stringify(stage.info.rig.bones)}`);
  log(`missing bones: ${stage.info.rig.missingBones.join(', ') || 'none'}; missing morphs: ${stage.info.rig.missingMorphs.join(', ') || 'none'}`);

  const hud = $<HTMLDivElement>('#hud');
  setInterval(() => {
    const s = stage.stats.snapshot();
    hud.textContent = `${stage.state}${stage.speaking ? ' (audio)' : ''} · ${s.fps.toFixed(0)} fps · ${s.frameMs.toFixed(1)} ms · cpu ${s.cpuMs.toFixed(2)} ms · max ${s.maxFrameMs.toFixed(0)} ms`;
  }, 250);

  const stateButtons = [...document.querySelectorAll<HTMLButtonElement>('[data-state]')];
  const syncStates = (): void => {
    for (const b of stateButtons) b.setAttribute('aria-pressed', String(b.dataset.state === stage.state));
    $('#speak').setAttribute('aria-pressed', String(stage.state === 'speaking'));
  };
  stage.onStateChange((n, p) => {
    log(`state ${p} -> ${n}`);
    syncStates();
  });
  syncStates();
  for (const b of stateButtons) b.addEventListener('click', () => stage.setState(b.dataset.state as StageState));

  const voiceFor = (): ReturnType<typeof createVoice> => {
    const mock = $<HTMLInputElement>('#mock').checked;
    const base = $<HTMLInputElement>('#base').value.trim().replace(/\/$/, '');
    return createVoice({
      ttsUrl: mock ? '/mock/v1/tts' : `${base}/v1/tts`,
      fetchInit: { credentials: 'include' },
      lang: 'it-IT',
    });
  };

  $('#speak').addEventListener('click', async () => {
    const text = $<HTMLInputElement>('#text').value;
    try {
      let audio: Blob | ArrayBuffer;
      try {
        audio = await voiceFor().speak(text);
      } catch (e) {
        log(`TTS unavailable (${isVoiceError(e) ? e.code : String(e)}), using generated audio`);
        audio = synthSpeechWav({ pattern: text });
      }
      const t0 = performance.now();
      await stage.speak(audio, { onEnd: () => log(`speech ended after ${((performance.now() - t0) / 1000).toFixed(2)} s`) });
    } catch (e) {
      log(`speak failed: ${String(e)}`);
    }
  });
  $('#stop').addEventListener('click', () => stage.stopSpeaking());
  $('#nod').addEventListener('click', () => stage.nod());

  const exprButtons = [...document.querySelectorAll<HTMLButtonElement>('[data-expr]')];
  for (const b of exprButtons) {
    b.addEventListener('click', () => {
      const e = (b.dataset.expr || null) as Expression | null;
      stage.setExpression(e, 1);
      for (const x of exprButtons) x.setAttribute('aria-pressed', String(x === b));
    });
  }

  const muteBtn = $('#mute');
  const syncMute = (): void => {
    muteBtn.setAttribute('aria-pressed', String(stage.isMuted()));
    muteBtn.textContent = stage.isMuted() ? 'muted' : 'mute';
  };
  syncMute();
  muteBtn.addEventListener('click', () => {
    stage.setMuted(!stage.isMuted());
    syncMute();
  });

  let look = true;
  stage.lookAtPointer(look);
  $('#look').addEventListener('click', () => {
    look = !look;
    stage.lookAtPointer(look);
    $('#look').setAttribute('aria-pressed', String(look));
  });
  let full = framingParam === 'full';
  $('#framing').setAttribute('aria-pressed', String(full));
  $('#framing').addEventListener('click', () => {
    full = !full;
    stage.setFraming(full ? 'full' : 'bust');
    $('#framing').setAttribute('aria-pressed', String(full));
  });
  let reduced = q.has('reduced');
  $('#reduced').setAttribute('aria-pressed', String(reduced));
  $('#reduced').addEventListener('click', () => {
    reduced = !reduced;
    stage.setReducedMotion(reduced);
    $('#reduced').setAttribute('aria-pressed', String(reduced));
  });

  stage.onTap((e) => {
    toast(`tap: ${e.object}`);
    log(`tap on ${e.object}`);
    stage.setExpression('smile', 0.8);
    setTimeout(() => stage.setExpression(null), 1200);
  });

  $('#mic').addEventListener('click', async () => {
    const voice = voiceFor();
    stage.setState('listening');
    try {
      const text = await voice.listen({
        lang: 'it-IT',
        interim: true,
        onInterim: (t) => log(`… ${t}`),
        onSpeechStart: () => stage.nod(),
      });
      log(`heard: ${text}`);
      stage.setState('thinking');
      setTimeout(() => $('#speak').click(), 900);
    } catch (e) {
      log(`listen failed: ${isVoiceError(e) ? e.code : String(e)}`);
      stage.setState('idle');
    }
  });

  if (q.has('state')) stage.setState(q.get('state') as StageState);
  if (q.has('expr')) stage.setExpression(q.get('expr') as Expression);
}

main().catch((e) => log(`init failed: ${String(e)}`));
