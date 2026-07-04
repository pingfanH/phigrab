#!/usr/bin/env bash
set -euo pipefail

APP_NAME="${APP_NAME:-Phigrab}"
BIN_NAME="${BIN_NAME:-phira-main}"
PACKAGE_NAME="${PACKAGE_NAME:-phigrab}"
FFMPEG_VERSION="${PRPR_AVC_FFMPEG_VERSION:-20260309_v0}"
BUILD_PROFILE="${BUILD_PROFILE:-release}"
ALLOW_LINUX_CROSS_WITH_ZIG="${ALLOW_LINUX_CROSS_WITH_ZIG:-0}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${DIST_DIR:-$ROOT_DIR/dist/desktop}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"
TOOLCHAIN_DIR="${TOOLCHAIN_DIR:-$DIST_DIR/toolchain}"

usage() {
  printf '%s\n' "Usage: $0 [--target all|windows-x64|macos-arm64|macos-x64|linux-x64] [--skip-build]"
  printf '%s\n' ""
  printf '%s\n' "Examples:"
  printf '%s\n' "  $0 --target all"
  printf '%s\n' "  $0 --target macos-arm64"
  printf '%s\n' "  DIST_DIR=dist/release $0 --target linux-x64"
}

TARGET_SELECTION="all"
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

host_os() {
  case "$(uname -s)" in
    Darwin) printf '%s\n' "macos" ;;
    Linux) printf '%s\n' "linux" ;;
    MINGW*|MSYS*|CYGWIN*) printf '%s\n' "windows" ;;
    *) printf '%s\n' "unknown" ;;
  esac
}

rust_target_for() {
  case "$1" in
    windows-x64) printf '%s\n' "x86_64-pc-windows-msvc" ;;
    macos-arm64) printf '%s\n' "aarch64-apple-darwin" ;;
    macos-x64) printf '%s\n' "x86_64-apple-darwin" ;;
    linux-x64) printf '%s\n' "x86_64-unknown-linux-gnu" ;;
    *) return 1 ;;
  esac
}

targets_for_selection() {
  case "$TARGET_SELECTION" in
    all)
      printf '%s\n' "windows-x64" "macos-arm64" "linux-x64"
      ;;
    windows-x64|macos-arm64|macos-x64|linux-x64)
      printf '%s\n' "$TARGET_SELECTION"
      ;;
    *)
      printf 'Unsupported target: %s\n' "$TARGET_SELECTION" >&2
      exit 2
      ;;
  esac
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

ensure_host_can_build() {
  local release_target="$1"
  local os
  os="$(host_os)"

  case "$release_target" in
    windows-x64)
      if [[ "$os" != "windows" ]]; then
        printf '%s\n' "warning: windows-x64 is intended to be built on windows-latest with MSVC." >&2
      fi
      ;;
    macos-arm64|macos-x64)
      if [[ "$os" != "macos" ]]; then
        printf '%s\n' "warning: macOS packages require macOS/Xcode for a real release build." >&2
      fi
      ;;
    linux-x64)
      if [[ "$os" == "linux" ]]; then
        printf '%s\n' "Linux dependencies may be required: libasound2-dev libglib2.0-dev libgtk-3-dev"
      elif ! command -v x86_64-linux-gnu-gcc >/dev/null 2>&1; then
        if [[ "$ALLOW_LINUX_CROSS_WITH_ZIG" == "1" ]]; then
          printf '%s\n' "warning: linux-x64 cross build will use zig cc because x86_64-linux-gnu-gcc was not found." >&2
        else
          cat >&2 <<'MSG'
linux-x64 release packages should be built on Linux.
This project depends on Linux desktop libraries such as ALSA/Wayland that need a Linux pkg-config sysroot.

Run this on an Ubuntu runner/machine instead:
  sudo apt-get update
  sudo apt-get install -y libasound2-dev libglib2.0-dev libgtk-3-dev pkg-config
  scripts/build_desktop_release.sh --target linux-x64

If you have a complete Linux sysroot and want to experiment from macOS, set:
  ALLOW_LINUX_CROSS_WITH_ZIG=1 scripts/build_desktop_release.sh --target linux-x64
MSG
          exit 1
        fi
      fi
      ;;
  esac
}

write_toolchain_wrapper() {
  local path="$1"
  local tool="$2"
  local target="$3"

  mkdir -p "$(dirname "$path")"
  cat > "$path" <<WRAPPER
#!/usr/bin/env bash
set -euo pipefail
args=()
for arg in "\$@"; do
  case "\$arg" in
    --target=x86_64-unknown-linux-gnu)
      ;;
    *)
      args+=("\$arg")
      ;;
  esac
