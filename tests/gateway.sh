#!/bin/sh
set -u

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/buzztalk-gateway-test.XXXXXX")
trap 'if [ -n "${RUNNING_PID:-}" ]; then kill "$RUNNING_PID" 2>/dev/null || true; fi; rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

passed=0
failed=0

run_test() {
    name=$1
    shift
    if "$@"; then
        printf 'ok - %s\n' "$name"
        passed=$((passed + 1))
    else
        printf 'not ok - %s\n' "$name"
        failed=$((failed + 1))
    fi
}

new_fixture() {
    fixture=$1
    FIXTURE_ROOT="$TMP_ROOT/$fixture"
    CONFIG_DIR="$FIXTURE_ROOT/config"
    STATE_DIR="$FIXTURE_ROOT/state"
    BIN_DIR="$FIXTURE_ROOT/bin"
    mkdir -p "$CONFIG_DIR" "$STATE_DIR" "$BIN_DIR"
    KEY_FILE="$FIXTURE_ROOT/key with spaces"
    printf 'secret-that-must-not-be-read\n' > "$KEY_FILE"
    FAKE_BIN="$BIN_DIR/fake gateway"
    cat > "$FAKE_BIN" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" > "$BUZZTALK_TEST_ARGS"
trap 'exit 0' TERM INT
while :; do sleep 1; done
EOF
    chmod +x "$FAKE_BIN"
    cat > "$CONFIG_DIR/gateway.conf" <<EOF
RELAY=wss://relay.example.test/path with spaces
CHANNEL=channel-id
AGENT_PUBKEY=npub1-example
KEY_FILE=$KEY_FILE
HEADPHONES=true
ENDPOINT_SILENCE_MS=700
VPIO=true
EOF
}

gateway() {
    BUZZTALK_GATEWAY_CONFIG_DIR="$CONFIG_DIR" \
        BUZZTALK_GATEWAY_STATE_DIR="$STATE_DIR" \
        BUZZTALKD_BIN="$FAKE_BIN" \
        BUZZTALK_GATEWAY_STARTUP_WAIT_MS=10 \
        BUZZTALK_TEST_ARGS="$FIXTURE_ROOT/args" \
        sh "$ROOT/scripts/buzztalk-gateway" "$@"
}

test_missing_config_is_actionable() {
    new_fixture missing
    rm -f "$CONFIG_DIR/gateway.conf"
    if gateway on >"$FIXTURE_ROOT/out" 2>"$FIXTURE_ROOT/err"; then return 1; fi
    grep -Fx 'Configuration not found. Run: buzztalk-gateway configure' "$FIXTURE_ROOT/err" >/dev/null
}

test_macos_launcher_includes_vpio_when_enabled() {
    new_fixture vpio
    output=$(gateway on 2>&1) || return 1
    case "$output" in *"running"*) : ;; *) return 1 ;; esac
    grep -F -- '--vpio' "$FIXTURE_ROOT/args" >/dev/null
}

test_on_passes_literal_arguments_and_hides_sensitive_values() {
    new_fixture literal
    sed 's/^VPIO=true$/VPIO=false/' "$CONFIG_DIR/gateway.conf" > "$CONFIG_DIR/next"
    mv "$CONFIG_DIR/next" "$CONFIG_DIR/gateway.conf"
    output=$(gateway on 2>&1) || return 1
    case "$output" in *"running"*) : ;; *) return 1 ;; esac
    grep -F -- '--relay wss://relay.example.test/path with spaces' "$FIXTURE_ROOT/args" >/dev/null || return 1
    grep -F -- '--key-file ' "$FIXTURE_ROOT/args" >/dev/null || return 1
    ! grep -F 'secret-that-must-not-be-read' "$FIXTURE_ROOT/args" >/dev/null || return 1
    ! printf '%s\n' "$output" | grep -E 'relay\.example|channel-id|npub1|key with spaces|secret-that' >/dev/null
}

test_lifecycle_is_idempotent_and_status_exit_codes_are_stable() {
    new_fixture lifecycle
    sed 's/^VPIO=true$/VPIO=false/' "$CONFIG_DIR/gateway.conf" > "$CONFIG_DIR/next"
    mv "$CONFIG_DIR/next" "$CONFIG_DIR/gateway.conf"
    gateway on >/dev/null 2>&1 || return 1
    second=$(gateway on 2>&1) || return 1
    case "$second" in *"already running"*) : ;; *) return 1 ;; esac
    gateway status >/dev/null 2>&1 || return 1
    gateway off >/dev/null 2>&1 || return 1
    if gateway status >"$FIXTURE_ROOT/status.out" 2>&1; then return 1; fi
    grep -Fx 'BuzzTalk gateway is stopped.' "$FIXTURE_ROOT/status.out" >/dev/null
}

test_startup_wait_override_is_validated() {
    new_fixture wait
    sed 's/^VPIO=true$/VPIO=false/' "$CONFIG_DIR/gateway.conf" > "$CONFIG_DIR/next"
    mv "$CONFIG_DIR/next" "$CONFIG_DIR/gateway.conf"
    if BUZZTALK_GATEWAY_CONFIG_DIR="$CONFIG_DIR" BUZZTALK_GATEWAY_STATE_DIR="$STATE_DIR" \
        BUZZTALKD_BIN="$FAKE_BIN" BUZZTALK_GATEWAY_STARTUP_WAIT_MS=0 \
        sh "$ROOT/scripts/buzztalk-gateway" on >"$FIXTURE_ROOT/out" 2>"$FIXTURE_ROOT/err"; then
        return 1
    fi
    grep -Fx 'BUZZTALK_GATEWAY_STARTUP_WAIT_MS must be a positive integer.' "$FIXTURE_ROOT/err" >/dev/null
}

test_key_validation_never_reads_contents() {
    new_fixture key
    missing="$FIXTURE_ROOT/missing-key"
    sed "s#^KEY_FILE=.*#KEY_FILE=$missing#; s/^VPIO=true$/VPIO=false/" "$CONFIG_DIR/gateway.conf" > "$CONFIG_DIR/next"
    mv "$CONFIG_DIR/next" "$CONFIG_DIR/gateway.conf"
    if gateway on >"$FIXTURE_ROOT/out" 2>"$FIXTURE_ROOT/err"; then return 1; fi
    grep -Fx 'Signing key file is missing or unreadable. Check KEY_FILE and run configure again.' "$FIXTURE_ROOT/err" >/dev/null || return 1
    ! grep -F "$missing" "$FIXTURE_ROOT/err" >/dev/null || return 1
    ! grep -F 'secret-that-must-not-be-read' "$FIXTURE_ROOT/err" >/dev/null
}

run_test 'missing config gives configure action' test_missing_config_is_actionable
run_test 'macOS launcher includes VPIO when enabled' test_macos_launcher_includes_vpio_when_enabled
run_test 'on preserves literal arguments and hides sensitive values' test_on_passes_literal_arguments_and_hides_sensitive_values
run_test 'lifecycle is idempotent with stable status codes' test_lifecycle_is_idempotent_and_status_exit_codes_are_stable
run_test 'startup wait override rejects zero' test_startup_wait_override_is_validated
run_test 'key validation does not read or echo contents' test_key_validation_never_reads_contents

printf '%s passed; %s failed\n' "$passed" "$failed"
[ "$failed" -eq 0 ]
