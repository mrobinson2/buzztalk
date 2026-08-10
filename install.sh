#!/bin/sh
set -eu

die() {
    printf 'buzztalk installer: %s\n' "$*" >&2
    exit 1
}

download() {
    source_url=$1
    destination=$2
    if command -v curl >/dev/null 2>&1; then
        curl --fail --location --silent --show-error --output "$destination" "$source_url"
    elif command -v wget >/dev/null 2>&1; then
        wget --quiet --output-document="$destination" "$source_url"
    else
        die 'curl or wget is required to download a release'
    fi
}

download_optional() {
    source_url=$1
    destination=$2
    if command -v curl >/dev/null 2>&1; then
        curl --fail --location --silent --output "$destination" "$source_url"
    elif command -v wget >/dev/null 2>&1; then
        wget --quiet --output-document="$destination" "$source_url"
    else
        return 1
    fi
}

download_stdout() {
    source_url=$1
    if command -v curl >/dev/null 2>&1; then
        curl --fail --location --silent --show-error "$source_url"
    elif command -v wget >/dev/null 2>&1; then
        wget --quiet --output-document=- "$source_url"
    else
        die 'curl or wget is required to resolve the newest release'
    fi
}

os=$(uname -s)
arch=$(uname -m)
case "$os:$arch" in
    Darwin:arm64 | Darwin:aarch64) target=macos-arm64 ;;
    Linux:x86_64 | Linux:amd64) target=linux-x86_64 ;;
    *) die "unsupported platform: $os $arch" ;;
esac