done
exec $tool -target $target "\${args[@]}"
WRAPPER
  chmod +x "$path"
}

write_simple_wrapper() {
  local path="$1"
  local tool="$2"

  mkdir -p "$(dirname "$path")"
  cat > "$path" <<WRAPPER
#!/usr/bin/env bash
set -euo pipefail
exec $tool "\$@"
WRAPPER
  chmod +x "$path"
}

configure_linux_cross_toolchain() {
  if [[ "$(host_os)" == "linux" ]]; then
    return
  fi
  if command -v x86_64-linux-gnu-gcc >/dev/null 2>&1; then
    return
  fi
  if [[ "$ALLOW_LINUX_CROSS_WITH_ZIG" != "1" ]]; then
    return
  fi
  if ! command -v zig >/dev/null 2>&1; then
    cat >&2 <<'MSG'
linux-x64 cross build needs a Linux C toolchain.
Install one of these, then run the script again:
  brew install zig
  brew install messense/macos-cross-toolchains/x86_64-unknown-linux-gnu

For official distribution builds, the most reliable path is running:
  scripts/build_desktop_release.sh --target linux-x64
on an ubuntu-latest GitHub Actions runner.
MSG
    exit 1
  fi

  local cc="$TOOLCHAIN_DIR/x86_64-unknown-linux-gnu-zig-cc"
  local cxx="$TOOLCHAIN_DIR/x86_64-unknown-linux-gnu-zig-cxx"
  local ar="$TOOLCHAIN_DIR/zig-ar"
  local ranlib="$TOOLCHAIN_DIR/zig-ranlib"
  write_toolchain_wrapper "$cc" "zig cc" "x86_64-linux-gnu"
  write_toolchain_wrapper "$cxx" "zig c++" "x86_64-linux-gnu"
  write_simple_wrapper "$ar" "zig ar"
  write_simple_wrapper "$ranlib" "zig ranlib"

  export CC_x86_64_unknown_linux_gnu="$cc"
  export CXX_x86_64_unknown_linux_gnu="$cxx"
  export AR_x86_64_unknown_linux_gnu="$ar"
  export RANLIB_x86_64_unknown_linux_gnu="$ranlib"
  export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="$cc"
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
  if [[ "$rust_target" == "x86_64-unknown-linux-gnu" ]]; then
    configure_linux_cross_toolchain
  fi
  CARGO_TARGET_DIR="$CARGO_TARGET_DIR" cargo build \
    --release \
    -p "$BIN_NAME" \
    --target "$rust_target"
}

profile_dir() {
  if [[ "$BUILD_PROFILE" == "release" ]]; then
    printf '%s\n' "release"
  else
    printf '%s\n' "$BUILD_PROFILE"
  fi
}

binary_path() {
  local rust_target="$1"
  local exe_suffix="$2"
  printf '%s\n' "$CARGO_TARGET_DIR/$rust_target/$(profile_dir)/$BIN_NAME$exe_suffix"
}

copy_common_files() {
  local out_dir="$1"
  rm -rf "$out_dir/assets"
  cp -R "$ROOT_DIR/assets" "$out_dir/assets"
  cp "$ROOT_DIR/LICENSE" "$out_dir/LICENSE.txt"
  if [[ -f "$ROOT_DIR/README-zh_CN.md" ]]; then
    cp "$ROOT_DIR/README-zh_CN.md" "$out_dir/README-zh_CN.md"
  fi
}

zip_dir() {
  local archive="$1"
  local dir="$2"
  rm -f "$archive"
  if command -v zip >/dev/null 2>&1; then
    (cd "$(dirname "$dir")" && zip -qry "$archive" "$(basename "$dir")")
  elif command -v powershell.exe >/dev/null 2>&1; then
    powershell.exe -NoProfile -Command "\$ErrorActionPreference = 'Stop'; Compress-Archive -Path '$dir' -DestinationPath '$archive' -Force"
  else
    printf '%s\n' "zip command not found; install zip or run on a GitHub runner with archive tools." >&2
    exit 1
  fi
}

package_windows() {
  local rust_target="$1"
  local version="$2"
  local out_dir="$DIST_DIR/$APP_NAME-$version-windows-x64"
  local archive="$DIST_DIR/$APP_NAME-$version-windows-x64.zip"

  log "Packaging Windows x64"
  rm -rf "$out_dir"
  mkdir -p "$out_dir"
  cp "$(binary_path "$rust_target" ".exe")" "$out_dir/$APP_NAME.exe"
  copy_common_files "$out_dir"
  zip_dir "$archive" "$out_dir"
}

