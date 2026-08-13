#!/usr/bin/env bash
# Verifies the auto-updater's signing chain offline, before a release depends on
# it. Checks that:
#
#   1. TAURI_SIGNING_PRIVATE_KEY / _PASSWORD are readable (from the environment,
#      or from the gitignored root .env) and shaped the way the Tauri CLI wants.
#   2. The private key actually decrypts with that password — i.e. the CLI can
#      sign with it.
#   3. The pubkey committed in app/src-tauri/tauri.conf.json is the PAIR of that
#      private key, by signing a scratch file and comparing the key id embedded
#      in the signature against the one in the public key.
#
# Check 3 is the one that earns this script. A mismatched pair builds green,
# publishes green, and then fails signature verification on every user's machine
# at install time — the failure surfaces where you cannot see it. Everything
# else here fails loudly in CI anyway.
#
# Usage:  ./scripts/verify-updater-key.sh
set -euo pipefail

cd "$(dirname "$0")/.."

pass=0
ok()   { printf '  \033[32mok\033[0m   %s\n' "$*"; }
fail() { printf '  \033[31mFAIL\033[0m %s\n' "$*" >&2; pass=1; }
die()  { printf '  \033[31mFAIL\033[0m %s\n' "$*" >&2; exit 1; }

# GNU base64 decodes with -d, BSD/macOS historically only with -D. Pick once.
if printf 'eA==' | base64 -d >/dev/null 2>&1; then
  b64d() { base64 -d; }
else
  b64d() { base64 -D; }
fi

# The 8-byte key id lives at the same offset in both a minisign public key and a
# minisign signature: 2 bytes of algorithm, then the id. (The algorithm bytes
# themselves differ — "Ed" in a key, "ED" in a prehashed signature — so compare
# only the id.)
keyid() { b64d | od -An -v -tx1 | tr -d ' \n' | cut -c5-20; }

echo "Environment"

# .env wins over the ambient environment. The point of this script is to check
# what is actually configured, not whatever a shell happened to export three
# hours ago — and a stale export is exactly how you end up comparing a new
# pubkey against an old private key and concluding the keypair is broken.
env_key="${TAURI_SIGNING_PRIVATE_KEY:-}"

if [ -f .env ]; then
  set -a; . ./.env; set +a
  ok ".env sourced"
  if [ -n "$env_key" ] && [ "$env_key" != "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
    fail "this shell exports a DIFFERENT TAURI_SIGNING_PRIVATE_KEY than .env holds. .env wins for the checks below, but a build run from this shell would sign with the stale key. Start a new shell, or: unset TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD"
  fi
elif [ -n "$env_key" ]; then
  ok "no .env — using TAURI_SIGNING_PRIVATE_KEY from the environment"
else
  die "no .env and no TAURI_SIGNING_PRIVATE_KEY exported"
fi

key="${TAURI_SIGNING_PRIVATE_KEY:-}"
pw="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"

[ -n "$key" ] || die "TAURI_SIGNING_PRIVATE_KEY is empty"
[ -n "$pw" ]  || die "TAURI_SIGNING_PRIVATE_KEY_PASSWORD is empty (GitHub rejects empty secrets, and release.yml's -n check fails on one)"
ok "both variables are set"

# A padded password is the corruption that survives a file but not a shell.
case "$pw" in
  [[:space:]]*|*[[:space:]]) fail "password has leading or trailing whitespace — quote it in .env, and re-set the GitHub secret from a file or stdin" ;;
  *) ok "password has no stray whitespace" ;;
esac

case "$(printf '%s' "$key" | b64d | head -1)" in
  *"encrypted secret key"*) ok "private key decodes to an rsign/minisign secret key" ;;
  *) fail "private key does not decode to a secret key — is it the whole file's contents, single-quoted?" ;;
esac

echo "Committed public key"

pub_b64="$(jq -r '.plugins.updater.pubkey // ""' app/src-tauri/tauri.conf.json)"
case "$pub_b64" in
  '')            die "plugins.updater.pubkey is missing from app/src-tauri/tauri.conf.json" ;;
  REPLACE_ME*)   die "plugins.updater.pubkey is still the placeholder — stamp in ~/.tauri/litecter.key.pub (see docs/release-pipeline.md)" ;;
esac

pub_raw="$(printf '%s' "$pub_b64" | b64d | tail -1)"
case "$(printf '%s' "$pub_b64" | b64d | head -1)" in
  *"minisign public key"*) ok "pubkey decodes to a minisign public key" ;;
  *) die "pubkey does not decode to a minisign public key" ;;
esac

echo "Pairing"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "litecter updater key check" > "$tmp/probe"

if ! (cd app && npx tauri signer sign -k "$key" -p "$pw" "$tmp/probe") >"$tmp/log" 2>&1; then
  sed 's/^/    /' "$tmp/log" >&2
  die "the CLI could not sign with this key — wrong password, or a mangled key string"
fi
ok "the private key signs (password is correct)"

sig_id="$(b64d < "$tmp/probe.sig" | sed -n 2p | keyid)"
pub_id="$(printf '%s' "$pub_raw" | keyid)"

if [ "$sig_id" = "$pub_id" ] && [ -n "$sig_id" ]; then
  ok "key ids match ($pub_id) — the committed pubkey is this key's pair"
else
  fail "KEY ID MISMATCH: pubkey $pub_id, signature $sig_id — the committed public key is NOT the pair of the private key in CI. Every update would fail verification on users' machines."
fi

echo
if [ "$pass" -eq 0 ]; then
  echo "All good. CI can sign an update that installed copies will accept."
else
  echo "Problems above — fix before merging to main." >&2
fi
exit "$pass"
