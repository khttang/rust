#!/usr/bin/env bash
# validate-openshell.sh — run agent-harness in a real OpenShell sandbox under
# openshell-policy.yaml and check that each policy rule is enforced.
#
# Unlike run-bounded.sh (an offline mock), this needs a live gateway with the
# docker compute driver. Validated with OpenShell 0.1.2 on colima (aarch64).
#
# Steps:
#   1. preflight   gateway connected; supervisor can reach it from the docker host
#   2. build       aarch64/x86_64 linux binary in rust:1.95 (pinned by digest),
#                  then sandbox.Dockerfile (base pinned by digest)
#   3. policy      the effective policy matches the file (plus gateway baseline)
#   4. enforce     identity, filesystem and egress probes inside one sandbox
#   5. provider    credential injection via providers/openai.yaml and
#                  providers/gemini.yaml
#   6. task        verified-fix in the sandbox: the policy covers the task's
#                  sandbox needs, and `verified-fix self-test` runs CBMC (and
#                  gcc) on every corpus case under Landlock, audited
#
# The egress probe sends a dummy Gemini key: Google's "API key not valid" reply
# proves the request crossed the proxy without spending quota. The provider
# check attaches a temporary provider holding a dummy OpenAI key: the sandbox
# must see only a placeholder, and OpenAI's "Incorrect API key provided:
# sk-dummy…" reply proves the proxy substituted the value. No real credentials
# are read or passed.
#
# Optional live call (costs one request on your key): create a provider of
# type agent-harness-openai yourself (see providers/openai.yaml), then
#   OPENSHELL_LIVE_OPENAI_PROVIDER=<provider name> ./validate-openshell.sh
# Optional live Gemini call (one request): create the gemini provider (see
# providers/gemini.yaml), then OPENSHELL_LIVE_GEMINI_PROVIDER=gemini (model:
# OPENSHELL_LIVE_GEMINI_MODEL, default gemini-3.5-flash).
# Optional live task run (several requests): additionally set
#   OPENSHELL_LIVE_FIX_CASE=<corpus case, e.g. average_div_zero>
# to run verified-fix end to end on that case, patches auto-approved.
#
# Usage: ./validate-openshell.sh [--skip-build]
#
# Writes only under this directory (target/linux-*, .sandbox/image).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
POLICY="$ROOT/openshell-policy.yaml"
IMAGE="agent-harness-sandbox:dev"
# The build toolchain (RUST_IMAGE), pinned by digest in images.env so the
# same commit always builds with the same compiler. Debian trixie: the base in
# sandbox.Dockerfile must stay trixie too so glibc matches.
# shellcheck source=images.env
source "$ROOT/images.env"
# Sandbox names are capped at 19 characters (OpenShell 0.1.2).
PREFIX="ahv-$$"
SKIP_BUILD=0
FAILS=0

die()  { echo "validate: error: $*" >&2; exit 1; }
log()  { echo "validate: $*" >&2; }
pass() { echo "  PASS  $*"; }
fail() { echo "  FAIL  $*"; FAILS=$((FAILS + 1)); }

# expect <label> <actual> <expected>
expect() { if [[ "$2" == "$3" ]]; then pass "$1 ($2)"; else fail "$1: got '$2', want '$3'"; fi; }

case "${1:-}" in
  --skip-build) SKIP_BUILD=1 ;;
  -h|--help) sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  "") ;;
  *) die "unknown argument: $1" ;;
esac

PROFILE="$ROOT/providers/openai.yaml"
PROFILE_ID="agent-harness-openai"
GEMINI_PROFILE="$ROOT/providers/gemini.yaml"
GEMINI_PROFILE_ID="agent-harness-gemini"

