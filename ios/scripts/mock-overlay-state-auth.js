// ---- iOS overlay state: device tokens ----
// Uses the upstream owner account (OWNER_EMAIL / ownerPassword) when the mock has one.
let iosFallbackPassword = process.env.MOCK_PASSWORD ?? "dev-password-123";
const iosOwnerEmail = () => (typeof OWNER_EMAIL !== "undefined" ? OWNER_EMAIL : "owner@example.test");
const iosOwnerPassword = () => (typeof ownerPassword !== "undefined" ? ownerPassword : iosFallbackPassword);
const iosSetOwnerPassword = (value) => {
  if (typeof ownerPassword !== "undefined") ownerPassword = value; else iosFallbackPassword = value;
};
/** @type {Map<string, {deviceName: string, deviceId: string}>} */
const iosTokens = new Map();
const iosLock = new Map();
// ---- end iOS overlay state: device tokens ----