install_dir=${BUZZTALK_INSTALL_DIR:-"${HOME}/.local/bin"}
release_base=${BUZZTALK_RELEASE_BASE_URL:-https://github.com/mrobinson2/buzztalk/releases}
archive_name="buzztalk-$target.tar.gz"
install_files="buzztalkd buzztalk-demo"
if [ "$target" = macos-arm64 ] || [ "$target" = macos-x86_64 ]; then
    install_files="$install_files buzztalk-gateway"
fi

if [ -n "${BUZZTALK_VERSION:-}" ]; then
    version=$BUZZTALK_VERSION
else
    release_api=${BUZZTALK_RELEASE_API_URL:-https://api.github.com/repos/mrobinson2/buzztalk/releases?per_page=10}
    release_json=$(download_stdout "$release_api") ||
        die 'could not resolve the newest GitHub release'
    # Walk top-level release objects without requiring jq. GitHub returns
    # newest-first and includes prereleases; drafts are explicitly skipped.
    version=$(printf '%s\n' "$release_json" | awk '
        BEGIN {
            depth = 0
            in_string = 0
            escaped = 0
            tag_pattern = "\"tag_name\"[[:space:]]*:[[:space:]]*\""
        }
        {
            line = $0 "\n"
            for (i = 1; i <= length(line); i++) {
                ch = substr(line, i, 1)
                if (!in_string && ch == "{") {
                    depth++
                    if (depth == 1) object = ""
                }
                if (depth > 0) object = object ch
                if (in_string) {
                    if (escaped) escaped = 0
                    else if (ch == "\\") escaped = 1
                    else if (ch == "\"") in_string = 0
                    continue
                }
                if (ch == "\"") {
                    in_string = 1
                    continue
                }
                if (ch == "}") {
                    depth--
                    if (depth == 0 && object !~ /\"draft\"[[:space:]]*:[[:space:]]*true/) {
                        if (match(object, tag_pattern)) {
                            tag = substr(object, RSTART + RLENGTH)
                            sub(/\".*/, "", tag)
                            print tag
                            exit
                        }
                    }
                }
            }
        }
    ')
    [ -n "$version" ] || die 'GitHub releases response contained no non-draft release tag'
fi
release_url="$release_base/download/$version"

mkdir -p "$install_dir"
stage_dir=$(mktemp -d "$install_dir/.buzztalk-install.XXXXXX") ||
    die "could not create a staging directory in $install_dir"
cleanup() {
    rm -rf "$stage_dir"
}
replacement_started=0
rollback_installation() {
    rollback_failed=0
    for restore_binary in $install_files; do
        # A same-filesystem rename removes the staged source. If it is still
        # present, that binary was never replaced and must be left alone.
        if [ -e "$payload/$restore_binary" ]; then
            continue
        fi
        if [ -e "$backup/$restore_binary" ]; then
            if ! mv -f "$backup/$restore_binary" "$install_dir/$restore_binary"; then
                rollback_failed=1
            fi
        elif ! rm -f "$install_dir/$restore_binary"; then
            rollback_failed=1
        fi
    done
    return "$rollback_failed"
}
handle_signal() {
    signal_status=$1
    trap - EXIT HUP INT TERM
    if [ "$replacement_started" -eq 1 ] && ! rollback_installation; then
        printf '%s\n' 'buzztalk installer: interrupted; rollback failed' >&2
    else
        printf '%s\n' 'buzztalk installer: interrupted; the previous installation was restored' >&2
    fi
    cleanup
    exit "$signal_status"
}
trap cleanup EXIT
trap 'handle_signal 129' HUP
trap 'handle_signal 130' INT
trap 'handle_signal 143' TERM

manifest="$stage_dir/SHA256SUMS"
archive="$stage_dir/$archive_name"
payload="$stage_dir/payload"

checksum_kind=manifest
if ! download_optional "$release_url/SHA256SUMS" "$manifest"; then
    checksum_kind=archive
    checksum_asset="buzztalk-$target.sha256"
    download "$release_url/$checksum_asset" "$manifest" ||
        die "could not download checksum data for $version"
fi
download "$release_url/$archive_name" "$archive" ||
    die "could not download release archive $archive_name"

if [ "$checksum_kind" = manifest ]; then
    expected=$(awk -v name="$archive_name" '
        $2 == name || $2 == "*" name { print tolower($1); exit }
    ' "$manifest")
else
    expected=$(awk -v name="$archive_name" '
        NF == 1 { print tolower($1); exit }
        $2 == name || $2 == "*" name { print tolower($1); exit }
    ' "$manifest")
fi
[ "${#expected}" -eq 64 ] || die "checksum manifest has no SHA-256 for $archive_name"
case "$expected" in
    *[!0-9a-f]*) die "checksum manifest has an invalid SHA-256 for $archive_name" ;;
esac

if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$archive" | awk '{print tolower($1)}')
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$archive" | awk '{print tolower($1)}')
else
    die 'sha256sum or shasum is required to verify the release'
fi
[ "$actual" = "$expected" ] ||
    die "checksum verification failed for $archive_name"

mkdir "$payload"
tar -xzf "$archive" -C "$payload" || die "could not extract $archive_name"
for binary in $install_files; do
    [ -f "$payload/$binary" ] || die "release archive is missing $binary"
    chmod +x "$payload/$binary"
done

# Staging lives on the destination filesystem, so each replacement is an
# atomic rename. Keep recoverable copies until the complete pair is installed
# so a failure cannot leave binaries from two different releases.
backup="$stage_dir/backup"
mkdir "$backup"
for binary in $install_files; do
    if [ -e "$install_dir/$binary" ]; then
        cp -p "$install_dir/$binary" "$backup/$binary" ||
            die "could not back up the existing $binary"
    fi
done

replacement_started=1
for binary in $install_files; do
    if ! mv -f "$payload/$binary" "$install_dir/$binary"; then
        if ! rollback_installation; then
            replacement_started=0
            die "could not replace $binary; rollback also failed"
        fi
        replacement_started=0
        die "could not replace $binary; the previous installation was restored"
    fi
done
replacement_started=0

printf 'Installed BuzzTalk %s to %s\n' "$version" "$install_dir"
if [ "$target" = macos-arm64 ] || [ "$target" = macos-x86_64 ]; then
    printf '%s\n' 'Gateway helper installed as buzztalk-gateway. Run: buzztalk-gateway configure'
fi
printf '%s\n' 'Speech models were not downloaded. Model download remains opt-in via buzztalkd --download-models.'
