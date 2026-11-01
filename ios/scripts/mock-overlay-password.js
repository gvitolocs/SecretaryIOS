// ---- iOS overlay: password change (when upstream lacks it) ----
if (path === "/v1/session/password" && method === "POST") {
  const s = currentSession(req);
  if (!s) return fail(res, 401, "unauthorized", "missing or malformed device token", { "WWW-Authenticate": "Bearer" });
  const body = await readJson(req);
  if (typeof body.currentPassword !== "string" || body.currentPassword !== iosOwnerPassword()) {
    return fail(res, 403, "invalid_credentials", "current password is wrong");
  }
  if (typeof body.newPassword !== "string" || body.newPassword.length < 8) {
    return fail(res, 400, "bad_request", "newPassword must be at least 8 characters");
  }
  iosSetOwnerPassword(body.newPassword);
  return send(res, 200, { changed: true, sessionsRevoked: 0 });
}
// ---- end iOS password overlay ----
