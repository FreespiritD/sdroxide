#!/usr/bin/env bash
# Add sdroxide to the desktop menu when running from the portable tarball.
# (The .deb and the AUR packages do this for you — you do not need this script
# if you installed one of those.)
#
#   ./install-desktop-entry.sh            # install for the current user
#   ./install-desktop-entry.sh --uninstall
#
# Everything lands under $XDG_DATA_HOME (~/.local/share by default), so no root
# is required and nothing outside your home directory is touched.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
entry="$data/applications/sdroxide.desktop"
icon="$data/icons/hicolor/scalable/apps/sdroxide.svg"

if [[ ${1-} == --uninstall ]]; then
  rm -f "$entry" "$icon"
  echo "removed $entry"
else
  [[ -x $here/sdroxide ]] || { echo "sdroxide binary not found next to this script" >&2; exit 1; }
  mkdir -p "$(dirname "$entry")" "$(dirname "$icon")"
  cp "$here/sdroxide.svg" "$icon"
  # Exec must be the absolute path: the tarball is not on $PATH.
  sed "s|^Exec=sdroxide$|Exec=$here/sdroxide|" "$here/sdroxide.desktop" > "$entry"
  chmod 644 "$entry"
  echo "installed $entry -> $here/sdroxide"
fi

# Best-effort cache refresh; most desktops pick the change up on their own.
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" 2>/dev/null || true
command -v gtk-update-icon-cache   >/dev/null && gtk-update-icon-cache -f -t "$data/icons/hicolor" 2>/dev/null || true
