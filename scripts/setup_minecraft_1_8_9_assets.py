#!/usr/bin/env python3
"""Download and extract Minecraft Java 1.8.9 client assets for local testing.

The jar and extracted resource-pack-style tree are written to `local_assets/`,
which is ignored by Git. Do not commit Mojang assets to this repository.
"""

from __future__ import annotations

import json
import pathlib
import urllib.request
import zipfile

VERSION = "1.8.9"
ROOT = pathlib.Path(__file__).resolve().parents[1]
ASSET_DIR = ROOT / "local_assets"
DEST = ASSET_DIR / "minecraft-1.8.9-client.jar"
EXTRACTED = ASSET_DIR / "minecraft-1.8.9"
USER_AGENT = "ReCraft local asset setup"
MANIFEST_URL = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"


def request_json(url: str) -> dict:
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=30) as response:
        return json.loads(response.read().decode("utf-8"))


def download(url: str, dest: pathlib.Path) -> None:
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=120) as response:
        dest.write_bytes(response.read())


def extract_assets(jar_path: pathlib.Path, dest: pathlib.Path) -> int:
    count = 0
    dest.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(jar_path) as jar:
        for info in jar.infolist():
            if info.is_dir() or not info.filename.startswith("assets/"):
                continue
            target = dest / info.filename
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(jar.read(info))
            count += 1
    return count


def main() -> None:
    ASSET_DIR.mkdir(parents=True, exist_ok=True)
    manifest = request_json(MANIFEST_URL)
    version_entry = next(item for item in manifest["versions"] if item["id"] == VERSION)
    version_meta = request_json(version_entry["url"])
    client_url = version_meta["downloads"]["client"]["url"]
    if DEST.exists():
        print(f"Already exists: {DEST}")
    else:
        print(f"Downloading {client_url}")
        download(client_url, DEST)
    print(f"Asset jar ready: {DEST}")
    count = extract_assets(DEST, EXTRACTED)
    print(f"Extracted {count} asset files to: {EXTRACTED}")
    print("Run with: cargo run -p recraft_app")


if __name__ == "__main__":
    main()
