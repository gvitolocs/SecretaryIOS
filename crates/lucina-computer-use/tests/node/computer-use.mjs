// Thin transport into native Hermes. Cua owns execution; no Harness fallback.
import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';

const LAUNCHER = '/home/nez/Projects/Lucina/lucina-node/scripts/run-hermes-computer.sh';
const TOOLS = new Set(['list_spaces', 'space_bash', 'space_write', 'list_tools', 'call_tool', 'stream_endpoint', 'list_space_windows', 'stream_space_window']);
const BACKEND_TOOLS = new Set(['terminal', 'process', 'process_manage', 'read_file', 'write_file', 'patch', 'search_files']);

function cuaTool(name) {
  if (name.startsWith('mcp__android__')) name=name.replace('mcp__android__','mcp__cua__');
  if (BACKEND_TOOLS.has(name)) return name;
  if (/^mcp__phone__(phone_info|phone_screenshot|phone_ui|phone_shell|phone_input|phone_otp|phone_call)$/.test(name)) return name;
  return [...TOOLS].find((tool) => name === `mcp_cua_${tool}` || name === `mcp__cua__${tool}`);
}

function succeeded(event) {
  if (event.is_error || /\[exit (?!0\])\d+\]/.test(event.output || '')) return false;
  // Hermes wraps MCP evidence as untrusted data. Inspect the payload without
  // treating its text as instructions, including nested driver errors.
  let payload = String(event.output || '').replace(/^<untrusted_tool_result[^>]*>[\s\S]*?\n\n/, '').replace(/\n<\/untrusted_tool_result>\s*$/, '');
  for (let depth = 0; depth < 3; depth++) {
    let value;
    try { value = JSON.parse(payload); } catch { break; }
    if (value.error || value.isError || value.not_found || value.verified === false ||
        (typeof value.exit_code === 'number' && value.exit_code !== 0)) return false;
    if (typeof value.result !== 'string') break;
    payload = value.result;
  }
  return true;
}

export function parseComputerResult(output) {
  let events;
  try { events = String(output).split('\n').filter((line) => line.trim()).map((line) => JSON.parse(line)); }
  catch { throw new Error('Hermes returned an invalid computer-use event stream.'); }
  const result = events.findLast((event) => event.type === 'result');
  if (result?.exit_code !== 0 && /quota|rate.limit|RESOURCE_EXHAUSTED/i.test(String(result?.error || result?.text || ''))) {
    throw new Error('The computer-use model quota is exhausted; no task completion was confirmed.');
  }
  if (!result || result.exit_code !== 0 || !result.text) throw new Error('Hermes computer-use request failed; no completion was confirmed.');
  const receipts = events.filter((event) => event.type === 'tool_result' && cuaTool(event.name || ''));
  const successful = receipts.filter(succeeded);
  const observed = successful.filter((event) => !['list_spaces', 'list_tools', 'stream_endpoint'].includes(cuaTool(event.name)));
  return {
    status: observed.length ? (successful.length === receipts.length ? 'completed' : 'partial') : 'unverified',
    reply: !observed.length ? `No sandbox action was confirmed. ${result.text}`
      : successful.length !== receipts.length ? `Some tool calls failed; the task is only partially verified.\n\n${result.text}` : result.text,
    sessionId: result.session_id,
    executedTools: observed.map((event) => event.name),
    toolErrors: receipts.length - successful.length,
  };
}

export function isComputerUseReady() {
  return process.env.LUCINA_COMPUTER_USE_ENABLED === 'true' && existsSync(LAUNCHER);
}

