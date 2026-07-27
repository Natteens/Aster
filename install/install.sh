#!/bin/sh
set -eu

PRODUCT=aster
TARGET=linux-x64
DEFAULT_BASE_URL=https://github.com/Natteens/Aster/releases/latest/download
ARCHIVE_NAME=aster-linux-x64.tar.gz
CHECKSUM_NAME=aster-linux-x64.tar.gz.sha256
MAX_ARCHIVE_BYTES=268435456
MAX_CHECKSUM_BYTES=4096

fail() {
    printf '%s\n' "error: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "Required tool not found: $1"
}

case "$(uname -s)" in
    Linux) ;;
    *) fail "This installer supports Linux x64 only." ;;
esac

case "$(uname -m)" in
    x86_64|amd64) ;;
    *) fail "Unsupported Linux architecture: $(uname -m). Expected x86_64." ;;
esac

for tool in curl tar gzip mktemp sed grep wc cp mv chmod find dirname cut tr cat; do
    require_command "$tool"
done

if command -v sha256sum >/dev/null 2>&1; then
    HASH_TOOL=sha256sum
elif command -v shasum >/dev/null 2>&1; then
    HASH_TOOL=shasum
else
    fail "SHA-256 tool not found. Install sha256sum or shasum."
fi

if [ "${ASTER_INSTALL_BASE_URL+x}" = x ]; then
    BASE_URL=$ASTER_INSTALL_BASE_URL
else
    BASE_URL=$DEFAULT_BASE_URL
fi
ALLOW_INSECURE=${ASTER_INSTALL_ALLOW_INSECURE:-0}
assert_safe_url() {
    url=$1
    [ -n "$url" ] || fail "ASTER_INSTALL_BASE_URL must not be empty."
    if printf '%s' "$url" | LC_ALL=C grep '[[:cntrl:]]' >/dev/null; then
        fail "ASTER_INSTALL_BASE_URL must not contain control characters."
    fi
    case "$url" in
        https://*) ;;
        http://*)
            [ "$ALLOW_INSECURE" = 1 ] ||
                fail "ASTER_INSTALL_BASE_URL must use HTTPS. Set ASTER_INSTALL_ALLOW_INSECURE=1 only for local tests."
            ;;
        *) fail "ASTER_INSTALL_BASE_URL must be an absolute HTTP or HTTPS URL." ;;
    esac
    authority=${url#*://}
    authority=${authority%%/*}
    case "$authority" in
        *"@"*) fail "ASTER_INSTALL_BASE_URL must not contain credentials." ;;
        "") fail "ASTER_INSTALL_BASE_URL must contain a host." ;;
    esac
}
assert_safe_url "$BASE_URL"

[ -n "${HOME:-}" ] || fail "HOME is not available."
if [ "${ASTER_INSTALL_DIR+x}" = x ]; then
    INSTALL_INPUT=$ASTER_INSTALL_DIR
else
    INSTALL_INPUT=$HOME/.aster
fi
[ -n "$INSTALL_INPUT" ] || fail "ASTER_INSTALL_DIR must not be empty."
if printf '%s' "$INSTALL_INPUT" | LC_ALL=C grep '[[:cntrl:]]' >/dev/null; then
    fail "ASTER_INSTALL_DIR must not contain control characters."
fi
case "$INSTALL_INPUT" in
    *"/../"*|*/..|../*|..) fail "ASTER_INSTALL_DIR must not contain unresolved '..' components." ;;
esac
case "$INSTALL_INPUT" in
    /*) INSTALL_DIR=$INSTALL_INPUT ;;
    *) INSTALL_DIR=$PWD/$INSTALL_INPUT ;;
esac
while [ "${INSTALL_DIR%/}" != "$INSTALL_DIR" ]; do INSTALL_DIR=${INSTALL_DIR%/}; done
[ -n "$INSTALL_DIR" ] || fail "ASTER_INSTALL_DIR must not be a filesystem root."
[ "$INSTALL_DIR" != / ] || fail "ASTER_INSTALL_DIR must not be a filesystem root."
[ "$INSTALL_DIR" != "$HOME" ] || fail "ASTER_INSTALL_DIR must not be the entire home directory."
PROBE=$INSTALL_DIR
while [ "$PROBE" != / ]; do
    [ ! -L "$PROBE" ] || fail "ASTER_INSTALL_DIR must not traverse a symlink."
    PROBE=$(dirname "$PROBE")
done
if [ -e "$INSTALL_DIR" ] && [ ! -d "$INSTALL_DIR" ]; then
    fail "ASTER_INSTALL_DIR exists and is not a directory."
fi
if [ -d "$INSTALL_DIR/.git" ] && [ -f "$INSTALL_DIR/Cargo.toml" ]; then
    fail "ASTER_INSTALL_DIR must not be the repository root."
fi

json_string() {
    key=$1
    file=$2
    sed -n 's/^[[:space:]]*"'"$key"'"[[:space:]]*:[[:space:]]*"\([^"]*\)"[[:space:]]*,\{0,1\}[[:space:]]*$/\1/p' "$file"
}

json_number() {
    key=$1
    file=$2
    sed -n 's/^[[:space:]]*"'"$key"'"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\)[[:space:]]*,\{0,1\}[[:space:]]*$/\1/p' "$file"
}

validate_managed_entries() {
    root=$1
    for path in "$root"/* "$root"/.[!.]* "$root"/..?*; do
        [ -e "$path" ] || [ -L "$path" ] || continue
        name=${path##*/}
        case "$name" in
            bin|stdlib|LICENSE|install-manifest.json|install-state.json) ;;
            *) fail "The managed installation contains an unexpected entry: $name" ;;
        esac
        [ ! -L "$path" ] ||
            fail "The managed installation contains a symlink: $name"
        if [ -d "$path" ] && [ -n "$(find "$path" -type l -print -quit)" ]; then
            fail "The managed installation contains a nested symlink: $name"
        fi
    done
}

