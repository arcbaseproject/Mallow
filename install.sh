#!/bin/sh
# Interactive installer for Mallow on Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/arcbaseproject/Mallow/main/install.sh | sh
#
# It picks the latest release (or MALLOW_TAG, e.g. MALLOW_TAG=v0.1.0), offers
# the .deb, .rpm or tarball that suits this system, checks the download against
# the release's SHA256SUMS, and asks before anything runs with sudo.
set -eu

REPO="arcbaseproject/Mallow"

say() { printf '%s\n' "$*"; }
die() { printf 'Error: %s\n' "$*" >&2; exit 1; }

# Questions read the terminal, so this works when the script itself arrives on stdin.
ask() {
    printf '%s ' "$1" >/dev/tty
    IFS= read -r answer </dev/tty || answer=""
    printf '%s' "$answer"
}
confirm() {
    case "$(ask "$1 [Y/n]")" in
        "" | y | Y | yes | Yes) return 0 ;;
        *) return 1 ;;
    esac
}

[ -r /dev/tty ] || die "no terminal to ask questions on. Download a package from https://github.com/$REPO/releases instead."
[ "$(uname -s)" = Linux ] || die "Mallow runs on Linux only (it is built on WebKitGTK)."
case "$(uname -m)" in
    x86_64 | amd64) ;;
    *) die "releases are built for x86_64 only. Build from source: https://github.com/$REPO#from-source" ;;
esac
for tool in curl sha256sum tar; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is required."
done

# Release
tag="${MALLOW_TAG:-}"
if [ -z "$tag" ]; then
    tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
        sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1) ||
        die "could not reach GitHub to find the latest release."
    [ -n "$tag" ] || die "no published release found at https://github.com/$REPO/releases"
fi
case "$tag" in
    v[0-9]*) ;;
    *) die "unexpected release tag '$tag'." ;;
esac
version="${tag#v}"
base="https://github.com/$REPO/releases/download/$tag"
deb="mallow_${version}-1_amd64.deb"
rpm="mallow-${version}-1.x86_64.rpm"
tarball="mallow-${version}-linux-x86_64.tar.gz"

# Suggest the package format this distribution uses.
id=""
like=""
if [ -r /etc/os-release ]; then
    id=$(sed -n 's/^ID=//p' /etc/os-release | tr -d '"')
    like=$(sed -n 's/^ID_LIKE=//p' /etc/os-release | tr -d '"')
fi
suggest=3
case " $id $like " in
    *" debian "* | *" ubuntu "*) command -v apt >/dev/null 2>&1 && suggest=1 ;;
    *" fedora "* | *" rhel "* | *" suse "* | *" opensuse "*)
        { command -v dnf >/dev/null 2>&1 || command -v zypper >/dev/null 2>&1; } && suggest=2 ;;
esac

say "Mallow $version installer"
say ""
say "  1) .deb package      Debian, Ubuntu and derivatives (uses apt)"
say "  2) .rpm package      Fedora, openSUSE and derivatives (uses dnf or zypper)"
say "  3) Tarball for you   into ~/.local, no sudo"
say "  4) Tarball for all   into /usr/local, uses sudo"
say "  q) Quit"
say ""
choice=$(ask "Install which? [$suggest]")
[ -n "$choice" ] || choice=$suggest
case "$choice" in
    1) file=$deb ;;
    2) file=$rpm ;;
    3 | 4) file=$tarball ;;
    q | Q) exit 0 ;;
    *) die "no option '$choice'." ;;
esac

# Download and verify
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
say "Downloading $file ..."
curl -fL --progress-bar -o "$tmp/$file" "$base/$file" || die "download failed: $base/$file"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || die "could not download SHA256SUMS."
expected=$(awk -v f="$file" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || die "$file is not listed in SHA256SUMS."
actual=$(sha256sum "$tmp/$file" | awk '{ print $1 }')
[ "$expected" = "$actual" ] || die "checksum mismatch for $file. Nothing was installed."
say "Checksum OK."

sudo_run() {
    if [ "$(id -u)" = 0 ]; then "$@"; else sudo "$@"; fi
}

case "$choice" in
    1)
        confirm "Run 'sudo apt install' for $file?" || exit 0
        sudo_run apt install -y "$tmp/$file"
        ;;
    2)
        if command -v dnf >/dev/null 2>&1; then
            confirm "Run 'sudo dnf install' for $file?" || exit 0
            sudo_run dnf install -y "$tmp/$file"
        elif command -v zypper >/dev/null 2>&1; then
            confirm "Run 'sudo zypper install' for $file?" || exit 0
            sudo_run zypper --non-interactive install --allow-unsigned-rpm "$tmp/$file"
        else
            die "neither dnf nor zypper found."
        fi
        ;;
    3 | 4)
        if [ "$choice" = 3 ]; then prefix="$HOME/.local"; run=""; else prefix=/usr/local; run=sudo_run; fi
        # The tarball uses the system's GTK and WebKitGTK; warn if they are missing.
        if command -v ldconfig >/dev/null 2>&1 &&
            ! ldconfig -p 2>/dev/null | grep -q 'libwebkitgtk-6.0.so'; then
            say "Warning: WebKitGTK 6.0 was not found. Install it (and GTK 4.20 or later) from your distribution, or Mallow will not start."
        fi
        confirm "Install into $prefix?" || exit 0
        tar -xzf "$tmp/$file" -C "$tmp"
        src="$tmp/${tarball%.tar.gz}"
        $run mkdir -p "$prefix/bin" "$prefix/share/applications" "$prefix/share/icons/hicolor/scalable/apps"
        $run install -m755 "$src/bin/mallow" "$prefix/bin/mallow"
        $run install -m644 "$src/share/applications/dev.mallow.Browser.desktop" "$prefix/share/applications/"
        $run install -m644 "$src/share/icons/hicolor/scalable/apps/dev.mallow.Browser.svg" "$prefix/share/icons/hicolor/scalable/apps/"
        command -v update-desktop-database >/dev/null 2>&1 && $run update-desktop-database -q "$prefix/share/applications" || true
        case ":$PATH:" in
            *":$prefix/bin:"*) ;;
            *) say "Note: $prefix/bin is not on your PATH. Start Mallow from your app menu, or add it to PATH." ;;
        esac
        say "To uninstall, delete $prefix/bin/mallow, $prefix/share/applications/dev.mallow.Browser.desktop and $prefix/share/icons/hicolor/scalable/apps/dev.mallow.Browser.svg."
        ;;
esac

say ""
say "Mallow $version is installed. Start it from your app menu or run: mallow"