cleanup() {
  openshell sandbox delete "$PREFIX-pol" >/dev/null 2>&1 || true
  openshell provider delete "$PREFIX-oai" >/dev/null 2>&1 || true
  openshell provider delete "$PREFIX-gem" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# 1. Preflight ---------------------------------------------------------------
log "1/6 preflight"
command -v openshell >/dev/null || die "openshell CLI not found"
command -v docker >/dev/null || die "docker CLI not found"
# The gateway connection occasionally fails once; allow a few attempts.
connected=0
for attempt in 1 2 3; do
  if openshell status 2>&1 | grep -q 'Status: Connected'; then connected=1; break; fi
  sleep 2
done
[[ $connected -eq 1 ]] || die "gateway not connected after 3 attempts (openshell status)"
# The docker driver runs the supervisor with host networking against
# https://127.0.0.1:<gateway port>. On colima/lima that is the VM, not the Mac,
# so a relay to the lima host (192.168.5.2) must be listening. mTLS rejecting
# an anonymous client (curl exit 56) proves the gateway is reachable.
set +e
docker run --rm --network host curlimages/curl:latest -sk --max-time 5 -o /dev/null https://127.0.0.1:17670/ 2>/dev/null
rc=$?
set -e
[[ $rc -eq 56 ]] || die "gateway unreachable from the docker host (curl exit $rc). On colima, start a relay:
  docker run -d --rm --name openshell-gw-relay --network host alpine/socat \\
    TCP-LISTEN:17670,bind=127.0.0.1,fork,reuseaddr TCP:192.168.5.2:17670"
pass "gateway reachable from docker host"

# 2. Build -------------------------------------------------------------------
case "$(docker version --format '{{.Server.Arch}}')" in
  arm64|aarch64) TRIPLE_DIR="linux-aarch64" ;;
  amd64|x86_64)  TRIPLE_DIR="linux-x86_64" ;;
  *) die "unsupported docker architecture" ;;
esac
RELEASE="$ROOT/target/$TRIPLE_DIR/release"
CORPUS="$ROOT/crates/agent-harness-task-verified-fix/corpus"
if [[ $SKIP_BUILD -eq 0 ]]; then
  # The commit the binary is built from, recorded in every run_started
  # audit record; "-dirty" when agent-harness/ has uncommitted changes.
  COMMIT="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
  [[ -z "$(git -C "$ROOT" status --porcelain -- . 2>/dev/null)" ]] || COMMIT="$COMMIT-dirty"
  log "2/6 build (${RUST_IMAGE%@*} @ commit $COMMIT -> target/$TRIPLE_DIR, then $IMAGE)"
  docker run --rm -v "$ROOT":/src -w /src -e CARGO_TARGET_DIR="/src/target/$TRIPLE_DIR" \
    -e AGENT_HARNESS_GIT_COMMIT="$COMMIT" "$RUST_IMAGE" \
    sh -c 'apt-get -qq update >/dev/null && apt-get -qq install -y cmake >/dev/null 2>&1
           cargo build --release --locked --quiet -p agent-harness -p agent-harness-task-verified-fix'
  rm -rf "$ROOT/.sandbox/image" && mkdir -p "$ROOT/.sandbox/image"
  cp "$RELEASE/agent-harness" "$RELEASE/verified-fix" "$ROOT/.sandbox/image/"
  cp -R "$CORPUS" "$ROOT/.sandbox/image/corpus"
  docker build -q --build-arg CBMC_PACKAGE="$CBMC_PACKAGE" --build-arg LIBC_DEV_PACKAGE="$LIBC_DEV_PACKAGE" \
    -f "$ROOT/sandbox.Dockerfile" \
    -t "$IMAGE" "$ROOT/.sandbox/image" >/dev/null 2>&1 || die "docker build failed"
else
  log "2/6 build skipped"
fi
docker image inspect "$IMAGE" >/dev/null 2>&1 || die "image $IMAGE missing; run without --skip-build"

# 3. Effective policy --------------------------------------------------------
log "3/6 effective policy"
created="$(openshell sandbox create --name "$PREFIX-pol" --from "$IMAGE" --policy "$POLICY" \
  --no-auto-providers --no-tty --detach -- sleep 120 2>&1)" \
  || die "sandbox create failed:"$'\n'"$(tail -15 <<<"$created")"
effective="$(openshell policy get "$PREFIX-pol" --full 2>&1)"
grep -q 'Status: *Effective' <<<"$effective" && pass "policy loaded and effective" || fail "policy not effective"
grep -q 'Source: *sandbox' <<<"$effective" && pass "policy source is the sandbox file" || fail "policy source is not the sandbox file"
for k in anthropic openai gemini ollama; do
  grep -Eq "^  $k:" <<<"$effective" && pass "network policy '$k' present" || fail "network policy '$k' missing"
done
cleanup

# 4. Enforcement probes ------------------------------------------------------
log "4/6 enforcement probes"
# Each probe prints "key=value"; values are compared on the host.
probes='
c() { curl -s -o /dev/null --max-time 10 "$@"; echo $?; }
echo "uid=$(id -un)"
echo "nonewprivs=$(awk "/^NoNewPrivs:/{print \$2}" /proc/self/status)"
echo "seccomp=$(awk "/^Seccomp:/{print \$2}" /proc/self/status)"
for p in /tmp /app /etc /usr /; do
  if (echo x > "$p/.probe") 2>/dev/null; then rm -f "$p/.probe"; r=allowed; else r=denied; fi
  echo "write:$p=$r"
