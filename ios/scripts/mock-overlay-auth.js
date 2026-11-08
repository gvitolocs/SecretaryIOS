// ---- iOS overlay (inserted by ios/scripts/run-mock.sh into a local copy) ----
// The web mock authenticates with the sec_session cookie only. The iOS app uses a
// device Bearer token, so this overlay adds what the real server has and the mock
// lacks: POST /v1/auth/device (email + password -> device token), Bearer auth on every
// route (CSRF-exempt), and POST /v1/session/password. Never committed with the mock.
if (path === "/v1/auth/device" && method === "POST") {
  const ip = req.socket.remoteAddress ?? "?";
  const now = Date.now();
  const lock = iosLock.get(ip);
  if (lock && lock.until > now) {
    return fail(res, 429, "account_locked", "Too many failed sign-ins. Try again later.", {
      "Retry-After": String(Math.ceil((lock.until - now) / 1000)),
    });
  }
  const body = await readJson(req);
  await sleep(250);
  const email = typeof body.email === "string" ? body.email.trim().toLowerCase() : "";
  const password = typeof body.password === "string" ? body.password : "";
  if (!email || !password) return fail(res, 400, "bad_request", "email and password are required");
  if (email !== iosOwnerEmail() || password !== iosOwnerPassword()) {
    const failures = (lock?.failures ?? 0) + 1;
    iosLock.set(ip, { failures, until: failures >= 5 ? now + 60_000 : 0 });
    return fail(res, 401, "invalid_credentials", "invalid email or password");
  }
  iosLock.delete(ip);
  const token = `sec_${randomUUID().replace(/-/g, "")}mock`;
  const deviceName = typeof body.deviceName === "string" && body.deviceName.trim() ? body.deviceName.trim() : "iPhone";
  const deviceId = randomUUID();
  iosTokens.set(token, { deviceName, deviceId });
  return send(res, 201, { token, deviceId, deviceName });
}
// ---- end iOS auth overlay ----
