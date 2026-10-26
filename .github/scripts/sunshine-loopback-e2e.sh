#!/usr/bin/env bash
# Real Sunshine -> Moonlight streaming end-to-end test on one Linux machine.
#
# Starts a REAL Sunshine host on virtual display A (software x264, X11 capture),
# pairs a REAL Moonlight client with a PIN through Sunshine's authenticated API,
# then runs CloudCraft's opt-in `real_sunshine_moonlight_stream_lifecycle` test,
# which configures the remote app over the pinned-TLS API, launches Moonlight on
# virtual display B, waits until the host reports the app session running, holds
# the stream, stops it and checks that Moonlight is gone and the host is free.
#
# Nothing here is mocked. What it does NOT prove: a remote machine on another
# network, a GPU encoder/decoder, or Minecraft itself (the remote app is an
# OpenGL program because a Minecraft client needs a licensed account).
#
# Requirements (Ubuntu 24.04): xvfb pulseaudio mesa-utils x11-apps openssl curl
# and the Sunshine .deb installed. Run as an unprivileged user.
#
# Environment:
#   CLOUDCRAFT_MOONLIGHT  absolute path of the Moonlight executable (required)
#   CLOUDCRAFT_TEST_BIN   prebuilt theseus_gui test binary; default runs cargo
#   CC_HOLD_SECS          seconds to hold the stream (default 15)
#   CC_LAUNCH_COMMAND     app Sunshine runs on the host (default glxgears)
#   CC_EVIDENCE_DIR       where logs and screenshots go (default ./sunshine-e2e)
set -euo pipefail

: "${CLOUDCRAFT_MOONLIGHT:?set CLOUDCRAFT_MOONLIGHT to the Moonlight executable}"
HOLD="${CC_HOLD_SECS:-15}"
LAUNCH="${CC_LAUNCH_COMMAND:-glxgears}"
EVIDENCE="$(mkdir -p "${CC_EVIDENCE_DIR:-sunshine-e2e}" && cd "${CC_EVIDENCE_DIR:-sunshine-e2e}" && pwd)"
HOST_DISPLAY=":91"
CLIENT_DISPLAY=":92"
PIN="$(printf '%04d' $((RANDOM % 10000)))"
PASSWORD="$(head -c 24 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' | head -c 24)"

WORK="$(mktemp -d)"
export HOME="$WORK/home"
export XDG_RUNTIME_DIR="$WORK/xdg"
mkdir -p "$HOME/.config/sunshine" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

pids=()
cleanup() {
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  pulseaudio --kill 2>/dev/null || true
  sleep 1
  for pid in "${pids[@]}"; do kill -9 "$pid" 2>/dev/null || true; done
  rm -rf "$WORK"
}
trap cleanup EXIT

log() { echo "[sunshine-e2e] $*"; }

Xvfb "$HOST_DISPLAY" -screen 0 1280x720x24 -nolisten tcp >"$EVIDENCE/xvfb-host.log" 2>&1 &
pids+=($!)
Xvfb "$CLIENT_DISPLAY" -screen 0 1280x720x24 -nolisten tcp >"$EVIDENCE/xvfb-client.log" 2>&1 &
pids+=($!)
pulseaudio --daemonize=yes --exit-idle-time=-1 --log-target="file:$EVIDENCE/pulseaudio.log" || log "pulseaudio unavailable; continuing without audio"
if command -v pactl >/dev/null; then
  for _ in $(seq 1 10); do pactl info >/dev/null 2>&1 && break; sleep 1; done
  # Runners have no sound card; give Sunshine a sink to capture.
  if pactl info >/dev/null 2>&1; then
    pactl list short sinks | grep -q . || pactl load-module module-null-sink sink_name=cloudcraft_null >/dev/null
    log "PulseAudio is up ($(pactl list short sinks | wc -l) sink(s))"
  else
    log "PulseAudio did not answer; continuing without audio"
  fi
fi
sleep 1

CONF="$HOME/.config/sunshine/sunshine.conf"
cat >"$CONF" <<'EOF'
sunshine_name = cloudcraft-loopback
encoder = software
capture = x11
address_family = ipv4
origin_web_ui_allowed = lan
EOF
sunshine "$CONF" --creds admin "$PASSWORD" >"$EVIDENCE/sunshine-creds.log" 2>&1
DISPLAY="$HOST_DISPLAY" sunshine "$CONF" >"$EVIDENCE/sunshine.log" 2>&1 </dev/null &
pids+=($!)

SERVERINFO="http://127.0.0.1:47989/serverinfo?uniqueid=0123456789ABCDEF"
for _ in $(seq 1 30); do
  curl -fsS "$SERVERINFO" >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS "$SERVERINFO" | grep -q '<state>SUNSHINE_SERVER_FREE</state>' || {
  log "Sunshine did not become ready"; tail -40 "$EVIDENCE/sunshine.log"; exit 1;
}
log "Sunshine is up and idle"

# Sunshine's API certificate is self-signed; CloudCraft pins its SHA-256.
CERT_SHA256="$(echo | openssl s_client -connect 127.0.0.1:47990 2>/dev/null | openssl x509 -outform DER | sha256sum | cut -d' ' -f1)"
[ "$CERT_SHA256" != "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" ] || {
  log "Could not read the Sunshine certificate"; exit 1;
}

