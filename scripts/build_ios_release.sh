#!/usr/bin/env bash
set -euo pipefail

APP_NAME="${APP_NAME:-Phigrab}"
BIN_NAME="${BIN_NAME:-phira-main}"
PACKAGE_ID="${PACKAGE_ID:-com.pingfanh.phigrab}"
FFMPEG_VERSION="${PRPR_AVC_FFMPEG_VERSION:-20260309_v0}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${DIST_DIR:-$ROOT_DIR/dist/ios}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"

usage() {
  printf '%s\n' "Usage: $0 [--target ios-arm64|ios-sim-arm64|all] [--skip-build]"
}

TARGET_SELECTION="ios-arm64"
SKIP_BUILD=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      TARGET_SELECTION="${2:?missing target}"
      shift 2
      ;;
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'Unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

version() {
  awk -F'"' '/^version = / { print $2; exit }' "$ROOT_DIR/Cargo.toml"
}

log() {
  printf '\n==> %s\n' "$*"
}

rust_target_for() {
  case "$1" in
    ios-arm64) printf '%s\n' "aarch64-apple-ios" ;;
    ios-sim-arm64) printf '%s\n' "aarch64-apple-ios-sim" ;;
    *) return 1 ;;
  esac
}

artifact_arch_for() {
  case "$1" in
    ios-arm64) printf '%s\n' "ios-arm64" ;;
    ios-sim-arm64) printf '%s\n' "ios-sim-arm64" ;;
    *) return 1 ;;
  esac
}

targets_for_selection() {
  case "$TARGET_SELECTION" in
    all)
      printf '%s\n' "ios-arm64" "ios-sim-arm64"
      ;;
    ios-arm64|ios-sim-arm64)
      printf '%s\n' "$TARGET_SELECTION"
      ;;
    *)
      printf 'Unsupported target: %s\n' "$TARGET_SELECTION" >&2
      exit 2
      ;;
  esac
}

ensure_macos() {
  if [[ "$(uname -s)" != "Darwin" ]]; then
    printf '%s\n' "iOS builds require a macOS runner with Xcode." >&2
    exit 1
  fi
}

ensure_static_libs() {
  local rust_target="$1"
  local dst="$ROOT_DIR/prpr-avc/static-lib/$rust_target"
  if [[ -d "$dst" ]] && find "$dst" -mindepth 1 -print -quit | grep -q .; then
    return
  fi

  log "Downloading FFmpeg static libs for $rust_target"
  mkdir -p "$dst"
  local archive="$DIST_DIR/cache/$rust_target-prpr-avc-static-lib.tar.gz"
  mkdir -p "$(dirname "$archive")"
  curl -fL --retry 3 \
    -o "$archive" \
    "https://github.com/TeamFlos/prpr-avc-ffmpeg/releases/download/$FFMPEG_VERSION/$rust_target.tar.gz"
  tar -xzf "$archive" -C "$dst"
}

build_target() {
  local rust_target="$1"
  if [[ "$SKIP_BUILD" -eq 1 ]]; then
    log "Skipping build for $rust_target"
    return
  fi

  log "Building $rust_target"
  rustup target add "$rust_target"
  ensure_static_libs "$rust_target"
  IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-13.0}" \
    CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    cargo build --release -p "$BIN_NAME" --target "$rust_target"
}

package_target() {
  local rust_target="$1"
  local artifact_arch="$2"
  local version="$3"
  local app_dir="$DIST_DIR/$APP_NAME-$artifact_arch.app"
  local archive="$DIST_DIR/$APP_NAME-$version-$artifact_arch.app.zip"
  local binary="$CARGO_TARGET_DIR/$rust_target/release/$BIN_NAME"

  log "Packaging $artifact_arch"
  rm -rf "$app_dir"
  mkdir -p "$app_dir"
  cp "$binary" "$app_dir/$APP_NAME"
  chmod +x "$app_dir/$APP_NAME"
  cp -R "$ROOT_DIR/assets" "$app_dir/assets"
  cat > "$app_dir/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>
  <string>$APP_NAME</string>
  <key>CFBundleIdentifier</key>
  <string>$PACKAGE_ID</string>
  <key>CFBundleName</key>
  <string>$APP_NAME</string>
  <key>CFBundleDisplayName</key>
  <string>$APP_NAME</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$version</string>
  <key>CFBundleVersion</key>
  <string>$version</string>
  <key>LSRequiresIPhoneOS</key>
  <true/>
  <key>MinimumOSVersion</key>
  <string>13.0</string>
</dict>
</plist>
PLIST

  rm -f "$archive"
  ditto -c -k --keepParent "$app_dir" "$archive"
}

main() {
  ensure_macos
  cd "$ROOT_DIR"
  mkdir -p "$DIST_DIR"
  local version
  version="$(version)"

  log "Preparing $APP_NAME iOS release $version"
  while IFS= read -r release_target; do
    local rust_target artifact_arch
    rust_target="$(rust_target_for "$release_target")"
    artifact_arch="$(artifact_arch_for "$release_target")"
    build_target "$rust_target"
    package_target "$rust_target" "$artifact_arch" "$version"
  done < <(targets_for_selection)

  log "Done. Artifacts are in $DIST_DIR"
}

main "$@"