export function createComputerUseClient({ launcher = LAUNCHER, enabled = isComputerUseReady, timeoutMs = 200000, env = process.env } = {}) {
  let active = false; let recoveryRequired = false;
  return async function runComputerTask(prompt, { signal } = {}) {
    if (!enabled()) throw new Error('Lucina’s sandbox tools are not enabled.');
    if (recoveryRequired) throw new Error('Lucina’s computer-use gateway needs recovery after an unconfirmed cancellation.');
    if (active) throw new Error('Lucina’s desktop is busy with another task.');
    const task = String(prompt || '').trim();
    if (!task || task.length > 16000) throw new Error('Computer-use request is empty or too long.');
    signal?.throwIfAborted();
    active = true;
    try {
      return await new Promise((resolve, reject) => {
        // Never inherit unrelated host credentials into the agent. Cua's guest
        // gets commands through its driver, without the orchestrator environment.
        const childEnv = { HOME: '/home/nez', USER: 'nez', LANG: 'C.UTF-8', PATH: '/home/nez/.local/bin:/usr/local/bin:/usr/bin:/bin' };
        if (env.GEMINI_API_KEY) childEnv.GEMINI_API_KEY = env.GEMINI_API_KEY;
        const child = spawn(launcher, [], { env: childEnv, cwd: '/tmp', detached: true, stdio: ['pipe', 'pipe', 'pipe'], ...(process.getuid?.() === 0 ? { uid: 1000, gid: 1000 } : {}) });
        let output = ''; let failure; let hardKill;
        // Discard stderr content, which may include provider diagnostics.
        child.stderr.resume();
        const stopGateway = () => new Promise((done, fail) => {
          const control = spawn(launcher, ['--stop-gateway'], {
            env: childEnv, cwd: '/tmp', stdio: 'ignore',
            ...(process.getuid?.() === 0 ? { uid: 1000, gid: 1000 } : {}),
          });
          const limit = setTimeout(() => { control.kill('SIGKILL'); fail(new Error('Gateway stop timed out.')); }, 15000);
          control.once('error', (error) => { clearTimeout(limit); fail(error); });
          control.once('close', (code) => { clearTimeout(limit); code === 0 ? done() : fail(new Error('Gateway stop failed.')); });
        });
        const terminate = (sig) => { try { if (child.pid) process.kill(-child.pid, sig); } catch {} };
        const stop = (error) => {
          if (failure) return;
          failure = error;
          terminate('SIGINT');
          hardKill = setTimeout(() => terminate('SIGKILL'), 3000);
        };
        const abort = () => stop(new Error('Computer-use request cancelled.'));
        const timer = setTimeout(() => stop(new Error('Computer-use deadline exceeded.')), timeoutMs);
        signal?.addEventListener('abort', abort, { once: true });
        if (signal?.aborted) abort();
        child.stdout.on('data', (chunk) => { output += chunk; if (Buffer.byteLength(output) > 2 * 1024 * 1024) stop(new Error('Computer-use output limit exceeded.')); });
        child.stdin.on('error', () => {});
        child.on('error', () => { failure ||= new Error('Native Hermes computer-use runtime unavailable.'); });
        child.on('close', async (code) => {
          clearTimeout(timer); clearTimeout(hardKill); signal?.removeEventListener('abort', abort);
          if (failure) {
            try { await stopGateway(); }
            catch { recoveryRequired = true; failure = new Error('Computer task stopped locally, but agent cancellation is unconfirmed; desktop execution is locked until recovery.'); }
            reject(failure); return;
          }
          if (code !== 0) {
            try { parseComputerResult(output); } catch (error) { reject(error); return; }
            reject(new Error('Native Hermes could not complete the computer-use request.')); return;
          }
          try { resolve(parseComputerResult(output)); } catch (error) { reject(error); }
        });
        child.stdin.end(task);
      });
    } finally { active = false; }
  };
}

export const runComputerTask = createComputerUseClient();

/** Legacy chat transport boundary only: device logic and secrets stay in Rust. */
export function runRustDevice(mode, input) {
  if (!['otp-chat','otp-intent','dashboard'].includes(mode)) return Promise.reject(new Error('Unsupported device operation.'));
  return new Promise((resolve,reject)=>{
    const child=spawn('/home/nez/Projects/Lucina/bin/lucina-device',[mode],{
      env:{HOME:'/home/nez',PATH:'/usr/bin:/bin'},...(process.getuid?.()===0?{uid:1000,gid:1000}:{}),stdio:['pipe','pipe','ignore']});
    let output='',failed=false;
    const stop=()=>{failed=true;child.kill('SIGKILL');reject(new Error('Rust device operation unavailable.'));};
    const timer=setTimeout(stop,15000);
    child.stdout.on('data',b=>{output+=b;if(output.length>65536)stop();});
    child.stdin.on('error',()=>{});
    child.on('error',()=>{clearTimeout(timer);stop();});
    child.on('close',()=>{clearTimeout(timer);if(failed)return;try{const value=JSON.parse(output);if(!value.ok)throw new Error();resolve(value.data);}catch{reject(new Error('Rust device operation unavailable.'));}});
    child.stdin.end(JSON.stringify(input));
  });
}