api() { curl -fsS -k -u "admin:$PASSWORD" "$@"; }

# One-time PIN pairing, the same handshake a user performs in the Sunshine web UI.
# Moonlight 6.2.0 derives its AES key with QByteArray(hash.constData()), which stops
# at the first NUL byte, so roughly 1 in 16 attempts fails with "Incorrect PIN"
# regardless of the PIN. Each attempt uses a fresh random salt, so retry with a
# clean Moonlight identity.
MOONLIGHT_CONFIG="$HOME/.config/Moonlight Game Streaming Project"
try_pair() {
  local attempt="$1" pair_pid pairing_id="" paired=1
  rm -rf "$MOONLIGHT_CONFIG"
  DISPLAY="$CLIENT_DISPLAY" timeout 60 "$CLOUDCRAFT_MOONLIGHT" pair 127.0.0.1 --pin "$PIN" \
    >"$EVIDENCE/moonlight-pair-$attempt.log" 2>&1 &
  pair_pid=$!
  pids+=("$pair_pid")
  for _ in $(seq 1 40); do
    pairing_id="$(api https://127.0.0.1:47990/api/pin | grep -o '"id":"[0-9a-fA-F]\{32\}"' | head -1 | cut -d'"' -f4 || true)"
    [ -n "$pairing_id" ] && break
    sleep 1
  done
  if [ -z "$pairing_id" ]; then
    log "attempt $attempt: Moonlight never requested pairing"
  elif api -H 'Content-Type: application/json' -X POST \
      -d "{\"pairing_id\":\"$pairing_id\",\"pin\":\"$PIN\",\"name\":\"cloudcraft-e2e\"}" \
      https://127.0.0.1:47990/api/pin | grep -q '"status":true' \
    && api https://127.0.0.1:47990/api/clients/list | grep -q '"name":"cloudcraft-e2e"'; then
    paired=0
  else
    log "attempt $attempt: pairing was rejected ($(grep -o 'Qt Critical: .*' "$EVIDENCE/moonlight-pair-$attempt.log" | head -1))"
  fi
  # The pair command keeps its window open; on success pairing is recorded on both sides.
  kill "$pair_pid" 2>/dev/null || true
  wait "$pair_pid" 2>/dev/null || true
  return "$paired"
}
paired=no
for attempt in 1 2 3 4 5; do
  if try_pair "$attempt"; then paired=yes; break; fi
  sleep 2
done
[ "$paired" = yes ] || { log "Moonlight could not pair after 5 attempts"; exit 1; }
log "Moonlight is paired (attempt $attempt)"

capture_frames() {
  sleep $((HOLD / 2 + 6))
  DISPLAY="$HOST_DISPLAY" xwd -root -silent >"$EVIDENCE/host-display.xwd" 2>/dev/null || true
  DISPLAY="$CLIENT_DISPLAY" xwd -root -silent >"$EVIDENCE/client-display.xwd" 2>/dev/null || true
}
capture_frames &
capture_pid=$!

export DISPLAY="$CLIENT_DISPLAY"
export CLOUDCRAFT_SUNSHINE_HOST=127.0.0.1
export CLOUDCRAFT_SUNSHINE_USER=admin
export CLOUDCRAFT_SUNSHINE_PASSWORD="$PASSWORD"
export CLOUDCRAFT_SUNSHINE_CERT_SHA256="$CERT_SHA256"
export CLOUDCRAFT_SUNSHINE_LAUNCH_COMMAND="$LAUNCH"
export CLOUDCRAFT_SUNSHINE_HOLD_SECS="$HOLD"
export CLOUDCRAFT_MOONLIGHT

set +e
if [ -n "${CLOUDCRAFT_TEST_BIN:-}" ]; then
  "$CLOUDCRAFT_TEST_BIN" --ignored --nocapture --exact \
    cloudcraft_cloud_sunshine::tests::real_sunshine_moonlight_stream_lifecycle 2>&1 | tee "$EVIDENCE/test.log"
else
  cargo test --package theseus_gui --no-default-features \
    cloudcraft_cloud_sunshine::tests::real_sunshine_moonlight_stream_lifecycle \
    -- --ignored --nocapture --exact 2>&1 | tee "$EVIDENCE/test.log"
fi
status=${PIPESTATUS[0]}
set -e
wait "$capture_pid" 2>/dev/null || true

grep -q 'test result: ok. 1 passed' "$EVIDENCE/test.log" || status=1
moonlight_name="$(basename "$CLOUDCRAFT_MOONLIGHT")"
if pgrep -u "$(id -u)" -x "$moonlight_name" >/dev/null; then
  log "Moonlight is still running after the test"; status=1
fi
if pgrep -u "$(id -u)" -x "$(basename "${LAUNCH%% *}")" >/dev/null; then
  log "The remote app is still running after Stop"; status=1
fi
grep -E 'stream ready after|Incoming frame rate|Decoding frame rate|Rendering frame rate|Average network latency' "$EVIDENCE/test.log" || true
[ "$status" -eq 0 ] && log "PASS: real stream started, held, stopped, and cleaned up" || log "FAIL"
exit "$status"