done
for p in /var/lib /root; do ls "$p" >/dev/null 2>&1 && r=allowed || r=denied; echo "read:$p=$r"; done
echo "curl:gemini=$(c -X POST https://generativelanguage.googleapis.com/v1beta/models/x:generateContent -d {})"
echo "curl:anthropic=$(c https://api.anthropic.com/v1/messages)"
echo "curl:example=$(c https://example.com/)"
/app/agent-harness "validation probe" 2>&1 | grep -q API_KEY_INVALID && r=reached || r=blocked
echo "harness:gemini=$r"
'
out="$(openshell sandbox create --name "$PREFIX-run" --from "$IMAGE" --policy "$POLICY" \
  --no-auto-providers --no-tty --no-keep --no-credential-warnings \
  --env GEMINI_API_KEY=dummy-validation-value --env HARNESS_MODEL=gemini \
  -- sh -c "$probes" 2>&1)" || true
get() { sed -n "s|^$1=||p" <<<"$out" | head -1; }

expect "runs as sandbox user"             "$(get uid)"            sandbox
expect "no_new_privs set"                 "$(get nonewprivs)"     1
expect "seccomp in filter mode"           "$(get seccomp)"        2
expect "write /tmp"                       "$(get write:/tmp)"     allowed
expect "write /app"                       "$(get write:/app)"     denied
expect "write /etc"                       "$(get write:/etc)"     denied
expect "write /usr"                       "$(get write:/usr)"     denied
expect "write /"                          "$(get write:/)"        denied
expect "read /var/lib (not allowlisted)"  "$(get read:/var/lib)"  denied
expect "read /root"                       "$(get read:/root)"     denied
expect "curl -> gemini (binary not pinned)" "$(get curl:gemini)"  7
expect "curl -> anthropic (binary not pinned)" "$(get curl:anthropic)" 7
expect "curl -> example.com (host not listed)" "$(get curl:example)" 7
expect "agent-harness -> gemini generateContent" "$(get harness:gemini)" reached

if [[ $FAILS -gt 0 ]]; then
  echo "validate: $FAILS check(s) failed. Raw sandbox output:" >&2
  echo "$out" >&2
  exit 1
fi

# 5. Provider credential injection ------------------------------------------
log "5/6 provider credential injection (agent-harness-openai, agent-harness-gemini)"
# check_profile <file> <id>: lint rejects an id the gateway already has, and
# update needs the gateway's resource_version, so lint+import a new profile;
# for an existing one, compare its security-relevant fields with the file and
# fail on drift. Then check the profile stays within openshell-policy.yaml.
check_profile() {
  local file="$1" id="$2" drift
  if openshell profile describe "$id" >/dev/null 2>&1; then
    command -v ruby >/dev/null || die "ruby is needed to compare the imported profile"
    if drift="$(openshell profile export "$id" 2>/dev/null | ruby -ryaml -e '
        pick = ->(p) { {
          "endpoints" => p["endpoints"],
          "binaries"  => p["binaries"],
          "credentials" => p["credentials"].to_a.map { |c| c.slice("env_vars", "auth_style", "header_name") },
        } }
        gw = pick.(YAML.safe_load($stdin.read)); file = pick.(YAML.safe_load(File.read(ARGV[0])))
        gw.each_key { |k| puts "  #{k} differs" unless gw[k] == file[k] }
        exit(gw == file ? 0 : 1)' "$file")"; then
      pass "$id: imported profile matches $(basename "$file")"
    else
      fail "$id: imported profile differs from file:"$'\n'"$drift"$'\n'"  re-import: openshell profile delete $id && openshell profile import -f $file"
    fi
  else
    openshell profile lint -f "$file" >/dev/null 2>&1 || die "profile lint failed: $file"
    openshell profile import -f "$file" >/dev/null 2>&1 && pass "$id: profile linted and imported" \
      || die "profile import failed: $file"
  fi
  # A provider widens the effective policy with its own endpoints and
  # binaries: pinned binary, L7 rules only.
  grep -Eq '^binaries: \[/app/agent-harness\]$' "$file" \
    && pass "$id: pins /app/agent-harness only" || fail "$id: binaries are wider than /app/agent-harness"
  grep -Eq '^\s+access:' "$file" \
    && fail "$id: uses an access preset instead of rules" || pass "$id: uses L7 rules, no access preset"
}
check_profile "$PROFILE" "$PROFILE_ID"
check_profile "$GEMINI_PROFILE" "$GEMINI_PROFILE_ID"