DIRECTORY_STATE=missing
INSTALLED_VERSION=
if [ -d "$INSTALL_DIR" ]; then
    if [ -z "$(find "$INSTALL_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
        DIRECTORY_STATE=empty
    else
        STATE_FILE=$INSTALL_DIR/install-state.json
        [ -f "$STATE_FILE" ] ||
            fail "The installation directory is not empty and is not managed by the ASTER installer."
        [ "$(json_number schema "$STATE_FILE")" = 1 ] ||
            fail "install-state.json is invalid for this ASTER installer."
        [ "$(json_string product "$STATE_FILE")" = "$PRODUCT" ] ||
            fail "install-state.json is invalid for this ASTER installer."
        [ "$(json_string target "$STATE_FILE")" = "$TARGET" ] ||
            fail "install-state.json is invalid for this ASTER installer."
        INSTALLED_VERSION=$(json_string version "$STATE_FILE")
        [ -n "$INSTALLED_VERSION" ] ||
            fail "install-state.json is invalid for this ASTER installer."
        DIRECTORY_STATE=managed
        validate_managed_entries "$INSTALL_DIR"
    fi
fi

DOWNLOAD_DIR=$(mktemp -d "${TMPDIR:-/tmp}/aster-install-download.XXXXXX")
EXTRACT_DIR=$(mktemp -d "${TMPDIR:-/tmp}/aster-install-extract.XXXXXX")
STAGING_DIR=
BACKUP_DIR=
PUBLISHED_NEW=0
REMOVED_EMPTY=0

cleanup() {
    status=$?
    trap - EXIT HUP INT TERM
    if [ "$status" -ne 0 ] && [ "$PUBLISHED_NEW" = 1 ] && [ -d "$INSTALL_DIR" ]; then
        rm -rf -- "$INSTALL_DIR"
    fi
    if [ "$status" -ne 0 ] && [ "$REMOVED_EMPTY" = 1 ] && [ ! -e "$INSTALL_DIR" ]; then
        mkdir -p -- "$INSTALL_DIR"
    fi
    if [ -n "$STAGING_DIR" ] && [ -d "$STAGING_DIR" ]; then
        rm -rf -- "$STAGING_DIR"
    fi
    if [ -n "$BACKUP_DIR" ] && [ "$status" -eq 0 ] && [ -d "$BACKUP_DIR" ]; then
        rm -rf -- "$BACKUP_DIR"
    fi
    rm -rf -- "$DOWNLOAD_DIR" "$EXTRACT_DIR"
    exit "$status"
}
trap cleanup EXIT HUP INT TERM

download_limited() {
    url=$1
    destination=$2
    maximum=$3
    effective_url=$(
        curl -fL --max-redirs 5 --max-filesize "$maximum" \
            --output "$destination" --write-out '%{url_effective}' "$url"
    ) || {
        rm -f -- "$destination"
        fail "Download failed: ${url##*/}"
    }
    assert_safe_url "$effective_url"
    size=$(wc -c < "$destination" | tr -d ' ')
    [ "$size" -gt 0 ] || fail "Downloaded file is empty: ${url##*/}"
    [ "$size" -le "$maximum" ] || fail "Download exceeds the allowed size: ${url##*/}"
}

BASE_URL=${BASE_URL%/}
ARCHIVE_PATH=$DOWNLOAD_DIR/$ARCHIVE_NAME
CHECKSUM_PATH=$DOWNLOAD_DIR/$CHECKSUM_NAME
download_limited "$BASE_URL/$ARCHIVE_NAME" "$ARCHIVE_PATH" "$MAX_ARCHIVE_BYTES"
download_limited "$BASE_URL/$CHECKSUM_NAME" "$CHECKSUM_PATH" "$MAX_CHECKSUM_BYTES"

CHECKSUM_LINE=$(sed -n '1p' "$CHECKSUM_PATH")
EXPECTED_HASH=$(printf '%s\n' "$CHECKSUM_LINE" | sed -n 's/^\([0-9A-Fa-f][0-9A-Fa-f]*\)[[:space:]][[:space:]]*.*$/\1/p')
case "$EXPECTED_HASH" in
    *[!0-9A-Fa-f]*|"") fail "The checksum file has an invalid format." ;;
