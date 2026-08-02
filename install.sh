#!/bin/sh
# cairn installer for Linux and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/Marb-AI/cairn/main/install.sh | sh
#
# Downloads the release binary for your OS/arch and symlinks it onto your PATH: into
# ~/.cairn/bin for a user install, /usr/local/lib/cairn/bin when run as root, since a
# root install has to be readable by the people who will actually use it.
# Re-run any time to upgrade. Windows: download the .exe from the releases page — this
# script does not cover it.
#
# Env overrides:
#   CAIRN_VERSION    tag to install (default: latest)
#   CAIRN_HOME       where the binary lives (default: ~/.cairn, or
#                    /usr/local/lib/cairn when run as root)
#   CAIRN_LINK_DIR   PATH dir to symlink into (default: /usr/local/bin)
set -eu

REPO="Marb-AI/cairn"
BIN="cairn"
LINK_DIR="${CAIRN_LINK_DIR:-/usr/local/bin}"
VERSION="${CAIRN_VERSION:-latest}"

# Installing as root is a *system* install, and `$HOME` is then `/root`, which is mode 700
# on every distribution worth naming. Putting the binary there and linking it into
# /usr/local/bin produces a link that looks perfect and that nobody but root can follow —
# every other user gets "Permission denied" from a command that appears to be installed.
# So root installs somewhere readable instead.
if [ -z "${CAIRN_HOME:-}" ] && [ "$(id -u)" = 0 ]; then
	CAIRN_HOME=/usr/local/lib/cairn
fi
CAIRN_HOME="${CAIRN_HOME:-$HOME/.cairn}"
INSTALL_DIR="$CAIRN_HOME/bin"

# --- detect platform -------------------------------------------------------
case "$(uname -s)" in
	Linux)  OS=linux ;;
	Darwin) OS=darwin ;;
	*) echo "cairn: unsupported OS '$(uname -s)' (install.sh supports Linux and macOS)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
	x86_64|amd64)  ARCH=amd64 ;;
	arm64|aarch64) ARCH=arm64 ;;
	*) echo "cairn: unsupported architecture '$(uname -m)'" >&2; exit 1 ;;
esac
# Only Apple Silicon is published. Saying so beats a 404 the user has to interpret.
if [ "$OS" = darwin ] && [ "$ARCH" = amd64 ]; then
	echo "cairn: no Intel macOS build is published — cairn ships arm64 (Apple Silicon) only" >&2
	exit 1
fi
ASSET="$BIN-$OS-$ARCH"

if [ "$VERSION" = latest ]; then
	URL="https://github.com/$REPO/releases/latest/download/$ASSET"
else
	URL="https://github.com/$REPO/releases/download/$VERSION/$ASSET"
fi

# --- downloader ------------------------------------------------------------
if command -v curl >/dev/null 2>&1; then
	fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
	fetch() { wget -qO "$2" "$1"; }
else
	echo "cairn: need curl or wget" >&2; exit 1
fi

# --- download --------------------------------------------------------------
echo "cairn: installing $VERSION for $OS/$ARCH"
mkdir -p "$INSTALL_DIR"
# Traversable by everyone: a system install is for every user on the machine, and a
# directory only its creator can enter is the same failure as installing into /root.
chmod 755 "$CAIRN_HOME" "$INSTALL_DIR" 2>/dev/null || true
TARGET="$INSTALL_DIR/$BIN"
# Download beside the target rather than over it, so a failed upgrade leaves the working
# binary in place instead of a truncated one.
TMP="$TARGET.download"
if ! fetch "$URL" "$TMP"; then
	rm -f "$TMP"
	echo "cairn: download failed ($URL)" >&2
	echo "       Release assets are only public when the repository is. On a private" >&2
	echo "       repo this URL is a 404 to anyone unauthenticated, and an SSH key does" >&2
	echo "       not help — asset downloads go over HTTPS, not git. Use:" >&2
	# `gh release download` with no tag means the latest one; "latest" is not a tag name.
	[ "$VERSION" = latest ] && tag="" || tag=" $VERSION"
	echo "         gh release download$tag --repo $REPO --pattern '$ASSET'" >&2
	exit 1
fi
chmod +x "$TMP"
mv -f "$TMP" "$TARGET"
echo "cairn: binary -> $TARGET"

# macOS: the published binary is built and signed natively, so this is belt and braces —
# it costs nothing and repairs a signature that did not survive the trip.
if [ "$OS" = darwin ] && command -v codesign >/dev/null 2>&1; then
	codesign --force --sign - "$TARGET" >/dev/null 2>&1 && echo "cairn: re-signed for macOS"
fi

# Fail loudly here rather than let the first real command be the thing that discovers a
# binary that does not run on this machine.
if ! "$TARGET" --version >/dev/null 2>&1; then
	echo "cairn: the downloaded binary does not run on this machine" >&2
	exit 1
fi

# --- symlink onto PATH -----------------------------------------------------
LINK="$LINK_DIR/$BIN"
if [ "$LINK" = "$TARGET" ]; then
	# Link dir is the install dir — the binary is already there, don't self-link.
	LINK=""
	echo "cairn: add $INSTALL_DIR to your PATH:"
	echo "         export PATH=\"$INSTALL_DIR:\$PATH\""
elif mkdir -p "$LINK_DIR" 2>/dev/null && [ -w "$LINK_DIR" ]; then
	ln -sf "$TARGET" "$LINK"
	echo "cairn: linked -> $LINK"
elif command -v sudo >/dev/null 2>&1; then
	echo "cairn: linking into $LINK_DIR (needs sudo)"
	sudo mkdir -p "$LINK_DIR"
	sudo ln -sf "$TARGET" "$LINK"
	echo "cairn: linked -> $LINK"
else
	LINK=""
	echo
	echo "cairn: could not write $LINK_DIR — add the binary to your PATH, e.g.:"
	echo "         export PATH=\"$INSTALL_DIR:\$PATH\""
fi

# --- check it is really usable ----------------------------------------------
# Not paranoia: the failure this catches shipped. A link into an unreadable directory
# passes every check above — the file is there, it is executable by its owner, the symlink
# resolves — and still leaves `cairn` unrunnable for the person who installed it.
if [ -n "$LINK" ] && ! [ -x "$LINK" ]; then
	echo >&2
	echo "cairn: $LINK is not executable by you." >&2
	echo "       The binary is at $TARGET; something between here and there is not" >&2
	echo "       readable. Re-run this installer as the user who will use cairn, or set" >&2
	echo "       CAIRN_HOME to somewhere they can read." >&2
	exit 1
fi

# --- done ------------------------------------------------------------------
echo
# Installation settings sit beside the binary, not in the checkout: one binary serves
# every repository on the machine (see cairn-store::config).
echo "cairn: done. Installation settings go in $INSTALL_DIR/cairn.yaml (optional)."
echo "       A repository's index lives in its own .cairn/ — run 'cairn index' there."
if [ -n "$LINK" ] && command -v "$BIN" >/dev/null 2>&1; then
	echo "cairn: run 'cairn --help' to get started."
else
	echo "cairn: run '$TARGET --help' to get started."
fi
