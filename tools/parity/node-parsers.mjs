// Runs inside the live Node Lucina container (read only: no writes, no network):
//   docker exec -i lucina-nez node --input-type=module -e "$(cat node-parsers.mjs)" < cases.json
// stdin: {"now": "<ISO>", "cases": [{"text", "sessionId"?}]}. Prints one line
// "PARITY_JSON:<json>" with the deterministic parsers' results per case, in the
// shape `lucina parity-parsers` compares against the Rust ports.
const chunks = [];
for await (const c of process.stdin) chunks.push(c);
const input = JSON.parse(Buffer.concat(chunks).toString('utf8'));
const now = new Date(input.now);
const tz = 'Europe/Rome';
const intent = await import('/app/src/lucina-intent.js');
const h = await import('/app/src/intent-heuristics.js');
const grocery = await import('/app/src/grocery/intent.js');
const notes = await import('/app/src/notes-bridge.js');
const outfit = await import('/app/src/avatar-outfit.js');
const cal = await import('/app/src/calendar-bridge.js');
const api = await import('/app/src/secretary-api.js');
const presence = await import('/app/src/discord-presence-context.js');
const hints = await import('/app/src/lucina-capability-hints.js');
const lang = await import('/app/src/reply-language.js');
const logInspect = await import('/app/src/lucina-log-inspect.js');
const caps = await import('/app/src/lucina-capabilities.js');

// A parser that throws is reported as {"throws": message} (Node would answer 500).
const safe = (fn) => { try { return fn(); } catch (e) { return { __throws: String(e?.message || e) }; } };
const thrown = (v) => v && typeof v === 'object' && '__throws' in v ? { throws: v.__throws } : undefined;
const pick = (o, keys) => (o ? Object.fromEntries(keys.map((k) => [k, o[k] ?? null])) : null);
const out = input.cases.map(({ text, sessionId = '' }) => {
  const t0 = intent.classifyLucinaIntentTier0(text, '');
  const g = grocery.parseGroceryIntent(text);
  const n = notes.parseNotesIntent(text, { pendingPart: false });
  const np = notes.parseNotesIntent(text, { pendingPart: true });
  const o = outfit.parseOutfitIntent(text);
  const cq = cal.parseCalendarQuestion(text, { now, timeZone: tz });
  const cc = safe(() => cal.parseCalendarCreate(text, { now, timeZone: tz }));
  const m = api.mcpTurnInfo({ sessionId, message: text });
  return {
    text,
    tier0: t0 ? { intent: t0.intent, confidence: t0.confidence, reply_mode: t0.reply_mode, scope: t0.scope ?? null } : null,
    grocery: g ? {
      intent: g.intent,
      rawProduct: g.rawProduct ?? '',
      product: g.product?.id ?? null,
      store: g.store?.displayName ?? null,
      quantity: g.quantity ?? null,
      unit: g.unit ?? null,
    } : null,
    notes: pick(n, ['kind', 'name', 'slug', 'title', 'content', 'more', 'lang']),
    notesPending: np ? np.kind : null,
    outfit: o ? { outfit: o.outfit ?? null, hair: o.hair ?? null, lang: o.lang } : null,
    calQuestion: pick(cq, ['from', 'to', 'week', 'lang']),
    calCreate: thrown(cc) ?? (cc ? (cc.error ? { error: cc.error, lang: cc.lang } : { event: cc.event, allDay: cc.allDay, lang: cc.lang }) : null),
    calConfirm: thrown(cc) ? null : (cc && !cc.error ? cal.confirmQuestion(cc, { timeZone: tz }) : null),
    mcp: m,
    presenceQ: presence.isPresenceQuestion(text),
    reminderReq: h.looksLikeReminderRequest(text),
    reminderList: h.looksLikeReminderListRequest(text),
    scope: h.reminderListScopeForRequest(text),
    complex: h.looksLikeComplexLucinaRequest(text, ''),
    recall: h.EXPLICIT_RECALL_RE.test(text),
    travel: h.looksLikeTravelInfoRequest(text),
    actionable: h.looksLikeActionableLucinaRequest(text),
    storeNote: notes.looksLikeStoreNoteRequest(text),
    logInspect: logInspect.looksLikeDirectLogInspectRequest(text),
    yesno: cal.classifyYesNo(text),
    selfDesc: caps.looksLikeSelfDescriptionRequest(text),
    lang: lang.detectReplyLanguage(text),
    hints: hints.appendCapabilityHints('Ok, ci penso io.', text, { action: 'reply' }),
  };
});
// Exit only after the pipe took the whole line (stdout to a pipe is async).
process.stdout.write(`PARITY_JSON:${JSON.stringify(out)}\n`, () => process.exit(0));
