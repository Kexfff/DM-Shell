#!/bin/sh
# Installs dmsh: its dependencies, the binary, a systemd user service (starts it with
# the session, restarts it if it crashes) and a niri config snippet. Nothing else — the
# rest of a desktop (polkit agent, lock screen, wallpaper...) is up to you.
#
#   ./install.sh               install (asks before changing anything outside the repo)
#   ./install.sh --yes         don't ask
#   ./install.sh --no-deps     skip installing packages
#   ./install.sh --no-systemd  start dmsh from niri's spawn-at-startup instead
#   ./install.sh --prefix DIR  install the binary to DIR/bin (default ~/.local)
#   ./install.sh --uninstall   remove the binary, the service and the niri snippet
set -eu

prefix="$HOME/.local"
deps=1
systemd=1
yes=0
uninstall=0
while [ $# -gt 0 ]; do
    case "$1" in
        --yes | -y) yes=1 ;;
        --no-deps) deps=0 ;;
        --no-systemd) systemd=0 ;;
        --prefix) prefix="$2"; shift ;;
        --uninstall) uninstall=1 ;;
        -h | --help) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

repo=$(cd "$(dirname "$0")" && pwd)
bin="$prefix/bin/dmsh"
niri_dir="${XDG_CONFIG_HOME:-$HOME/.config}/niri"
niri_config="$niri_dir/config.kdl"
snippet="$niri_dir/dmsh.kdl"
include_line='include "dmsh.kdl"'
sudo=${SUDO:-sudo}

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
note() { printf '    %s\n' "$*"; }
ask() {
    [ "$yes" = 1 ] && return 0
    printf '\033[1m==>\033[0m %s [y/N] ' "$1"
    read -r answer || return 1
    [ "$answer" = y ] || [ "$answer" = Y ]
}

if [ "$uninstall" = 1 ]; then
    if [ -x "$bin" ]; then
        "$bin" service --remove >/dev/null 2>&1 || true
    fi
    rm -f "$bin" "$snippet"
    if [ -f "$niri_config" ]; then
        sed -i "/^$include_line\$/d" "$niri_config"
    fi
    say "Removed $bin, its service, $snippet and the include line. Kill dmsh to stop it now."
    exit 0
fi

# --- Dependencies -------------------------------------------------------------------
# Required: fontconfig (fonts), libxkbcommon, and Rust to build.
# Optional, per feature: pactl + wpctl (volume OSD), UPower (battery),
# NetworkManager (Wi-Fi messages and toggle). Missing ones just turn that feature off.
install_packages() {
    if command -v pacman >/dev/null; then
        $sudo pacman -S --needed --noconfirm "$@"
    elif command -v apt-get >/dev/null; then
        $sudo apt-get install -y "$@"
    elif command -v dnf >/dev/null; then
        $sudo dnf install -y "$@"
    else
        say "Unknown package manager; please install: $*"
        return 1
    fi
}

pkg() {
    # pkg <pacman> <apt> <dnf>
    if command -v pacman >/dev/null; then echo "$1"
    elif command -v apt-get >/dev/null; then echo "$2"
    elif command -v dnf >/dev/null; then echo "$3"
    fi
}

if [ "$deps" = 1 ]; then
    required="$(pkg fontconfig fontconfig fontconfig) $(pkg libxkbcommon libxkbcommon0 libxkbcommon)"
    if ! command -v cargo >/dev/null; then
        required="$required $(pkg rust cargo cargo)"
    fi
    optional=""
    command -v pactl >/dev/null || optional="$optional $(pkg pipewire-pulse pipewire-pulse pipewire-pulseaudio)"
    command -v wpctl >/dev/null || optional="$optional $(pkg wireplumber wireplumber wireplumber)"
    command -v upower >/dev/null || optional="$optional $(pkg upower upower upower)"
    command -v nmcli >/dev/null || optional="$optional $(pkg networkmanager network-manager NetworkManager)"

    say "Installing required packages: $required"
    install_packages $required
    if [ -n "$optional" ]; then
        say "Optional packages for volume, battery and Wi-Fi features:$optional"
        if ask "Install them too?"; then
            install_packages $optional || say "Skipped; those features stay off."
        fi
    fi