esac
[ "${#EXPECTED_HASH}" -eq 64 ] || fail "The checksum file has an invalid format."
[ -z "$(sed -n '2p' "$CHECKSUM_PATH")" ] || fail "The checksum file has an invalid format."

if [ "$HASH_TOOL" = sha256sum ]; then
    ACTUAL_HASH=$(sha256sum "$ARCHIVE_PATH" | sed 's/[[:space:]].*$//')
else
    ACTUAL_HASH=$(shasum -a 256 "$ARCHIVE_PATH" | sed 's/[[:space:]].*$//')
fi
[ "$(printf '%s' "$ACTUAL_HASH" | tr 'A-F' 'a-f')" = "$(printf '%s' "$EXPECTED_HASH" | tr 'A-F' 'a-f')" ] ||
    fail "SHA-256 verification failed for the ASTER archive."

LIST_FILE=$DOWNLOAD_DIR/archive-list.txt
TYPE_FILE=$DOWNLOAD_DIR/archive-types.txt
tar -tzf "$ARCHIVE_PATH" > "$LIST_FILE" || fail "The archive could not be listed."
tar -tvzf "$ARCHIVE_PATH" > "$TYPE_FILE" || fail "The archive types could not be inspected."
[ -s "$LIST_FILE" ] || fail "The archive is empty."

while IFS= read -r line; do
    type=$(printf '%s' "$line" | cut -c 1)
    case "$type" in
        -|d) ;;
        *) fail "The archive contains a symlink, hardlink, device, FIFO, or unexpected type." ;;
    esac
done < "$TYPE_FILE"

