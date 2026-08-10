#!/bin/sh
set -u

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/buzztalk-install-test.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

passed=0
failed=0

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

host_target() {
    case "$(uname -s):$(uname -m)" in
        Darwin:arm64 | Darwin:aarch64) printf '%s\n' macos-arm64 ;;
        Darwin:x86_64) printf '%s\n' macos-x86_64 ;;
        Linux:x86_64 | Linux:amd64) printf '%s\n' linux-x86_64 ;;
        Linux:arm64 | Linux:aarch64) printf '%s\n' linux-arm64 ;;
        *) return 1 ;;
    esac
}

make_unix_release() {
    version=$1
    target=$2
    marker=$3
    release_dir="$TMP_ROOT/releases/download/$version"
    payload="$TMP_ROOT/payload-$marker"
    mkdir -p "$release_dir" "$payload"
    printf '#!/bin/sh\nprintf "%%s\\n" "%s-daemon"\n' "$marker" > "$payload/buzztalkd"
    printf '#!/bin/sh\nprintf "%%s\\n" "%s-demo"\n' "$marker" > "$payload/buzztalk-demo"
    chmod +x "$payload/buzztalkd" "$payload/buzztalk-demo"
    archive="buzztalk-$target.tar.gz"
    tar -czf "$release_dir/$archive" -C "$payload" buzztalkd buzztalk-demo
    hash=$(sha256_file "$release_dir/$archive")
    printf '%s  %s\n' "$hash" "$archive" > "$release_dir/SHA256SUMS"
}

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

test_installs_selected_version_and_is_idempotent() {
    target=$(host_target) || return 1
    make_unix_release v9.8.7 "$target" selected
    install_dir="$TMP_ROOT/bin"
    base_url="file://$TMP_ROOT/releases"

    BUZZTALK_VERSION=v9.8.7 \
        BUZZTALK_INSTALL_DIR="$install_dir" \
        BUZZTALK_RELEASE_BASE_URL="$base_url" \
        sh "$ROOT/install.sh" >"$TMP_ROOT/install.out" 2>"$TMP_ROOT/install.err" || return 1

    [ -x "$install_dir/buzztalkd" ] || return 1
    [ -x "$install_dir/buzztalk-demo" ] || return 1
    [ "$("$install_dir/buzztalkd")" = selected-daemon ] || return 1
    [ "$("$install_dir/buzztalk-demo")" = selected-demo ] || return 1
    [ ! -e "$install_dir/models" ] || return 1

    BUZZTALK_VERSION=v9.8.7 \
        BUZZTALK_INSTALL_DIR="$install_dir" \
        BUZZTALK_RELEASE_BASE_URL="$base_url" \
        sh "$ROOT/install.sh" >/dev/null 2>"$TMP_ROOT/rerun.err" || return 1
    [ "$("$install_dir/buzztalkd")" = selected-daemon ]
}

test_checksum_failure_preserves_existing_install() {
    target=$(host_target) || return 1
    make_unix_release v9.8.8 "$target" corrupt
    release_dir="$TMP_ROOT/releases/download/v9.8.8"
    archive="buzztalk-$target.tar.gz"
    printf '%064d  %s\n' 0 "$archive" > "$release_dir/SHA256SUMS"
    install_dir="$TMP_ROOT/existing-bin"
    mkdir -p "$install_dir"
    printf '#!/bin/sh\necho old-daemon\n' > "$install_dir/buzztalkd"
    printf '#!/bin/sh\necho old-demo\n' > "$install_dir/buzztalk-demo"
    chmod +x "$install_dir/buzztalkd" "$install_dir/buzztalk-demo"

    if BUZZTALK_VERSION=v9.8.8 \
        BUZZTALK_INSTALL_DIR="$install_dir" \
        BUZZTALK_RELEASE_BASE_URL="file://$TMP_ROOT/releases" \
        sh "$ROOT/install.sh" >"$TMP_ROOT/bad.out" 2>"$TMP_ROOT/bad.err"; then
        return 1
    fi
    grep -qi 'checksum' "$TMP_ROOT/bad.err" || return 1
    [ "$("$install_dir/buzztalkd")" = old-daemon ] || return 1
    [ "$("$install_dir/buzztalk-demo")" = old-demo ]
}

