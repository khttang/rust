#!/usr/bin/env bash
# run-bounded.sh — locally mock the layout declared in openshell-policy.yaml,
# then launch the agent-harness REPL.
#
# This is a MOCK. It enforces nothing: no Landlock, no seccomp, no egress
# proxy. It checks the policy, maps its sandbox paths to local stand-ins,
# mirrors the non-root rule, and runs the binary. Real enforcement comes
# from OpenShell.
#
#   policy path          local stand-in
#   /app/agent-harness   target/release/agent-harness
#   /tmp (read_write)    .sandbox/tmp (mode 700, exported as TMPDIR)
#
# Usage: ./run-bounded.sh [--dry-run] [agent-harness args…]
#   e.g. ./run-bounded.sh --model ollama:llama3.2:3b
#
# Touches nothing outside the directory this script lives in.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
POLICY="$ROOT/openshell-policy.yaml"
BIN="$ROOT/target/release/agent-harness"
SANDBOX_BIN="/app/agent-harness"
MOCK_TMP="$ROOT/.sandbox/tmp"
DRY_RUN=0

die() { echo "run-bounded: error: $*" >&2; exit 1; }
log() { echo "run-bounded: $*" >&2; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) break ;;
  esac
done

# 1. Policy: required structure, checked with grep so no parser is needed.
[[ -f "$POLICY" ]] || die "missing $POLICY"
grep -Eq '^version:[[:space:]]*1[[:space:]]*$' "$POLICY" || die "policy must declare 'version: 1'"
for section in filesystem_policy landlock process network_policies; do
  grep -Eq "^${section}:" "$POLICY" || die "policy has no '$section' section"
done
grep -Eq "^[[:space:]]*-[[:space:]]*${SANDBOX_BIN}([[:space:]]|$)" "$POLICY" \
  || die "policy does not list $SANDBOX_BIN as read_only"
grep -Eq "^[[:space:]]*-[[:space:]]*path:[[:space:]]*${SANDBOX_BIN}([[:space:]]|$)" "$POLICY" \
  || die "policy does not list $SANDBOX_BIN under binaries"
log "policy: $POLICY"

# 2. Policy: schema rules, when Ruby (with its bundled YAML parser) exists.
if command -v ruby >/dev/null 2>&1; then
  ruby -ryaml -e '
    p = YAML.safe_load(File.read(ARGV[0]))
    errs = []
    errs << "version must be integer 1" unless p["version"] == 1
    extra = p.keys - %w[version filesystem_policy landlock process network_policies network_middlewares]
    errs << "unknown top-level keys: #{extra.join(", ")}" unless extra.empty?
    fs = p["filesystem_policy"] || {}
    (fs["read_only"].to_a + fs["read_write"].to_a).each do |x|
      errs << "bad path #{x.inspect}" unless x.is_a?(String) && x.start_with?("/") && !x.include?("..")
    end
    errs << "read_write must not be /" if fs["read_write"].to_a.include?("/")
    mode = (p["landlock"] || {})["compatibility"]
    errs << "landlock.compatibility: #{mode.inspect}" unless [nil, "best_effort", "hard_requirement"].include?(mode)
    (p["process"] || {}).each { |k, v| errs << "process.#{k} must not be root" if v.to_s == "0" }
    (p["network_policies"] || {}).each do |k, e|
      errs << "#{k}: endpoints and binaries are required" if e["endpoints"].to_a.empty? || e["binaries"].to_a.empty?
      e["endpoints"].to_a.each do |ep|
        errs << "#{k}: port must be an integer" unless ep["port"].is_a?(Integer)
        if %w[rest websocket graphql].include?(ep["protocol"]) && ep.key?("access") == ep.key?("rules")
          errs << "#{k}: #{ep["protocol"]} needs exactly one of access/rules"
        end
        if ep.key?("rules")
          rules = ep["rules"]
          if !rules.is_a?(Array) || rules.empty?
            errs << "#{k}: rules must be a non-empty list"
          elsif ep["protocol"] == "rest" && rules.any? { |r| !r.is_a?(Hash) || !r["allow"].is_a?(Hash) || (%w[method path] - r["allow"].keys).any? }
            errs << "#{k}: each rest rule needs allow.method and allow.path"
          end
        end
        if ep.key?("access") && !%w[read-only read-write full].include?(ep["access"])
          errs << "#{k}: access must be read-only, read-write or full"
        end
      end
    end
    abort errs.map { |e| "  - #{e}" }.unshift("policy schema check failed:").join("\n") unless errs.empty?
  ' "$POLICY" || die "fix $POLICY"
  log "policy schema check: ok"
else
  log "ruby not found; skipping policy schema check"
fi

# 3. process.run_as_user forbids root; mirror that locally.
[[ $(id -u) -ne 0 ]] || die "refusing to run as root (policy: process.run_as_user)"

# 4. read_write /tmp -> private, project-local TMPDIR. Refuse symlinks so it
#    cannot be redirected outside the project root.
for d in "$ROOT/.sandbox" "$MOCK_TMP"; do
  [[ ! -L "$d" ]] || die "$d is a symlink; refusing"
  [[ ! -e "$d" || -d "$d" ]] || die "$d exists and is not a directory"
done
if [[ $DRY_RUN -eq 1 ]]; then
  log "dry-run: would create $MOCK_TMP (mode 700) as TMPDIR"
else
  (umask 077 && mkdir -p "$MOCK_TMP")
  chmod 700 "$ROOT/.sandbox" "$MOCK_TMP"
  resolved="$(cd "$MOCK_TMP" && pwd -P)"
  [[ "$resolved" == "$ROOT/"* ]] || die "mock tmp resolved outside project root: $resolved"
  export TMPDIR="$MOCK_TMP"
  log "mock /tmp: $MOCK_TMP (mode 700)"
fi

# 5. Report which provider credentials are present. Never print values.
#    In the sandbox these are OpenShell placeholders, not real keys.
for var in ANTHROPIC_API_KEY OPENAI_API_KEY; do
  if [[ -n "${!var:-}" ]]; then log "$var: set"; else log "$var: missing"; fi
done
case " $* ${HARNESS_MODEL:-} " in
  *ollama*)
    log "OLLAMA_API_BASE_URL: ${OLLAMA_API_BASE_URL:-unset (localhost)};" \
        "in the sandbox use http://host.openshell.internal:11434" ;;
esac

# 6. /app/agent-harness -> the local release build.
if [[ $DRY_RUN -eq 1 ]]; then
  log "dry-run: would build and exec $BIN (as $SANDBOX_BIN) $*"
  exit 0
fi
cargo build --release --quiet --manifest-path "$ROOT/Cargo.toml"
[[ -x "$BIN" ]] || die "binary not found at $BIN"
log "mock $SANDBOX_BIN: $BIN"
cd "$ROOT"
exec "$BIN" "$@"