openshell provider create --name "$PREFIX-oai" --type "$PROFILE_ID" \
  --credential OPENAI_API_KEY=sk-dummy-validation-0000 >/dev/null 2>&1 || die "provider create failed"
oprobe='
k="${OPENAI_API_KEY:-}"
case "$k" in openshell:*) echo "key=placeholder" ;; "") echo "key=unset" ;; *) echo "key=raw" ;; esac
echo "curl:openai=$(curl -s -o /dev/null --max-time 10 -X POST https://api.openai.com/v1/responses -H "Authorization: Bearer $k"; echo $?)"
r=$(/app/agent-harness "validation probe" 2>&1)
case "$r" in *"Incorrect API key provided: sk-dummy"*) echo "harness:openai=injected" ;; *invalid_api_key*) echo "harness:openai=not-injected" ;; *) echo "harness:openai=blocked" ;; esac
'
pout="$(openshell sandbox create --name "$PREFIX-oai-run" --from "$IMAGE" --policy "$POLICY" \
  --provider "$PREFIX-oai" --no-auto-providers --no-tty --no-keep --env HARNESS_MODEL=openai \
  -- sh -c "$oprobe" 2>&1)" || true
pget() { sed -n "s|^$1=||p" <<<"$pout" | head -1; }
expect "sandbox sees only a placeholder"      "$(pget key)"            placeholder
expect "curl with placeholder (binary not pinned)" "$(pget curl:openai)" 7
expect "proxy substitutes key for agent-harness" "$(pget harness:openai)" injected

# Gemini: Google's errors do not echo the key, so a dummy can only show the
# sandbox holds a placeholder (and curl cannot use it); substitution is shown
# by the optional live call below.
openshell provider create --name "$PREFIX-gem" --type "$GEMINI_PROFILE_ID" \
  --credential GEMINI_API_KEY=dummy-validation-value >/dev/null 2>&1 || die "gemini provider create failed"
gprobe='
k="${GEMINI_API_KEY:-}"
case "$k" in openshell:*) echo "key=placeholder" ;; "") echo "key=unset" ;; *) echo "key=raw" ;; esac
echo "curl:gemini=$(curl -s -o /dev/null --max-time 10 -X POST "https://generativelanguage.googleapis.com/v1beta/models/x:generateContent" -H "x-goog-api-key: $k"; echo $?)"
'
gout="$(openshell sandbox create --name "$PREFIX-gem-run" --from "$IMAGE" --policy "$POLICY" \
  --provider "$PREFIX-gem" --no-auto-providers --no-tty --no-keep -- sh -c "$gprobe" 2>&1)" || true
gget() { sed -n "s|^$1=||p" <<<"$gout" | head -1; }
expect "gemini: sandbox sees only a placeholder" "$(gget key)" placeholder
expect "gemini: curl with placeholder (binary not pinned)" "$(gget curl:gemini)" 7

if [[ -n "${OPENSHELL_LIVE_GEMINI_PROVIDER:-}" ]]; then
  model="${OPENSHELL_LIVE_GEMINI_MODEL:-gemini-3.5-flash}"
  log "live Gemini call via provider '$OPENSHELL_LIVE_GEMINI_PROVIDER' ($model, one request)"
  live="$(openshell sandbox create --name "$PREFIX-glive" --from "$IMAGE" --policy "$POLICY" \
    --provider "$OPENSHELL_LIVE_GEMINI_PROVIDER" --no-auto-providers --no-tty --no-keep \
    -- /app/agent-harness --model "gemini:$model" "Reply with exactly: sandbox ok" 2>&1)" || true
  grep -qi 'sandbox ok' <<<"$live" && r=ok || r=failed
  expect "live agent-harness -> Gemini ($model)" "$r" ok
  [[ $r == ok ]] || grep -E 'error|status' <<<"$live" | head -3 >&2
fi

if [[ -n "${OPENSHELL_LIVE_OPENAI_PROVIDER:-}" ]]; then
  log "live OpenAI call via provider '$OPENSHELL_LIVE_OPENAI_PROVIDER' (one request)"
  live="$(openshell sandbox create --name "$PREFIX-live" --from "$IMAGE" --policy "$POLICY" \
    --provider "$OPENSHELL_LIVE_OPENAI_PROVIDER" --no-auto-providers --no-tty --no-keep \
    --env HARNESS_MODEL=openai -- /app/agent-harness "Reply with exactly: sandbox ok" 2>&1)" || true
  grep -qi 'sandbox ok' <<<"$live" && r=ok || r=failed
  expect "live agent-harness -> OpenAI" "$r" ok
