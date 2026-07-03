#!/usr/bin/env bash
#
# Build a FPSMaster-Extreme package for distribution through FPSMaster-Launcher.
#
# Unlike scripts/package.ps1 (which builds a run-in-place dist/ with bundled
# Mojang assets + a test server), this produces the launcher-facing artifact
# described in docs/LAUNCHER_INTEGRATION.md:
#
#     FPSMaster-Extreme-<version>-<target>.tar.gz
#     FPSMaster-Extreme-<version>-<target>.tar.gz.sha256
#     FPSMaster-Extreme-<version>-<target>.manifest.json
#
# Deliberately EXCLUDED from the tarball:
#   - local_assets/  (Mojang 1.8.9 assets — the launcher downloads & extracts
#     these legally and points the client at them via --assets)
#   - local_server/  (bundled test server — not needed for end users)
#   - fpsmaster_options.txt (user config — the client generates it on first run)
#
# Usage:
#   scripts/package-launcher.sh [--target <name>] [--download-url <url>] [--dlss] [--out <dir>]
#
#   --target        override the target label (default: auto-detected, e.g. macos-aarch64)
#   --download-url  the URL the tarball will be served from (baked into manifest.json;
#                   defaults to a placeholder the CI/backend can rewrite)
#   --dlss          build with the `dlss` feature and bundle nvngx_dlss.dll (Win/Linux)
#   --out           output directory (default: dist-launcher)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

TARGET=""
DOWNLOAD_URL=""
DLSS=0
OUT_DIR="dist-launcher"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)       TARGET="$2"; shift 2 ;;
    --download-url) DOWNLOAD_URL="$2"; shift 2 ;;
    --dlss)         DLSS=1; shift ;;
    --out)          OUT_DIR="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

# --- Resolve version from the app crate manifest ---
APP_MANIFEST="$REPO_ROOT/crates/fpsmaster_app/Cargo.toml"
VERSION="$(grep -m1 '^version' "$APP_MANIFEST" | sed -E 's/.*"([^"]+)".*/\1/')"
if [[ -z "$VERSION" ]]; then
  echo "could not read version from $APP_MANIFEST" >&2; exit 1
fi

# --- Auto-detect target label (matches the launcher CI's naming) ---
if [[ -z "$TARGET" ]]; then
  os="$(uname -s)"; arch="$(uname -m)"
  case "$os" in
    Darwin) os_label="macos" ;;
    Linux)  os_label="linux" ;;
    MINGW*|MSYS*|CYGWIN*) os_label="windows" ;;
    *) os_label="unknown" ;;
  esac
  case "$arch" in
    arm64|aarch64) arch_label="aarch64" ;;
    x86_64|amd64)  arch_label="x86_64" ;;
    *) arch_label="$arch" ;;
  esac
  TARGET="${os_label}-${arch_label}"
fi

# Binary name is .exe only on Windows targets.
BIN_NAME="fpsmaster_app"
[[ "$TARGET" == windows-* ]] && BIN_NAME="fpsmaster_app.exe"

STAMP="FPSMaster-Extreme-${VERSION}-${TARGET}"
DEFAULT_URL="https://cdn.fpsmaster.top/extreme/${VERSION}/${STAMP}.tar.gz"
[[ -z "$DOWNLOAD_URL" ]] && DOWNLOAD_URL="$DEFAULT_URL"

echo "==> Packaging $STAMP (dlss=$DLSS)"

# --- 1. Build release ---
# NOTE: don't expand an array here — macOS runners ship bash 3.2, where
# "${arr[@]}" on an empty array under `set -u` errors with "unbound variable".
if [[ "$DLSS" == "1" ]]; then
  echo "==> cargo build --release -p fpsmaster_app --features dlss"
  ( cd "$REPO_ROOT" && cargo build --release -p fpsmaster_app --features dlss )
else
  echo "==> cargo build --release -p fpsmaster_app"
  ( cd "$REPO_ROOT" && cargo build --release -p fpsmaster_app )
fi

BIN_SRC="$REPO_ROOT/target/release/$BIN_NAME"
[[ -f "$BIN_SRC" ]] || { echo "build succeeded but binary missing: $BIN_SRC" >&2; exit 1; }

# --- 2. Fresh staging dir (this IS the tarball root / client working dir) ---
OUT_ABS="$REPO_ROOT/$OUT_DIR"
STAGE="$OUT_ABS/$STAMP"
rm -rf "$STAGE"
mkdir -p "$STAGE"

# --- 3. binary ---
cp "$BIN_SRC" "$STAGE/$BIN_NAME"
echo "  + $BIN_NAME"

# --- 4. DLSS dll (Win/Linux only; the launcher never needs it on macOS) ---
if [[ "$DLSS" == "1" ]]; then
  DLL_SRC="${DLSS_SDK:-}/lib/Windows_x86_64/rel/nvngx_dlss.dll"
  if [[ -f "$DLL_SRC" ]]; then
    cp "$DLL_SRC" "$STAGE/nvngx_dlss.dll"
    echo "  + nvngx_dlss.dll"
  else
    echo "  ! --dlss set but nvngx_dlss.dll not found at $DLL_SRC (skipping)" >&2
  fi
fi

# --- 5. bundled mods / resourcepacks / sdk (optional; local_assets excluded) ---
for d in mods resourcepacks sdk; do
  if [[ -d "$REPO_ROOT/$d" ]]; then
    cp -R "$REPO_ROOT/$d" "$STAGE/$d"
    echo "  + $d/"
  fi
done

# --- 6. per-file sha1 manifest (relative paths, POSIX slashes) ---
sha1_of() {
  if command -v sha1sum >/dev/null 2>&1; then sha1sum "$1" | awk '{print $1}';
  else shasum -a 1 "$1" | awk '{print $1}'; fi
}
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}';
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}

FILES_JSON=""
while IFS= read -r -d '' f; do
  rel="${f#"$STAGE/"}"
  h="$(sha1_of "$f")"
  entry="    { \"path\": \"$rel\", \"sha1\": \"$h\" }"
  if [[ -z "$FILES_JSON" ]]; then FILES_JSON="$entry"; else FILES_JSON="$FILES_JSON,
$entry"; fi
done < <(find "$STAGE" -type f -print0)

# --- 7. tarball + sha256 ---
# Archive the STAGE *contents* (not a top-level dir) so the launcher can extract
# straight into the install dir — see docs/LAUNCHER_INTEGRATION.md §2/§4.
TARBALL="$OUT_ABS/${STAMP}.tar.gz"
( cd "$STAGE" && tar -czf "$TARBALL" . )
TAR_SHA256="$(sha256_of "$TARBALL")"
echo "$TAR_SHA256  ${STAMP}.tar.gz" > "$TARBALL.sha256"
echo "  + ${STAMP}.tar.gz  (sha256 $TAR_SHA256)"

# --- 8. manifest.json ---
cat > "$OUT_ABS/${STAMP}.manifest.json" <<EOF
{
  "versionTag": "$VERSION",
  "target": "$TARGET",
  "downloadUrl": "$DOWNLOAD_URL",
  "checksum": "$TAR_SHA256",
  "files": [
$FILES_JSON
  ]
}
EOF
echo "  + ${STAMP}.manifest.json"

echo "==> Done. Artifacts in: $OUT_ABS"