SEEN_FILE=$DOWNLOAD_DIR/archive-seen.txt
: > "$SEEN_FILE"
ROOT=
while IFS= read -r entry; do
    [ -n "$entry" ] || fail "The archive contains an empty path."
    case "$entry" in
        /*|[A-Za-z]:/*) fail "The archive contains an absolute path." ;;
        *\\*) fail "The archive contains a backslash path." ;;
        */../*|../*|*/..|..) fail "The archive contains path traversal." ;;
    esac
    grep -Fqx -- "$entry" "$SEEN_FILE" && fail "The archive contains a duplicate entry."
    printf '%s\n' "$entry" >> "$SEEN_FILE"
    item_root=${entry%%/*}
    [ -n "$ROOT" ] || ROOT=$item_root
    [ "$item_root" = "$ROOT" ] || fail "The archive must contain exactly one root directory."
done < "$LIST_FILE"

case "$ROOT" in
    aster-*-linux-x64) ;;
    *) fail "The archive root does not match the ASTER Linux release format." ;;
esac
ARCHIVE_VERSION=${ROOT#aster-}
ARCHIVE_VERSION=${ARCHIVE_VERSION%-linux-x64}
[ -n "$ARCHIVE_VERSION" ] || fail "The archive version is empty."
printf '%s' "$ARCHIVE_VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$' ||
    fail "The archive version is invalid."

while IFS= read -r entry; do
    relative=${entry#"$ROOT"}
    relative=${relative#/}
    case "$relative" in
        ""|LICENSE|install-manifest.json|bin/|bin/aster|stdlib/|stdlib/aster/|stdlib/aster/*) ;;
        *) fail "The archive contains an unexpected entry: $entry" ;;
    esac
done < "$LIST_FILE"

for required in \
    "$ROOT/" \
    "$ROOT/bin/" \
    "$ROOT/bin/aster" \
    "$ROOT/stdlib/" \
    "$ROOT/stdlib/aster/" \
    "$ROOT/LICENSE" \
    "$ROOT/install-manifest.json"
do
    grep -Fqx -- "$required" "$LIST_FILE" || fail "The archive is missing required entry: $required"
done

tar -xzf "$ARCHIVE_PATH" -C "$EXTRACT_DIR" || fail "The archive could not be extracted."
EXTRACTED_ROOT=$EXTRACT_DIR/$ROOT

validate_install_root() {
    root=$1
    expected_version=$2
    manifest=$root/install-manifest.json
    [ -f "$manifest" ] || fail "The installation is missing install-manifest.json."
    [ "$(json_number schema "$manifest")" = 1 ] ||
        fail "install-manifest.json has an invalid schema."
    [ "$(json_string product "$manifest")" = "$PRODUCT" ] ||
        fail "install-manifest.json has an invalid product."
    [ "$(json_string target "$manifest")" = "$TARGET" ] ||
        fail "install-manifest.json has an incompatible target."
    version=$(json_string version "$manifest")
    [ -n "$version" ] && [ "$version" = "$expected_version" ] ||
        fail "install-manifest.json has an invalid version."
    [ "$(json_string entrypoint "$manifest")" = "bin/aster" ] ||
        fail "install-manifest.json has an invalid entrypoint."
    [ "$(json_string stdlib "$manifest")" = "stdlib" ] ||
        fail "install-manifest.json has an invalid stdlib path."
    [ "$(json_string license "$manifest")" = "LICENSE" ] ||
        fail "install-manifest.json has an invalid license path."
    [ -x "$root/bin/aster" ] || fail "The installation is missing executable bin/aster."
    [ -f "$root/LICENSE" ] || fail "The installation is missing LICENSE."
    for module in \
        aster/math.aster \
        aster/text/text.aster \
        aster/core/core.aster \
        aster/io/io.aster \
        aster/collections/collections.aster
    do
        [ -f "$root/stdlib/$module" ] || fail "The installation has an incomplete standard library."
    done
}

validate_install_root "$EXTRACTED_ROOT" "$ARCHIVE_VERSION"

validate_cli() {
    install=$1
    version=$2
    project=$(mktemp -d "${TMPDIR:-/tmp}/aster-install-project.XXXXXX")
    printf '%s\n' \
        'using aster.math; public class Program { public static int Main() { return Math.Max(40, 2); } }' \
        > "$project/main.aster"
    old_stdlib_set=0
    if [ "${ASTER_STDLIB+x}" = x ]; then
        old_stdlib_set=1
        old_stdlib=$ASTER_STDLIB
    fi
    unset ASTER_STDLIB
    (
        cd "$project"
        "$install/bin/aster" --version | grep -F "$version" >/dev/null
        "$install/bin/aster" check "$project/main.aster" >/dev/null
        "$install/bin/aster" dump-hir "$project/main.aster" >/dev/null
        "$install/bin/aster" dump-mir "$project/main.aster" >/dev/null
        [ "$("$install/bin/aster" run "$project/main.aster")" = 40 ]
    ) || {
        rm -rf -- "$project"
        fail "Installed ASTER failed functional validation."
    }
    if [ "$old_stdlib_set" = 1 ]; then ASTER_STDLIB=$old_stdlib; export ASTER_STDLIB; fi
    rm -rf -- "$project"
}

escape_profile_path() {
    printf '%s' "$1" | sed 's/[\\"$`]/\\&/g'
}

add_path_block() {
    bin=$1
    [ "${ASTER_INSTALL_SKIP_PATH:-0}" = 1 ] && return
    profile=$HOME/.profile
    begin='# >>> ASTER installer >>>'
    end='# <<< ASTER installer <<<'
    if [ -f "$profile" ]; then
        begin_count=$(grep -Fxc "$begin" "$profile" || true)
        end_count=$(grep -Fxc "$end" "$profile" || true)
        if [ "$begin_count" -ne 0 ] || [ "$end_count" -ne 0 ]; then
            [ "$begin_count" -eq 1 ] && [ "$end_count" -eq 1 ] ||
                fail "The ASTER PATH block in .profile is incomplete or duplicated."
            return
        fi
    fi
    if [ -f "$profile" ]; then
        cp -- "$profile" "$profile.aster-backup"
    fi
    escaped=$(escape_profile_path "$bin")
    {
        [ ! -s "$profile" ] || printf '\n'
        printf '%s\n' "$begin"
        printf 'export PATH="%s:$PATH"\n' "$escaped"
        printf '%s\n' "$end"
    } >> "$profile"
}

write_install_state() {
    directory=$1
    version=$2
    cat > "$directory/install-state.json" <<EOF
{
  "schema": 1,
  "product": "aster",
  "version": "$version",
  "target": "linux-x64"
}
EOF
}

rollback_managed() {
    original_error=$1
    PUBLISHED_NEW=0
    if [ -d "$INSTALL_DIR" ]; then
        rm -rf -- "$INSTALL_DIR" ||
            fail "ASTER update failed: $original_error Rollback also failed. Installation: $INSTALL_DIR Backup: $BACKUP_DIR"
    fi
    if ! mv -- "$BACKUP_DIR" "$INSTALL_DIR"; then
        fail "ASTER update failed: $original_error Rollback also failed. Installation: $INSTALL_DIR Backup: $BACKUP_DIR"
    fi
    BACKUP_DIR=
    restored_version=$(json_string version "$INSTALL_DIR/install-state.json")
    [ "$restored_version" = "$INSTALLED_VERSION" ] ||
        fail "ASTER update failed: $original_error Rollback restored an incompatible marker. Installation: $INSTALL_DIR"
    if [ "$PREVIOUS_HEALTHY" = 1 ]; then
        if ! (validate_install_root "$INSTALL_DIR" "$INSTALLED_VERSION"; validate_cli "$INSTALL_DIR" "$INSTALLED_VERSION"); then
            fail "ASTER update failed: $original_error Rollback restored files that failed validation. Installation: $INSTALL_DIR"
        fi
    fi
    printf '%s\n' "error: ASTER update failed: $original_error" >&2
    printf '%s\n' "The previous installation was restored. Location: $INSTALL_DIR" >&2
    exit 1
}

PREVIOUS_HEALTHY=0
if [ "$DIRECTORY_STATE" = managed ]; then
    if (validate_install_root "$INSTALL_DIR" "$INSTALLED_VERSION"; validate_cli "$INSTALL_DIR" "$INSTALLED_VERSION") >/dev/null 2>&1; then
        PREVIOUS_HEALTHY=1
    fi
    if [ "$INSTALLED_VERSION" = "$ARCHIVE_VERSION" ] && [ "$PREVIOUS_HEALTHY" = 1 ]; then
        add_path_block "$INSTALL_DIR/bin"
        printf '\nASTER is already installed and healthy\n\n'
        printf 'Version: %s\nLocation: %s\n' "$INSTALLED_VERSION" "$INSTALL_DIR"
        exit 0
    fi
fi

PARENT=$(dirname "$INSTALL_DIR")
mkdir -p -- "$PARENT"
STAGING_DIR=$(mktemp -d "$INSTALL_DIR.staging.XXXXXX")
cp -R -- "$EXTRACTED_ROOT"/. "$STAGING_DIR"/
chmod 755 "$STAGING_DIR/bin/aster"
validate_install_root "$STAGING_DIR" "$ARCHIVE_VERSION"
validate_cli "$STAGING_DIR" "$ARCHIVE_VERSION"

if [ "$DIRECTORY_STATE" = managed ]; then
    BACKUP_DIR=$(mktemp -d "$INSTALL_DIR.backup.XXXXXX")
    rmdir -- "$BACKUP_DIR"
    mv -- "$INSTALL_DIR" "$BACKUP_DIR"
    if ! mv -- "$STAGING_DIR" "$INSTALL_DIR"; then
        rollback_managed "The staged installation could not be published."
    fi
    STAGING_DIR=
    PUBLISHED_NEW=1
    if ! write_install_state "$INSTALL_DIR" "$ARCHIVE_VERSION"; then
        rollback_managed "The new install-state.json could not be written."
    fi
    if ! (validate_install_root "$INSTALL_DIR" "$ARCHIVE_VERSION"; validate_cli "$INSTALL_DIR" "$ARCHIVE_VERSION"); then
        rollback_managed "The published installation failed functional validation."
    fi
    PUBLISHED_NEW=0
    if ! rm -rf -- "$BACKUP_DIR"; then
        fail "ASTER was updated, but its backup could not be removed safely: $BACKUP_DIR"
    fi
    BACKUP_DIR=
    add_path_block "$INSTALL_DIR/bin"
    if [ "$INSTALLED_VERSION" = "$ARCHIVE_VERSION" ]; then
        printf '\nASTER repaired successfully\n\n'
        printf 'Version: %s\nLocation: %s\n' "$ARCHIVE_VERSION" "$INSTALL_DIR"
    else
        printf '\nASTER updated successfully\n\n'
        printf 'Previous version: %s\nCurrent version: %s\nLocation: %s\n' \
            "$INSTALLED_VERSION" "$ARCHIVE_VERSION" "$INSTALL_DIR"
    fi
    exit 0
elif [ "$DIRECTORY_STATE" = empty ]; then
    rmdir -- "$INSTALL_DIR"
    REMOVED_EMPTY=1
fi
mv -- "$STAGING_DIR" "$INSTALL_DIR"
STAGING_DIR=
PUBLISHED_NEW=1
write_install_state "$INSTALL_DIR" "$ARCHIVE_VERSION"
validate_cli "$INSTALL_DIR" "$ARCHIVE_VERSION"
add_path_block "$INSTALL_DIR/bin"

printf '\nASTER installed successfully\n\n'
printf 'Version: %s\nTarget: %s\nLocation: %s\n\n' "$ARCHIVE_VERSION" "$TARGET" "$INSTALL_DIR"
printf 'Restart your shell or reload your profile, then run:\n'
printf '  aster --version\n'