fi

if [[ $FAILS -gt 0 ]]; then
  echo "validate: $FAILS check(s) failed. Raw provider sandbox output:" >&2
  echo "$pout" >&2
  exit 1
fi

# 6. The verified-fix task in the sandbox ------------------------------------
log "6/6 verified-fix in the sandbox"
# The task declares the programs it runs; each must be readable (and so
# executable) under the policy's read_only paths. Debian trixie has a merged
# /usr: /bin and /lib are symlinks into /usr.
needs="$(docker run --rm "$IMAGE" /app/verified-fix sandbox-needs)"
command -v ruby >/dev/null || die "ruby is needed to check sandbox needs against the policy"
if gaps="$(ruby -ryaml -rjson -e '
    policy = YAML.safe_load(File.read(ARGV[0]))
    allowed = policy.dig("filesystem_policy", "read_only").to_a + policy.dig("filesystem_policy", "read_write").to_a
    needs = JSON.parse(ARGV[1])
    merged = ->(p) { p.sub(%r{\A/(bin|sbin|lib)(/|\z)}, "/usr/\\1\\2") }
    covered = ->(p) { allowed.any? { |a| m = merged.(a); m == merged.(p) || merged.(p).start_with?(m.chomp("/") + "/") } }
    gaps = needs["binaries"].reject(&covered)
    gaps += needs["egress"].map { |e| "egress #{e["host"]}:#{e["port"]} (not generated yet)" }
    puts gaps
    exit(gaps.empty? ? 0 : 1)' "$POLICY" "$needs")"; then
  pass "policy covers the task's sandbox needs ($(ruby -rjson -e 'puts JSON.parse(ARGV[0])["binaries"].join(", ")' "$needs"))"
else
  fail "policy does not cover: $gaps"
fi

# CBMC and gcc under Landlock, seccomp and the non-root user: every corpus
# original must fail and every reference fix verify, on a verified chain.
st="$(openshell sandbox create --name "$PREFIX-st" --from "$IMAGE" --policy "$POLICY" \
  --no-auto-providers --no-tty --no-keep \
  -- /app/verified-fix self-test /app/corpus --audit /tmp/self-test.jsonl 2>&1)" || true
grep -q '"self_test": "passed"' <<<"$st" && r=passed || r=failed
expect "self-test: CBMC on every corpus case in the sandbox" "$r" passed
cbmc_version="$(sed -n 's/.*"cbmc": "\([^"]*\)".*/\1/p' <<<"$st" | head -1)"
expect "CBMC identified inside the sandbox" "${cbmc_version%% *}" "6.6.0"
records="$(sed -n 's/.*"audit_records": \([0-9]*\).*/\1/p' <<<"$st" | head -1)"
[[ "${records:-0}" -gt 10 ]] && pass "self-test audit chain verified ($records records)" \
  || fail "self-test audit chain: '${records:-none}' records"

if [[ -n "${OPENSHELL_LIVE_OPENAI_PROVIDER:-}" && -n "${OPENSHELL_LIVE_FIX_CASE:-}" ]]; then
  log "live verified-fix on '$OPENSHELL_LIVE_FIX_CASE' via provider '$OPENSHELL_LIVE_OPENAI_PROVIDER' (several requests)"
  fix="$(openshell sandbox create --name "$PREFIX-fix" --from "$IMAGE" --policy "$POLICY" \
    --provider "$OPENSHELL_LIVE_OPENAI_PROVIDER" --no-auto-providers --no-tty --no-keep \
    --env HARNESS_MODEL=openai --env HARNESS_AUTO_APPROVE=1 \
    -- /app/verified-fix run "/app/corpus/$OPENSHELL_LIVE_FIX_CASE" --audit /tmp/fix.jsonl 2>&1)" || true
  grep -q '"accepted": true' <<<"$fix" && r=accepted || r=rejected
  expect "live verified-fix accepted" "$r" accepted
  [[ $r == accepted ]] || echo "$fix" | tail -40 >&2
fi

if [[ $FAILS -gt 0 ]]; then
  echo "validate: $FAILS check(s) failed. Raw self-test output:" >&2
  echo "$st" | tail -30 >&2
  exit 1
fi
log "all checks passed"
