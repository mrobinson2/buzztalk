#!/bin/sh
set -u

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/buzztalk-release-test.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

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

make_fake_gh() {
    fake_bin=$1
    mkdir -p "$fake_bin"
    cat > "$fake_bin/gh" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$BUZZTALK_GH_LOG"
if [ "$1 $2" = 'release view' ]; then
    exit "$BUZZTALK_GH_VIEW_STATUS"
fi
EOF
    chmod +x "$fake_bin/gh"
}

test_creates_missing_release_before_upload() {
    fake_bin="$TMP_ROOT/missing-release-bin"
    log="$TMP_ROOT/missing-release.log"
    dist="$TMP_ROOT/missing-dist"
    make_fake_gh "$fake_bin"
    mkdir -p "$dist"
    : > "$dist/archive.tar.gz"
    : > "$dist/archive.sha256"

    PATH="$fake_bin:$PATH" \
        BUZZTALK_GH_LOG="$log" \
        BUZZTALK_GH_VIEW_STATUS=1 \
        sh "$ROOT/scripts/attach-release.sh" v9.9.9 owner/repo "$dist" || return 1

    sed -n '1p' "$log" | grep -Fx 'release view v9.9.9 --repo owner/repo' >/dev/null || return 1
    sed -n '2p' "$log" | grep -Fx 'release create v9.9.9 --verify-tag --draft --generate-notes --title v9.9.9 --repo owner/repo' >/dev/null || return 1
    sed -n '3p' "$log" | grep -F 'release upload v9.9.9 ' >/dev/null || return 1
    sed -n '3p' "$log" | grep -F -- '--clobber --repo owner/repo' >/dev/null
}

test_reuses_existing_release_before_upload() {
    fake_bin="$TMP_ROOT/existing-release-bin"
    log="$TMP_ROOT/existing-release.log"
    dist="$TMP_ROOT/existing-dist"
    make_fake_gh "$fake_bin"
    mkdir -p "$dist"
    : > "$dist/archive.zip"

    PATH="$fake_bin:$PATH" \
        BUZZTALK_GH_LOG="$log" \
        BUZZTALK_GH_VIEW_STATUS=0 \
        sh "$ROOT/scripts/attach-release.sh" v9.9.8 owner/repo "$dist" || return 1

    sed -n '1p' "$log" | grep -Fx 'release view v9.9.8 --repo owner/repo' >/dev/null || return 1
    [ "$(wc -l < "$log" | tr -d ' ')" -eq 2 ] || return 1
    sed -n '2p' "$log" | grep -F 'release upload v9.9.8 ' >/dev/null || return 1
    ! grep -q 'release create' "$log"
}

run_test 'missing release is created before assets are uploaded' test_creates_missing_release_before_upload
run_test 'existing release is reused before assets are uploaded' test_reuses_existing_release_before_upload

printf '%s passed; %s failed\n' "$passed" "$failed"
[ "$failed" -eq 0 ]
