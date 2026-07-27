#!/bin/sh
set -eu

PRODUCT=aster
TARGET=linux-x64

fail() {
    printf '%s\n' "error: $*" >&2
    exit 1
}

case "$(uname -s)" in
    Linux) ;;
    *) fail "This uninstaller supports Linux x64 only." ;;
esac
case "$(uname -m)" in
    x86_64|amd64) ;;
    *) fail "Unsupported Linux architecture. Expected x86_64." ;;
esac

for tool in sed grep cp mv chmod find dirname rm rmdir mktemp; do
    command -v "$tool" >/dev/null 2>&1 || fail "Required tool not found: $tool"
done

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
[ -n "$INSTALL_DIR" ] && [ "$INSTALL_DIR" != / ] && [ "$INSTALL_DIR" != "$HOME" ] ||
    fail "ASTER_INSTALL_DIR is too broad to remove safely."
PROBE=$INSTALL_DIR
while [ "$PROBE" != / ]; do
    [ ! -L "$PROBE" ] || fail "ASTER_INSTALL_DIR must not traverse a symlink."
    PROBE=$(dirname "$PROBE")
done
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

validate_entries() {
    root=$1
    for path in "$root"/* "$root"/.[!.]* "$root"/..?*; do
        [ -e "$path" ] || [ -L "$path" ] || continue
        name=${path##*/}
        case "$name" in
            bin|stdlib|LICENSE|install-manifest.json|install-state.json) ;;
            *) fail "The managed installation contains an unexpected entry: $name" ;;
        esac
        [ ! -L "$path" ] || fail "The managed installation contains a symlink: $name"
        if [ -d "$path" ] && [ -n "$(find "$path" -type l -print -quit)" ]; then
            fail "The managed installation contains a nested symlink: $name"
        fi
    done
}

remove_path_block() {
    [ "${ASTER_INSTALL_SKIP_PATH:-0}" = 1 ] && return
    profile=$HOME/.profile
    [ -f "$profile" ] || return
    begin='# >>> ASTER installer >>>'
    end='# <<< ASTER installer <<<'
    begin_count=$(grep -Fxc "$begin" "$profile" || true)
    end_count=$(grep -Fxc "$end" "$profile" || true)
    if [ "$begin_count" -eq 0 ] && [ "$end_count" -eq 0 ]; then return; fi
    [ "$begin_count" -eq 1 ] && [ "$end_count" -eq 1 ] ||
        fail "The ASTER PATH block in .profile is incomplete or duplicated."
    begin_line=$(grep -Fn "$begin" "$profile" | sed 's/:.*//')
    end_line=$(grep -Fn "$end" "$profile" | sed 's/:.*//')
    [ "$begin_line" -lt "$end_line" ] ||
        fail "The ASTER PATH block in .profile is malformed."
    cp -p -- "$profile" "$profile.aster-backup"
    temporary=$(mktemp "$HOME/.profile.aster.XXXXXX")
    sed '/^# >>> ASTER installer >>>$/,/^# <<< ASTER installer <<<$/d' "$profile" > "$temporary"
    chmod --reference="$profile" "$temporary"
    mv -- "$temporary" "$profile"
}

if [ ! -e "$INSTALL_DIR" ]; then
    printf '%s\n' "ASTER is not installed"
    exit 0
fi
[ -d "$INSTALL_DIR" ] || fail "ASTER_INSTALL_DIR exists and is not a directory."
if [ -z "$(find "$INSTALL_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
    rmdir -- "$INSTALL_DIR"
    printf '%s\n' "ASTER is not installed"
    exit 0
fi

STATE_FILE=$INSTALL_DIR/install-state.json
[ -f "$STATE_FILE" ] || fail "The installation directory is not managed by the ASTER installer."
[ "$(json_number schema "$STATE_FILE")" = 1 ] ||
    fail "install-state.json is invalid for this ASTER uninstaller."
[ "$(json_string product "$STATE_FILE")" = "$PRODUCT" ] ||
    fail "install-state.json is invalid for this ASTER uninstaller."
[ -n "$(json_string version "$STATE_FILE")" ] ||
    fail "install-state.json is invalid for this ASTER uninstaller."
[ "$(json_string target "$STATE_FILE")" = "$TARGET" ] ||
    fail "install-state.json is invalid or targets another platform."

validate_entries "$INSTALL_DIR"
remove_path_block

for name in bin stdlib LICENSE install-manifest.json install-state.json; do
    path=$INSTALL_DIR/$name
    [ -e "$path" ] || continue
    case "$name" in
        bin|stdlib) rm -rf -- "$path" ;;
        *) rm -f -- "$path" ;;
    esac
done
[ -z "$(find "$INSTALL_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ] ||
    fail "The installation directory was not empty after removing managed entries."
rmdir -- "$INSTALL_DIR"

printf '\nASTER uninstalled successfully\n\n'
printf 'Location: %s\n\n' "$INSTALL_DIR"
printf '%s\n' "Open shells may retain the previous PATH until restarted."
