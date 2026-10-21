// ---- iOS overlay state (inserted by ios/scripts/run-mock.sh) ----
const IOS_EMAIL = "owner@example.test";
let iosPassword = process.env.MOCK_PASSWORD ?? "dev-password-123";
/** @type {Map<string, {deviceName: string, deviceId: string}>} */
const iosTokens = new Map();
const iosLock = new Map();
const iosTimeline = [];
let iosSeq = 0;
const iosStreams = new Set();
const iosChatItems = new Map();
function iosEvent(item) {
  return `id: ${item.id}\nevent: item\ndata: ${JSON.stringify(item)}\n\n`;
}
function iosAppend(fields, ts = new Date()) {
  const item = { id: `tl_${String(++iosSeq).padStart(6, "0")}`, ts: ts.toISOString(), ...fields };
  iosTimeline.push(item);
  for (const stream of iosStreams) stream.write(iosEvent(item));
  return item;
}
{
  const ago = (h) => new Date(Date.now() - h * 3_600_000);
  iosAppend({ channel: "telegram", role: "user", text: "Ricordami di comprare il latte", meta: { chatTitle: "Giuseppe", isDm: true } }, ago(50));
  iosAppend({ channel: "telegram", role: "lucina", text: "Fatto: latte aggiunto alla lista della spesa.", meta: { chatTitle: "Giuseppe", isDm: true } }, ago(49.99));
  iosAppend({ channel: "discord", role: "user", text: "Lucina, what's on the agenda tomorrow?", meta: { guildName: "Home", channelName: "general" } }, ago(27));
  iosAppend({ channel: "discord", role: "lucina", text: "Dentist at 9:30, then the gym. https://example.com/agenda", meta: { guildName: "Home", channelName: "general" } }, ago(26.99));
  iosAppend({ channel: "alexa", role: "user", text: "Add eggs to the shopping list" }, ago(5));
  iosAppend({ channel: "alexa", role: "lucina", text: "Eggs are on the list." }, ago(4.99));
  iosAppend({ channel: "telegram", role: "lucina", text: "Reminder: call the dentist.", meta: { kind: "reminder", isDm: true } }, ago(2));
  iosAppend({ channel: "web", role: "user", text: "Plan a light push workout for tonight" }, ago(1));
  iosAppend({ channel: "web", role: "lucina", text: "Bench 3×8, overhead press 3×8, lateral raises 3×12." }, ago(0.99));
}
// ---- end iOS overlay state ----
