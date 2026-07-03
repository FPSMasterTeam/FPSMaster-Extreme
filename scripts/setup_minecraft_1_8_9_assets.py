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
USER_AGENT = "FPSMaster local asset setup"
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


# Mojang's public asset object store (no login). Objects are content-addressed:
# <RESOURCES>/<hash[:2]>/<hash>. The asset index maps logical paths
# (e.g. "minecraft/sounds/random/pop.ogg") to those hashes.
RESOURCES = "https://resources.download.minecraft.net"


def download_sounds(version_meta: dict, dest: pathlib.Path) -> int:
    """Download the 1.8.9 sound objects (ogg + sounds.json) into the extracted
    asset tree. The client jar ships textures but not sounds, so these come from
    the public asset object store referenced by the version's asset index."""
    index_info = version_meta["assetIndex"]
    index = request_json(index_info["url"])
    objects = index["objects"]
    wanted = {
        path: meta["hash"]
        for path, meta in objects.items()
        if path == "minecraft/sounds.json" or path.startswith("minecraft/sounds/")
    }
    count = 0
    for i, (path, h) in enumerate(sorted(wanted.items())):
        target = dest / "assets" / path
        if target.exists():
            count += 1
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        url = f"{RESOURCES}/{h[:2]}/{h}"
        try:
            download(url, target)
            count += 1
        except Exception as err:  # noqa: BLE001 - best effort per file
            print(f"  WARN failed {path}: {err}")
        if (i + 1) % 100 == 0:
            print(f"  ...{i + 1}/{len(wanted)} sound objects")
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
    print("Downloading 1.8.9 sound objects (ogg + sounds.json)...")
    sounds = download_sounds(version_meta, EXTRACTED)
    print(f"Sound objects ready: {sounds}")
    print("Run with: cargo run -p fpsmaster_app")


if __name__ == "__main__":
    main()
