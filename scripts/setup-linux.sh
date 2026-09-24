#!/usr/bin/env bash
# setup-linux.sh — one-time setup for Splitter on Linux (Debian/Ubuntu/Fedora/Arch)
#
# Installs the GUI and audio runtime libraries: WebKitGTK, libxdo and ALSA
# (playback goes through ALSA; PulseAudio/PipeWire provide an ALSA device).
#
# Usage (from the extracted release archive):
#   chmod +x setup-linux.sh && sudo ./setup-linux.sh

set -e

info()  { echo -e "\033[34m[info]\033[0m  $*"; }
ok()    { echo -e "\033[32m[ok]\033[0m    $*"; }
skip()  { echo -e "\033[33m[skip]\033[0m  $*"; }
err()   { echo -e "\033[31m[error]\033[0m $*"; exit 1; }

# ── Detect distro ─────────────────────────────────────────────────────────────
if   command -v apt-get &>/dev/null; then DISTRO=debian
elif command -v dnf     &>/dev/null; then DISTRO=fedora
elif command -v pacman  &>/dev/null; then DISTRO=arch
else err "Unsupported distro — install dependencies manually (see README)"
fi

info "Detected distro family: $DISTRO"

# ── Runtime dependencies (WebKitGTK + libxdo + ALSA) ─────────────────────────
case "$DISTRO" in
  debian)
    PKGS=()
    dpkg -l libwebkit2gtk-4.1-0 &>/dev/null || dpkg -l libwebkit2gtk-4.0-0 &>/dev/null || PKGS+=(libwebkit2gtk-4.1-0)
    dpkg -l libxdo3 &>/dev/null || PKGS+=(libxdo3)
    # Ubuntu 24.04 renamed the ALSA library package for the 64-bit time_t transition.
    dpkg -l libasound2 &>/dev/null || dpkg -l libasound2t64 &>/dev/null || PKGS+=(libasound2)
    if [ ${#PKGS[@]} -gt 0 ]; then
      info "Installing runtime libs: ${PKGS[*]}"
      apt-get update -qq
      apt-get install -y "${PKGS[@]}" 2>/dev/null \
        || apt-get install -y libwebkit2gtk-4.1-0 libxdo3 libasound2t64 2>/dev/null \
        || apt-get install -y libwebkit2gtk-4.0-0 libxdo3 libasound2
      ok "Runtime libs installed"
    else
      skip "Runtime libs already installed"
    fi
    ;;
  fedora)
    if ! rpm -q webkit2gtk4.1 &>/dev/null && ! rpm -q webkit2gtk3 &>/dev/null; then
      info "Installing WebKitGTK runtime..."
      dnf install -y webkit2gtk4.1 2>/dev/null || dnf install -y webkit2gtk3
      ok "WebKitGTK installed"
    else
      skip "WebKitGTK already installed"
    fi
    rpm -q xdotool &>/dev/null || dnf install -y xdotool
    rpm -q alsa-lib &>/dev/null || dnf install -y alsa-lib
    ;;
  arch)
    if ! pacman -Qi webkit2gtk-4.1 &>/dev/null && ! pacman -Qi webkit2gtk &>/dev/null; then
      info "Installing WebKitGTK runtime..."
      pacman -S --noconfirm webkit2gtk-4.1 2>/dev/null || pacman -S --noconfirm webkit2gtk
      ok "WebKitGTK installed"
    else
      skip "WebKitGTK already installed"
    fi
    pacman -Qi xdotool &>/dev/null || pacman -S --noconfirm xdotool
    pacman -Qi alsa-lib &>/dev/null || pacman -S --noconfirm alsa-lib
    ;;
esac

# ── Desktop shortcut (optional) ───────────────────────────────────────────────
BINARY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DESKTOP_FILE="/usr/share/applications/splitter.desktop"

# Register the bundled icon with the hicolor theme so launchers pick it up.
# Falls back to a generic audio icon if the PNG isn't shipped.
ICON_NAME="audio-x-generic"
if [[ -f "$BINARY_DIR/icon.png" ]]; then
  ICON_DIR="/usr/share/icons/hicolor/256x256/apps"
  install -Dm644 "$BINARY_DIR/icon.png" "$ICON_DIR/splitter.png"
  if command -v gtk-update-icon-cache &>/dev/null; then
    gtk-update-icon-cache -q -t /usr/share/icons/hicolor || true
  fi
  ICON_NAME="splitter"
  ok "Icon installed to $ICON_DIR/splitter.png"
fi

if [[ -f "$BINARY_DIR/splitter" && ! -f "$DESKTOP_FILE" ]]; then
  info "Creating .desktop launcher..."
  cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Name=Splitter
Comment=Split long MP3/WAV recordings into tracks
Exec=$BINARY_DIR/splitter %f
Icon=$ICON_NAME
Terminal=false
Type=Application
Categories=AudioVideo;Audio;AudioVideoEditing;
MimeType=audio/mpeg;audio/x-wav;audio/wav;inode/directory;
StartupWMClass=splitter
EOF
  if command -v update-desktop-database &>/dev/null; then
    update-desktop-database -q /usr/share/applications || true
  fi
  ok "Desktop shortcut created"
fi

echo ""
echo "Setup complete. Run ./splitter to start the app."
