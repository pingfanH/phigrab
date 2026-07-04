#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pillow>=10"]
# ///
from __future__ import annotations

import argparse
from pathlib import Path

from PIL import Image, ImageOps


SIZES = (16, 32, 64)
NAME_BY_SIZE = {16: "small", 32: "medium", 64: "big"}
ANDROID_MIPMAP_SIZES = {
    "mipmap-mdpi": 48,
    "mipmap-hdpi": 72,
    "mipmap-xhdpi": 96,
    "mipmap-xxhdpi": 144,
    "mipmap-xxxhdpi": 192,
}
IOS_APPICON_SIZE = 1024


def parse_args() -> argparse.Namespace:
    repo_root = Path(__file__).resolve().parents[1]
    default_input = repo_root / "assets" / "icon.png"
    default_output_dir = repo_root / "phira" / "icon"
    default_android_res_dir = repo_root / "phira-main" / "android-res"
    default_ios_appicon_dir = repo_root / "xcode" / "Assets.xcassets" / "AppIcon.appiconset"

    parser = argparse.ArgumentParser(
        description=(
            "Generate packed RGBA bytes for 16/32/64 icons. "
            "Output layout matches Icon { small, medium, big }."
        )
    )
    parser.add_argument(
        "input",
        nargs="?",
        type=Path,
        default=default_input,
        help=f"Input image path (default: {default_input})",
    )
    parser.add_argument(
        "output_dir",
        nargs="?",
        type=Path,
        default=default_output_dir,
        help=f"Output directory (default: {default_output_dir})",
    )
    parser.add_argument(
        "--ext",
        default="",
        help="Optional file extension (e.g. .rgba). Default: no extension.",
    )
    parser.add_argument(
        "--android-res-dir",
        type=Path,
        default=default_android_res_dir,
        help=f"Android resource output directory (default: {default_android_res_dir})",
    )
    parser.add_argument(
        "--ios-appicon-dir",
        type=Path,
        default=default_ios_appicon_dir,
        help=f"iOS AppIcon.appiconset output directory (default: {default_ios_appicon_dir})",
    )
    return parser.parse_args()


def generate_icon_bytes(input_path: Path, output_dir: Path, ext: str) -> None:
    if not input_path.exists():
        raise FileNotFoundError(f"Input not found: {input_path}")

    resample = getattr(Image, "Resampling", Image).LANCZOS
    img = Image.open(input_path).convert("RGBA")

    output_dir.mkdir(parents=True, exist_ok=True)
    for size in SIZES:
        resized = ImageOps.fit(img, (size, size), method=resample)
        buf = resized.tobytes()
        expected = size * size * 4
        if len(buf) != expected:
            raise ValueError(f"Unexpected output size for {size}: {len(buf)} (expected {expected})")
        name = NAME_BY_SIZE[size]
        output_path = output_dir / f"{name}{ext}"
        output_path.write_bytes(buf)
        print(f"Wrote {len(buf)} bytes to {output_path}")


def generate_android_icons(input_path: Path, output_dir: Path) -> None:
    resample = getattr(Image, "Resampling", Image).LANCZOS
    img = Image.open(input_path).convert("RGBA")

    for folder, size in ANDROID_MIPMAP_SIZES.items():
        target_dir = output_dir / folder
        target_dir.mkdir(parents=True, exist_ok=True)
        resized = ImageOps.fit(img, (size, size), method=resample)
        output_path = target_dir / "ic_launcher.png"
        resized.save(output_path, format="PNG")
        print(f"Wrote {size}x{size} PNG to {output_path}")


def generate_ios_app_icon(input_path: Path, output_dir: Path) -> None:
    resample = getattr(Image, "Resampling", Image).LANCZOS
    img = Image.open(input_path).convert("RGBA")

    output_dir.mkdir(parents=True, exist_ok=True)
    output_path = output_dir / "icon1.png"
    ImageOps.fit(img, (IOS_APPICON_SIZE, IOS_APPICON_SIZE), method=resample).convert("RGB").save(output_path, format="PNG")
    contents = """{
  "images" : [
    {
      "filename" : "icon1.png",
      "idiom" : "universal",
      "platform" : "ios",
      "size" : "1024x1024"
    }
  ],
  "info" : {
    "author" : "xcode",
    "version" : 1
  }
}
"""
    (output_dir / "Contents.json").write_text(contents, encoding="utf-8")
    print(f"Wrote {IOS_APPICON_SIZE}x{IOS_APPICON_SIZE} PNG to {output_path}")


def main() -> None:
    args = parse_args()
    generate_icon_bytes(args.input, args.output_dir, args.ext)
    generate_android_icons(args.input, args.android_res_dir)
    generate_ios_app_icon(args.input, args.ios_appicon_dir)


if __name__ == "__main__":
    main()
