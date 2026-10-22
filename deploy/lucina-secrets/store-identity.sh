#!/usr/bin/env bash
# Store Lucina's Infisical machine identity (Universal Auth client id and
# client secret) ENCRYPTED with systemd-creds, for the lucina-rs user unit.
#
# Run it yourself, in your own terminal, on nezopt:
#
#   deploy/lucina-secrets/store-identity.sh
#
# It asks for the two values with `read -s` (nothing is echoed, nothing goes
# into argv, the shell history or a file in clear text), encrypts each one with
# `systemd-creds encrypt` (TPM2 by default), checks the round trip without
# printing it, and writes the systemd drop-in that loads them with
# `LoadCredentialEncrypted=`. It restarts nothing: it prints the two commands
# for you to run when you are ready.
#
# Why TPM2 and not the host key: lucina-rs is a USER unit and nezopt runs
# systemd 255. The user manager decrypts credentials as `nez`; the host key
# (/var/lib/systemd/credential.secret) is root-only, and per-user credentials
# (`systemd-creds --user`) only arrive in systemd 256. A TPM2-sealed
# credential can be opened by the user manager once `nez` may use the TPM
# (/dev/tpmrm0, group `tss`). The script checks that first and tells you the
# one-time fix if it is missing.
#
# Options:
#   --with-key KEY   tpm2 (default) | host | host+tpm2 (host keys need root and
#                    a SYSTEM unit; see docs/lucina-rust/SECRETS.md)
#   --out-dir DIR    where the .cred files go
#                    (default ~/.config/lucina-rs/credstore.encrypted)
#   --dropin FILE    the drop-in to write
#                    (default ~/.config/systemd/user/lucina-rs.service.d/secrets.conf)
#   --no-dropin      only store the credentials
#   --force          replace existing .cred files
#   --skip-tpm-check (tests only)
# Environment: SYSTEMD_CREDS (default systemd-creds; tests use a fake).
set -euo pipefail
set +o xtrace
umask 077

ID_NAME="lucina-infisical-client-id"
SECRET_NAME="lucina-infisical-client-secret"
WITH_KEY="tpm2"
OUT_DIR="${HOME}/.config/lucina-rs/credstore.encrypted"
DROPIN="${HOME}/.config/systemd/user/lucina-rs.service.d/secrets.conf"
WRITE_DROPIN=1
FORCE=0
TPM_CHECK=1
CREDS="${SYSTEMD_CREDS:-systemd-creds}"

die() { printf 'store-identity: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --with-key) WITH_KEY="${2:?}"; shift 2 ;;
    --out-dir) OUT_DIR="${2:?}"; shift 2 ;;
    --dropin) DROPIN="${2:?}"; shift 2 ;;
    --no-dropin) WRITE_DROPIN=0; shift ;;
    --force) FORCE=1; shift ;;
    --skip-tpm-check) TPM_CHECK=0; shift ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) die "unknown option $1" ;;
  esac
done

case "$WITH_KEY" in
  tpm2) ;;
  host|host+tpm2)
    [ "$(id -u)" -eq 0 ] || die "--with-key $WITH_KEY needs root and a system unit (User=nez); the user unit cannot open host-key credentials on systemd 255"
    ;;
  *) die "--with-key must be tpm2, host or host+tpm2" ;;
esac

if [ "$WITH_KEY" = "tpm2" ] && [ "$TPM_CHECK" -eq 1 ]; then
  if ! "$CREDS" has-tpm2 -q 2>/dev/null; then
    die "no usable TPM2 for $(id -un). One-time fix (as the owner):
    sudo usermod -aG tss $(id -un)
then log out and in again (the user manager must pick up the group; that
restarts your user services, so do it when lucina-rs may restart), and run
this script again."
  fi
  if [ ! -r /dev/tpmrm0 ] || [ ! -w /dev/tpmrm0 ]; then
    die "/dev/tpmrm0 is not usable by $(id -un). One-time fix (as the owner): sudo usermod -aG tss $(id -un), log out and in again, run this script again."
  fi
fi

mkdir -p "$OUT_DIR"
chmod 700 "$OUT_DIR"
ID_FILE="$OUT_DIR/$ID_NAME.cred"
SECRET_FILE="$OUT_DIR/$SECRET_NAME.cred"
if [ "$FORCE" -eq 0 ] && { [ -e "$ID_FILE" ] || [ -e "$SECRET_FILE" ]; }; then
  die "credentials already stored in $OUT_DIR (use --force to replace them, e.g. after a rotation)"
fi

# Read both values without echo. Works on a terminal (echo off) and on a pipe.
client_id=""
client_secret=""
IFS= read -r -s -p "Infisical machine identity client ID: " client_id || true
printf '\n' >&2
IFS= read -r -s -p "Infisical machine identity client secret: " client_secret || true
printf '\n' >&2
cleanup() { client_id=""; client_secret=""; unset client_id client_secret; }
trap cleanup EXIT

[ -n "$client_id" ] || die "empty client ID"
[ -n "$client_secret" ] || die "empty client secret"
case "$client_id" in
  *[[:space:]]*) die "the client ID contains whitespace" ;;
esac
case "$client_secret" in
  *[[:space:]]*) die "the client secret contains whitespace" ;;
esac
[ "${#client_secret}" -ge 16 ] || die "the client secret is shorter than 16 characters; is it the right value?"

# printf is a builtin: the values never appear in any process's argv.
encrypt() { # name value out
  printf '%s' "$2" | "$CREDS" encrypt --with-key="$WITH_KEY" --name="$1" - "$3.tmp" \
    || die "systemd-creds encrypt failed for $1"
  chmod 600 "$3.tmp"
  mv -f "$3.tmp" "$3"
  # Round trip, compared in memory, never printed.
  if ! "$CREDS" decrypt --name="$1" "$3" - 2>/dev/null | cmp -s - <(printf '%s' "$2"); then
    rm -f "$3"
    die "round trip failed for $1 (credential removed)"
  fi
}
encrypt "$ID_NAME" "$client_id" "$ID_FILE"
encrypt "$SECRET_NAME" "$client_secret" "$SECRET_FILE"
cleanup
printf 'Stored (encrypted, %s): %s, %s\n' "$WITH_KEY" "$ID_FILE" "$SECRET_FILE"

if [ "$WRITE_DROPIN" -eq 1 ]; then
  mkdir -p "$(dirname "$DROPIN")"
  cat > "$DROPIN" <<EOF
# Lucina secrets skill (written by deploy/lucina-secrets/store-identity.sh).
# The Infisical machine identity, decrypted by systemd into the unit's private
# credentials directory (\$CREDENTIALS_DIRECTORY, ramfs, mode 0400) at start.
[Service]
LoadCredentialEncrypted=$ID_NAME:$ID_FILE
LoadCredentialEncrypted=$SECRET_NAME:$SECRET_FILE
Environment=LUCINA_SECRETS_CONFIG=%h/.config/lucina-rs/secrets.toml
EOF
  chmod 600 "$DROPIN"
  printf 'Drop-in: %s\n' "$DROPIN"
fi

cat <<'EOF'
Next (when you are ready; nothing was restarted):
  1. ~/.config/lucina-rs/secrets.toml from deploy/lucina-secrets/secrets.example.toml (project id, no values)
  2. systemctl --user daemon-reload && systemctl --user restart lucina-rs
  3. journalctl --user -u lucina-rs | grep secrets   # "Infisical store ready"
EOF
