#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${MANIFEST:-$ROOT/phira-main/Cargo.toml}"
ANDROID_SDK_ROOT="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-$HOME/Library/Android/sdk}}"
ANDROID_HOME="${ANDROID_HOME:-$ANDROID_SDK_ROOT}"
DIST_DIR="${DIST_DIR:-$ROOT/dist/android}"
FFMPEG_VERSION="${PRPR_AVC_FFMPEG_VERSION:-20260309_v0}"
ANDROID_NDK_VERSION="${ANDROID_NDK_VERSION:-26.1.10909125}"
TARGET_SDK="$(awk '
  /^\[package\.metadata\.android\]/ { in_android=1; next }
  /^\[/ { if (in_android==1) in_android=0 }
  in_android && $1 ~ /^target_sdk_version/ {
    gsub(/[^0-9]/, "", $0);
    print $0;
    exit
  }
' "$MANIFEST")"
TARGET_SDK="${TARGET_SDK:-35}"
NDK_HOME="${NDK_HOME:-${ANDROID_NDK_HOME:-$ANDROID_SDK_ROOT/ndk/$ANDROID_NDK_VERSION}}"
case "$(uname -s)" in
  Darwin) HOST_TAG="darwin-x86_64" ;;
  Linux) HOST_TAG="linux-x86_64" ;;
  MINGW*|MSYS*|CYGWIN*) HOST_TAG="windows-x86_64" ;;
  *) echo "Unsupported host OS: $(uname -s)" >&2; exit 1 ;;
esac
TOOLCHAIN_BIN="$NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG/bin"
BUILD_TOOLS_ROOT="$ANDROID_SDK_ROOT/build-tools"

find_sdkmanager() {
  local c1="$ANDROID_SDK_ROOT/cmdline-tools/latest/bin/sdkmanager"
  local c2="$ANDROID_SDK_ROOT/tools/bin/sdkmanager"
  if [[ -x "$c1" ]]; then
    echo "$c1"
    return 0
  fi
  if [[ -x "$c2" ]]; then
    echo "$c2"
    return 0
  fi
  return 1
}

if ! cargo --list | grep -q "quad-apk"; then
  echo "[Phigrab] cargo-quad-apk not found. Installing..."
  cargo install cargo-quad-apk
fi

check_java_toolchain() {
  local java_bin javac_bin keytool_bin
  java_bin="$(command -v java || true)"
  javac_bin="$(command -v javac || true)"
  keytool_bin="$(command -v keytool || true)"
  if [[ -z "$java_bin" || -z "$javac_bin" || -z "$keytool_bin" ]]; then
    echo "[Phigrab] java/javac/keytool not found in PATH."
    return 1
  fi

  echo "[Phigrab] using Java runtime: $("$java_bin" -version 2>&1 | head -n 1)"
}

download_static_libs() {
  local target="$1"
  local dst="$ROOT/prpr-avc/static-lib/$target"
  if [[ -d "$dst" ]] && find "$dst" -mindepth 1 -print -quit | grep -q .; then
    return
  fi
  mkdir -p "$dst"
  local archive="$DIST_DIR/cache/$target-prpr-avc-static-lib.tar.gz"
  mkdir -p "$(dirname "$archive")"
  curl -fL --retry 3 \
    -o "$archive" \
    "https://github.com/TeamFlos/prpr-avc-ffmpeg/releases/download/$FFMPEG_VERSION/$target.tar.gz"
  tar -xzf "$archive" -C "$dst"
}

echo "[Phigrab] ensure Android targets..."
rustup target add aarch64-linux-android armv7-linux-androideabi
download_static_libs aarch64-linux-android
download_static_libs armv7-linux-androideabi
echo "[Phigrab] target sdk from Cargo.toml: android-$TARGET_SDK"

if [[ -x "$TOOLCHAIN_BIN/llvm-ar" ]]; then
  for ar_name in \
    aarch64-linux-android-ar \
    armv7-linux-androideabi-ar \
    x86_64-linux-android-ar \
    i686-linux-android-ar
  do
    if [[ ! -e "$TOOLCHAIN_BIN/$ar_name" ]]; then
      ln -s llvm-ar "$TOOLCHAIN_BIN/$ar_name"
    fi
  done
