import { createLucinaStage, createVoice } from './lucina-stage.standalone.js';

const $ = (id) => document.getElementById(id);
const statusEl = $('status');
const log = $('log');
const muteBtn = $('mute');
const micBtn = $('mic');
const form = $('composer');
const input = $('text');
const sendBtn = $('send');
const hint = $('hint');

const auth = { credentials: 'same-origin', headers: { 'X-Secretary-CSRF': '1' } };
const sessionId = (() => {
  try {
    const k = 'lucina-page:session';
    let v = localStorage.getItem(k);
    if (!v) { v = crypto.randomUUID(); localStorage.setItem(k, v); }
    return v;
  } catch { return crypto.randomUUID(); }
})();

function say(role, text) {
  const div = document.createElement('div');
  div.className = `msg ${role}`;
  div.textContent = text; // never innerHTML
  log.appendChild(div);
  log.scrollTop = log.scrollHeight;
  return div;
}
function setStatus(t) { statusEl.textContent = t; }

async function ensureSignedIn() {
  const r = await fetch('/v1/session', { credentials: 'same-origin' });
  const j = r.ok ? await r.json().catch(() => ({})) : {};
  if (!j.authenticated) {
    setStatus('Not signed in');
    say('sys', 'Sign in on the main page first, then come back here.');
    const a = document.createElement('a');
    a.href = '/'; a.textContent = 'Open Secretary to sign in'; a.style.color = '#9fb0ff';
    log.appendChild(a);
    return false;
  }
  return true;
}

let stage = null;
let voice = null;
let busy = false;

function renderMute() {
  const muted = stage ? stage.isMuted() : true;
  muteBtn.textContent = muted ? '🔇' : '🔊';
  muteBtn.setAttribute('aria-pressed', String(muted));
  muteBtn.setAttribute('aria-label', muted ? 'Unmute Lucina' : 'Mute Lucina');
}

async function send(text) {
  text = (text || '').trim();
  if (!text || busy) return;
  busy = true; sendBtn.disabled = true; input.disabled = true;
  say('me', text);
  hint.classList.add('gone');
  stage?.setState('thinking');
  setStatus('Lucina is thinking…');
  try {
    const r = await fetch('/v1/chat', {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json', 'X-Secretary-CSRF': '1', 'X-Secretary-Client': 'web' },
      body: JSON.stringify({ message: text, sessionId: `lucina-page-${sessionId}`, clientMessageId: crypto.randomUUID() }),
    });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(j?.error?.message || `HTTP ${r.status}`);
    const reply = j.reply || '…';
    say('her', reply);
    setStatus('');
    if (stage && !stage.isMuted() && voice) {
      try {
        const audio = await voice.speak(reply.slice(0, 1200));
        await stage.speak(audio);
      } catch (e) { console.warn('tts', e); }
    }
  } catch (e) {
    say('sys', `Couldn't reach Lucina: ${e.message}`);
    setStatus('Offline?');
  } finally {
    stage?.setState('idle');
    busy = false; sendBtn.disabled = false; input.disabled = false; input.focus();
  }
}

async function listenAndSend() {
  if (!voice || busy) return;
  micBtn.classList.add('listening');
  stage?.setState('listening');
  setStatus('Listening…');
  try {
    const text = await voice.listen({ lang: 'it-IT' });
    micBtn.classList.remove('listening');
    if (text) await send(text);
    else { stage?.setState('idle'); setStatus(''); }
  } catch (e) {
    micBtn.classList.remove('listening');
    stage?.setState('idle');
    setStatus(e?.code === 'not-supported' ? 'Voice input not supported here: type instead' : 'Didn’t catch that');
    input.focus();
  }
}

form.addEventListener('submit', (e) => { e.preventDefault(); const t = input.value; input.value = ''; send(t); });
micBtn.addEventListener('click', listenAndSend);
muteBtn.addEventListener('click', () => {
  if (!stage) return;
  stage.unlockAudio?.();
  stage.setMuted(!stage.isMuted());
  renderMute();
});

(async () => {
  if (!(await ensureSignedIn())) return;
  try {
    stage = await createLucinaStage($('stage'), {
      modelUrl: '/v1/assets/lucina.glb',
      manifestUrl: '/v1/assets/manifest.json',
      fetchInit: auth,
      framing: 'bust',
      reducedMotion: matchMedia('(prefers-reduced-motion: reduce)').matches,
      onError: (e) => { console.warn('stage', e); setStatus('Model failed to load'); },
    });
    // First visit stays muted; later visits remember the choice (stored by the stage).
    if (localStorage.getItem('lucina-stage:muted') === null) stage.setMuted(true);
    stage.lookAtPointer?.(true);
    stage.onTap?.(() => listenAndSend());
    voice = createVoice({ ttsUrl: '/v1/tts', fetchInit: auth, lang: 'it-IT' });
    renderMute();
    setStatus('');
    setTimeout(() => hint.classList.add('gone'), 6000);
  } catch (e) {
    console.error(e);
    setStatus('Could not start the 3D stage');
    say('sys', `3D stage error: ${e.message}`);
  }
})();