test_second_replacement_failure_rolls_back_first_binary() {
    target=$(host_target) || return 1
    make_unix_release v9.8.10 "$target" replacement
    install_dir="$TMP_ROOT/rollback-bin"
    mkdir -p "$install_dir"
    printf '#!/bin/sh\necho old-daemon\n' > "$install_dir/buzztalkd"
    printf '#!/bin/sh\necho old-demo\n' > "$install_dir/buzztalk-demo"
    chmod +x "$install_dir/buzztalkd" "$install_dir/buzztalk-demo"

    fake_bin="$TMP_ROOT/failing-mv-bin"
    mkdir -p "$fake_bin"
    cat > "$fake_bin/mv" <<'EOF'
#!/bin/sh
count=0
if [ -f "$BUZZTALK_MV_COUNT_FILE" ]; then
    count=$(cat "$BUZZTALK_MV_COUNT_FILE")
fi
count=$((count + 1))
printf '%s\n' "$count" > "$BUZZTALK_MV_COUNT_FILE"
if [ "$count" -eq 2 ]; then
    printf '%s\n' 'simulated second replacement failure' >&2
    exit 1
fi
exec "$BUZZTALK_REAL_MV" "$@"
EOF
    chmod +x "$fake_bin/mv"

    if PATH="$fake_bin:$PATH" \
        BUZZTALK_REAL_MV="$(command -v mv)" \
        BUZZTALK_MV_COUNT_FILE="$TMP_ROOT/mv-count" \
        BUZZTALK_VERSION=v9.8.10 \
        BUZZTALK_INSTALL_DIR="$install_dir" \
        BUZZTALK_RELEASE_BASE_URL="file://$TMP_ROOT/releases" \
        sh "$ROOT/install.sh" >"$TMP_ROOT/rollback.out" 2>"$TMP_ROOT/rollback.err"; then
        return 1
    fi

    grep -qi 'replacement failure' "$TMP_ROOT/rollback.err" || return 1
    [ "$("$install_dir/buzztalkd")" = old-daemon ] || return 1
    [ "$("$install_dir/buzztalk-demo")" = old-demo ]
}

test_unset_version_installs_newest_prerelease() {
    target=$(host_target) || return 1
    make_unix_release v9.8.9 "$target" prerelease
    release_dir="$TMP_ROOT/releases/download/v9.8.9"
    mv "$release_dir/SHA256SUMS" "$release_dir/buzztalk-$target.sha256"
    cat > "$TMP_ROOT/releases.json" <<'EOF'
[
  {
    "tag_name": "v10.0.0-draft",
    "draft": true,
    "prerelease": false
  },
  {
    "tag_name": "v9.8.9",
    "draft": false,
    "prerelease": true
  }
]
EOF
    install_dir="$TMP_ROOT/prerelease-bin"

    BUZZTALK_INSTALL_DIR="$install_dir" \
        BUZZTALK_RELEASE_BASE_URL="file://$TMP_ROOT/releases" \
        BUZZTALK_RELEASE_API_URL="file://$TMP_ROOT/releases.json" \
        sh "$ROOT/install.sh" >"$TMP_ROOT/prerelease.out" 2>"$TMP_ROOT/prerelease.err" || return 1

    [ "$("$install_dir/buzztalkd")" = prerelease-daemon ] || return 1
    grep -q 'Installed BuzzTalk v9.8.9' "$TMP_ROOT/prerelease.out"
}

test_unsupported_platform_is_clear() {
    fake_bin="$TMP_ROOT/fake-bin"
    mkdir -p "$fake_bin"
    cat > "$fake_bin/uname" <<'EOF'
#!/bin/sh
case "$1" in
    -s) echo Plan9 ;;
    -m) echo mips ;;
    *) echo Plan9 ;;
esac
EOF
    chmod +x "$fake_bin/uname"
    if PATH="$fake_bin:$PATH" sh "$ROOT/install.sh" >"$TMP_ROOT/platform.out" 2>"$TMP_ROOT/platform.err"; then
        return 1
    fi
    grep -qi 'unsupported platform.*Plan9.*mips' "$TMP_ROOT/platform.err"
}

run_test 'selected version installs both binaries and reruns safely' test_installs_selected_version_and_is_idempotent
run_test 'unset version installs prerelease with per-archive checksum' test_unset_version_installs_newest_prerelease
run_test 'checksum failure keeps the previous installation' test_checksum_failure_preserves_existing_install
run_test 'second replacement failure rolls back the first binary' test_second_replacement_failure_rolls_back_first_binary
run_test 'unsupported platforms fail with a clear diagnostic' test_unsupported_platform_is_clear

printf '%s passed; %s failed\n' "$passed" "$failed"
[ "$failed" -eq 0 ]