fi

# cargo-quad-apk / older cargo-ndk helper may still expect legacy *-ld names.
if [[ -x "$TOOLCHAIN_BIN/ld.lld" ]]; then
  for ld_name in \
    aarch64-linux-android-ld \
    arm-linux-androideabi-ld \
    i686-linux-android-ld \
    x86_64-linux-android-ld
  do
    if [[ ! -e "$TOOLCHAIN_BIN/$ld_name" ]]; then
      ln -s ld.lld "$TOOLCHAIN_BIN/$ld_name"
    fi
  done
fi

# cargo-quad-apk may expect legacy binutils names that are absent in newer NDKs.
link_llvm_tool() {
  local legacy_name="$1"
  local llvm_name="$2"
  if [[ -x "$TOOLCHAIN_BIN/$llvm_name" && ! -e "$TOOLCHAIN_BIN/$legacy_name" ]]; then
    ln -s "$llvm_name" "$TOOLCHAIN_BIN/$legacy_name"
  fi
}

for triple in aarch64-linux-android arm-linux-androideabi i686-linux-android x86_64-linux-android; do
  link_llvm_tool "${triple}-readelf" "llvm-readelf"
  link_llvm_tool "${triple}-strip" "llvm-strip"
  link_llvm_tool "${triple}-objcopy" "llvm-objcopy"
  link_llvm_tool "${triple}-nm" "llvm-nm"
done

export NDK_HOME
export ANDROID_NDK_HOME="$NDK_HOME"
export ANDROID_NDK_ROOT="$NDK_HOME"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ROOT/.cargo/android-linker-aarch64.sh"
export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="$ROOT/.cargo/android-linker-armv7.sh"

# NDK r26 + legacy ld invocation may miss libunwind search paths. The per-target
# linker wrapper adds the correct clang runtime directory for each ABI.

if [[ ! -d "$ANDROID_SDK_ROOT/platforms/android-$TARGET_SDK" ]]; then
  echo "[Phigrab] Android platform $TARGET_SDK missing; trying to install..."
  if SDKMANAGER="$(find_sdkmanager)"; then
    yes | "$SDKMANAGER" --sdk_root="$ANDROID_SDK_ROOT" "platforms;android-$TARGET_SDK" "platform-tools" "build-tools;$TARGET_SDK.0.0" >/dev/null
    echo "[Phigrab] installed Android platform $TARGET_SDK."
  else
    echo "[Phigrab] sdkmanager not found."
    echo "Please install Android SDK command-line tools, then run:"
    echo "  sdkmanager --sdk_root=\"$ANDROID_SDK_ROOT\" \"platforms;android-$TARGET_SDK\" \"platform-tools\""
    exit 1
  fi
fi

check_java_toolchain || exit 1

ensure_dx_compat() {
  local latest_bt fallback_dx
  latest_bt="$(ls -1 "$BUILD_TOOLS_ROOT" 2>/dev/null | sort -V | tail -n 1 || true)"
  if [[ -z "$latest_bt" ]]; then
    return 0
  fi
  if [[ -x "$BUILD_TOOLS_ROOT/$latest_bt/dx" ]]; then
    return 0
  fi

  fallback_dx="$(find "$BUILD_TOOLS_ROOT" -maxdepth 2 -type f -name dx | sort -V | tail -n 1 || true)"
  if [[ -n "${fallback_dx:-}" ]]; then
    ln -sf "$fallback_dx" "$BUILD_TOOLS_ROOT/$latest_bt/dx"
    echo "[Phigrab] linked dx for build-tools/$latest_bt -> $fallback_dx"
  else
    echo "[Phigrab] warning: no legacy dx found in build-tools; build may fail."
  fi
}

ensure_dx_compat

mkdir -p "$DIST_DIR"
echo "[Phigrab] building APK via cargo quad-apk..."
cargo quad-apk build --manifest-path "$MANIFEST" --release -p phira-main --out-dir "$DIST_DIR"

echo "[Phigrab] Android artifacts:"
find "$DIST_DIR" -maxdepth 2 -type f | sort