package_macos() {
  local rust_target="$1"
  local version="$2"
  local arch="$3"
  local app_dir="$DIST_DIR/$APP_NAME.app"
  local archive="$DIST_DIR/$APP_NAME-$version-macos-$arch.app.zip"
  local dmg="$DIST_DIR/$APP_NAME-$version-macos-$arch.dmg"

  log "Packaging macOS $arch"
  rm -rf "$app_dir"
  mkdir -p "$app_dir/Contents/MacOS" "$app_dir/Contents/Resources"
  cp "$(binary_path "$rust_target" "")" "$app_dir/Contents/MacOS/$APP_NAME"
  chmod +x "$app_dir/Contents/MacOS/$APP_NAME"
  cp -R "$ROOT_DIR/assets" "$app_dir/Contents/MacOS/assets"
  cp "$ROOT_DIR/assets/icon.png" "$app_dir/Contents/Resources/icon.png"
  cp "$ROOT_DIR/LICENSE" "$app_dir/Contents/Resources/LICENSE.txt"
  cat > "$app_dir/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>
  <string>$APP_NAME</string>
  <key>CFBundleIdentifier</key>
  <string>com.pingfanh.phigrab</string>
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
  <key>LSMinimumSystemVersion</key>
  <string>10.15</string>
</dict>
</plist>
PLIST
  printf 'APPL????' > "$app_dir/Contents/PkgInfo"

  rm -f "$archive"
  ditto -c -k --keepParent "$app_dir" "$archive"
  if command -v hdiutil >/dev/null 2>&1; then
    rm -f "$dmg"
    hdiutil create -volname "$APP_NAME" -srcfolder "$app_dir" -ov -format UDZO "$dmg"
  fi
}

package_linux() {
  local rust_target="$1"
  local version="$2"
  local out_dir="$DIST_DIR/$APP_NAME-$version-linux-x64"
  local archive="$DIST_DIR/$APP_NAME-$version-linux-x64.tar.gz"

  log "Packaging Linux x64"
  rm -rf "$out_dir"
  mkdir -p "$out_dir"
  cp "$(binary_path "$rust_target" "")" "$out_dir/$APP_NAME"
  chmod +x "$out_dir/$APP_NAME"
  copy_common_files "$out_dir"
  cat > "$out_dir/$PACKAGE_NAME.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=$APP_NAME
Exec=$APP_NAME
Icon=icon
Categories=Game;
Terminal=false
DESKTOP
  tar -czf "$archive" -C "$DIST_DIR" "$(basename "$out_dir")"

  if command -v appimagetool >/dev/null 2>&1; then
    local appdir="$DIST_DIR/$APP_NAME.AppDir"
    rm -rf "$appdir"
    mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications" "$appdir/usr/share/icons/hicolor/256x256/apps"
    cp "$out_dir/$APP_NAME" "$appdir/usr/bin/$APP_NAME"
    cp -R "$out_dir/assets" "$appdir/usr/bin/assets"
    cp "$out_dir/$PACKAGE_NAME.desktop" "$appdir/usr/share/applications/$PACKAGE_NAME.desktop"
    cp "$ROOT_DIR/assets/icon.png" "$appdir/usr/share/icons/hicolor/256x256/apps/icon.png"
    cp "$out_dir/$PACKAGE_NAME.desktop" "$appdir/$PACKAGE_NAME.desktop"
    cp "$ROOT_DIR/assets/icon.png" "$appdir/icon.png"
    appimagetool "$appdir" "$DIST_DIR/$APP_NAME-$version-linux-x64.AppImage"
  fi
}

package_target() {
  local release_target="$1"
  local rust_target="$2"
  local version="$3"

  case "$release_target" in
    windows-x64) package_windows "$rust_target" "$version" ;;
    macos-arm64) package_macos "$rust_target" "$version" "arm64" ;;
    macos-x64) package_macos "$rust_target" "$version" "x64" ;;
    linux-x64) package_linux "$rust_target" "$version" ;;
  esac
}

main() {
  cd "$ROOT_DIR"
  mkdir -p "$DIST_DIR"
  local version
  version="$(version)"

  log "Preparing $APP_NAME desktop release $version"
  while IFS= read -r release_target; do
    local rust_target
    rust_target="$(rust_target_for "$release_target")"
    ensure_host_can_build "$release_target"
    build_target "$rust_target"
    package_target "$release_target" "$rust_target" "$version"
  done < <(targets_for_selection)

  log "Done. Artifacts are in $DIST_DIR"
}

main "$@"
