#!/usr/bin/env bash
#
# Install File Converter into this user's account:
#   ~/.local/bin/fileconverter                    (Rust binary)
#   ~/.local/share/applications/fileconverter.desktop
#   ~/.local/share/icons/hicolor/*/apps/fileconverter.png
#   ~/.config/file-converter/presets.json
#   Dolphin service menu
#
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
APP_DESKTOP="$SCRIPT_DIR/fileconverter-app.desktop"
SERVICE_DESKTOP="$SCRIPT_DIR/fileconverter.desktop"
ICON_DIR="$SCRIPT_DIR/assets"

echo "== File Converter install =="

# ------------------------------------------------------------- Rust toolchain
export PATH="$HOME/.cargo/bin:$PATH"
if ! command -v cargo >/dev/null 2>&1; then
    echo "ERROR: cargo not found. Install Rust with:" >&2
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
    echo "  source \"\$HOME/.cargo/env\"" >&2
    exit 1
fi

# ------------------------------------------------------------------- build
echo "Building release binary (this can take a minute)…"
cargo build --release --manifest-path "$SCRIPT_DIR/Cargo.toml"

# ------------------------------------------------------------------ launcher
mkdir -p "$HOME/.local/bin"
# Remove first: an existing ~/.local/bin/fileconverter may be a symlink to the
# repository launcher, and `install` would follow it and overwrite the source.
rm -f "$HOME/.local/bin/fileconverter"
install -Dm755 "$SCRIPT_DIR/target/release/fileconverter" "$HOME/.local/bin/fileconverter"
BIN_PATH="$HOME/.local/bin/fileconverter"
echo "OK  $BIN_PATH"

case ":$PATH:" in
    *":$HOME/.local/bin:"*) : ;;
    *) echo "NOTE: $HOME/.local/bin is not on your PATH." >&2
       echo "      Add it in ~/.bashrc / ~/.zshrc / /etc/profile if needed." >&2 ;;
esac

# -------------------------------------------------------------------- icons
for size in 32 48 64 128 256 512; do
    icon_src="$ICON_DIR/fileconverter-${size}.png"
    icon_dir="$HOME/.local/share/icons/hicolor/${size}x${size}/apps"
    if [ -f "$icon_src" ]; then
        mkdir -p "$icon_dir"
        install -Dm644 "$icon_src" "$icon_dir/fileconverter.png"
    fi
done
echo "OK  icons:   ~/.local/share/icons/hicolor/*/apps/fileconverter.png"

# -------------------------------------------------------- application entry
mkdir -p "$HOME/.local/share/applications"
sed "s|@LOCALBIN@|$BIN_PATH|" "$APP_DESKTOP" \
    > "$HOME/.local/share/applications/fileconverter.desktop"
echo "OK  app menu entry: ~/.local/share/applications/fileconverter.desktop"

# --------------------------------------------------------------- config dir
mkdir -p "$HOME/.config/file-converter"
if [ ! -f "$HOME/.config/file-converter/presets.json" ]; then
    "$BIN_PATH" --write-default-presets >/dev/null 2>&1 || true
fi
echo "OK  presets:  ~/.config/file-converter/presets.json"

# ------------------------------------------------------- Dolphin service menu
SERVICE_DIR="$HOME/.local/share/kio/servicemenus"
_guess_service_dir() {
    # Plasma 6 reads ~/.local/share/kio/servicemenus
    # Plasma 5 (21.08+) read ~/.local/share/kio/servicemenu-download
    # Older Plasma 5 read      ~/.local/share/kservices5/ServiceMenus
    if [ -d "$HOME/.local/share/kio/servicemenus" ] || \
       [ -d "/usr/share/kio/servicemenus" ]; then
        return
    fi
    if [ -d "$HOME/.local/share/kio/servicemenu-download" ] || \
       command -v kf5-config >/dev/null 2>&1; then
        eval "$1=\"$HOME/.local/share/kio/servicemenu-download\""
        return
    fi
    if [ -d "$HOME/.local/share/kservices5" ] || \
       [ -d "/usr/share/kservices5/ServiceMenus" ]; then
        eval "$1=\"$HOME/.local/share/kservices5/ServiceMenus\""
        return
    fi
    eval "$1=\"$HOME/.local/share/kio/servicemenus\""
}
_guess_service_dir SERVICE_DIR
mkdir -p "$SERVICE_DIR"
sed "s|@LOCALBIN@|$BIN_PATH|" "$SERVICE_DESKTOP" \
    > "$SERVICE_DIR/fileconverter.desktop"
chmod +x "$SERVICE_DIR/fileconverter.desktop"
echo "OK  Dolphin service menu: $SERVICE_DIR/fileconverter.desktop"

# --------------------------------------------------------- refresh caches
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" >/dev/null 2>&1 || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
fi
if command -v kbuildsycoca6 >/dev/null 2>&1; then
    kbuildsycoca6 >/dev/null 2>&1 || true
    echo "OK  refreshed KDE menu cache (kbuildsycoca6)"
elif command -v kbuildsycoca5 >/dev/null 2>&1; then
    kbuildsycoca5 >/dev/null 2>&1 || true
    echo "OK  refreshed KDE menu cache (kbuildsycoca5)"
else
    echo "NOTE: kbuildsycoca not found; restart Dolphin to load the menu."
fi

# ---------------------------------------------------------------- tool check
echo ""
echo "Checking conversion tools:"
MISSING=()
for tool in ffmpeg ffprobe magick soffice; do
    if command -v "$tool" >/dev/null 2>&1; then
        printf "  OK   %-9s %s\n" "$tool" "$(command -v "$tool")"
    else
        printf "  MISS %-9s (not found)\n" "$tool"
        MISSING+=("$tool")
    fi
done

if [ "${#MISSING[@]}" -gt 0 ]; then
    echo ""
    echo "WARNING: missing conversion tools: ${MISSING[*]}"
    echo "  Install everything with:"
    echo "  sudo pacman -S --needed ffmpeg imagemagick libreoffice"
fi
if ! command -v img2pdf >/dev/null 2>&1; then
    echo ""
    echo "Optional:  sudo pacman -S img2pdf"
    echo "            (assembly of image -> PDF; ImageMagick is used instead when absent)"
fi
if ! command -v mediainfo >/dev/null 2>&1; then
    echo ""
    echo "Optional:  sudo pacman -S mediainfo"
    echo "            (adds the detailed 'mediainfo' text in the GUI Media Info panel)"
fi
if ! command -v gs >/dev/null 2>&1; then
    echo ""
    echo "Optional:  sudo pacman -S ghostscript"
    echo "            (needed to enforce 'Max File Size' on PDF outputs)"
fi

echo ""
echo "Done. The menu entry and taskbar icon should now be live."
echo "If Dolphin is open, restart it to see 'Convert with File Converter…'."
echo "Usage: right-click files in Dolphin -> 'Convert with File Converter…',"
echo "       or run:  ~/.local/bin/fileconverter clip.mp4 --to webm"
