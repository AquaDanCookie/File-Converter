#!/usr/bin/env bash
#
# Remove File Converter from this user's account.
#
set -euo pipefail

echo "== File Converter uninstall =="

rm -f "$HOME/.local/bin/fileconverter"
echo "OK  removed ~/.local/bin/fileconverter"

rm -f "$HOME/.local/share/applications/fileconverter.desktop"
echo "OK  removed ~/.local/share/applications/fileconverter.desktop"

for size in 32 48 64 128 256 512; do
    rm -f "$HOME/.local/share/icons/hicolor/${size}x${size}/apps/fileconverter.png"
done
echo "OK  removed icon theme files"

for dir in \
    "$HOME/.local/share/kio/servicemenus" \
    "$HOME/.local/share/kio/servicemenu-download" \
    "$HOME/.local/share/kservices5/ServiceMenus"; do
    if [ -f "$dir/fileconverter.desktop" ]; then
        rm -f "$dir/fileconverter.desktop"
        echo "OK  removed $dir/fileconverter.desktop"
    fi
done

if [ -d "$HOME/.config/file-converter" ]; then
    echo "NOTE: keeping ~/.config/file-converter (presets)."
    echo "      Remove it manually if you want a clean slate:"
    echo "      rm -rf ~/.config/file-converter"
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" >/dev/null 2>&1 || true
fi
if command -v kbuildsycoca6 >/dev/null 2>&1; then
    kbuildsycoca6 >/dev/null 2>&1 || true
elif command -v kbuildsycoca5 >/dev/null 2>&1; then
    kbuildsycoca5 >/dev/null 2>&1 || true
fi

echo "Done."