fi

# --- Binary --------------------------------------------------------------------------
say "Building dmsh (release)..."
if ! (cd "$repo" && cargo build --release --locked -p dmsh); then
    say "Build failed. dmsh needs a recent stable Rust; rustup (https://rustup.rs) has it."
    exit 1
fi
install -Dm755 "$repo/target/release/dmsh" "$bin"
say "Installed $bin"
case ":$PATH:" in
    *":$prefix/bin:"*) ;;
    *) note "$prefix/bin isn't on your PATH; add it to use 'dmsh' in a terminal." ;;
esac

# --- Autostart ------------------------------------------------------------------------
# A systemd user service restarts dmsh if it crashes. It starts with graphical-session.target,
# which niri brings up when started as a session (niri-session, or a display manager's
# niri entry); a niri started plainly (`niri` on a tty) doesn't, so use spawn-at-startup.
service=0
if [ "$systemd" = 1 ] && command -v systemctl >/dev/null \
    && systemctl --user show-environment >/dev/null 2>&1; then
    service=1
    if [ -n "${NIRI_SOCKET:-}" ] && ! systemctl --user is-active -q graphical-session.target; then
        note "This niri wasn't started as a session (no graphical-session.target);"
        note "starting dmsh from niri's spawn-at-startup instead of a systemd service."
        service=0
    fi
fi
if [ "$service" = 1 ]; then
    "$bin" service --apply >/dev/null
    say "Enabled the dmsh systemd user service (starts with the session, restarts on a crash)"
fi

# --- niri snippet --------------------------------------------------------------------
mkdir -p "$niri_dir"
if [ "$service" = 1 ]; then
    "$bin" niri --no-autostart > "$snippet.new"
else
    "$bin" niri > "$snippet.new"
fi
# Don't take keys you already use: comment out dmsh's bind for them instead.
for key in $(sed -n 's/^ *\([A-Za-z0-9+]*\) hotkey-overlay-title=.*/\1/p' "$snippet.new"); do
    key_re=$(printf '%s' "$key" | sed 's/+/\\+/g')
    if grep -rqsE "^[[:space:]]*$key_re([[:space:]]|\{)" --include='*.kdl' --exclude=dmsh.kdl "$niri_dir"; then
        note "$key is already bound in your niri config; dmsh's $key bind is commented out in $snippet."
        sed -i "s|^\( *\)$key |\1// $key |" "$snippet.new"
    fi
done
mv "$snippet.new" "$snippet"
say "Wrote $snippet (autostart and keybinds)"

if [ ! -f "$niri_config" ]; then
    note "No $niri_config yet (niri creates it on first start)."
    note "Add this line to it then: $include_line"
elif grep -qxF "$include_line" "$niri_config"; then
    note "$niri_config already includes it."
elif ask "Add '$include_line' to $niri_config?"; then
    # Only the line itself, so --uninstall restores the file exactly.
    [ -z "$(tail -c1 "$niri_config")" ] || printf '\n' >> "$niri_config"
    printf '%s\n' "$include_line" >> "$niri_config"
    if command -v niri >/dev/null && ! niri validate -c "$niri_config" >/dev/null 2>&1; then
        sed -i "/^$include_line\$/d" "$niri_config"
        say "niri rejected the config with the include; removed it again. See: niri validate"
        exit 1
    fi
    say "Added the include to $niri_config"
else
    note "Not changed. To use it, add this line to $niri_config: $include_line"
fi

if [ -n "${NIRI_SOCKET:-}" ] && ! pgrep -x dmsh >/dev/null && ask "Start dmsh now?"; then
    if [ "$service" = 1 ]; then
        systemctl --user start dmsh.service
    else
        niri msg action spawn -- "$bin"
    fi
fi
say "Done. dmsh starts with niri from now on; see 'dmsh --help' and README.md."
