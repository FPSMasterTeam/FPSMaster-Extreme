#!/usr/bin/env python3
"""Download the Minecraft Java 1.8.9 client jar for local texture testing.

The jar is written to `local_assets/` which is ignored by Git. Do not commit
Mojang assets to this repository.
"""

from __future__ import annotations

import json
import pathlib
import urllib.request

VERSION = "1.8.9"
ROOT = pathlib.Path(__file__).resolve().parents[1]
ASSET_DIR = ROOT / "local_assets"
DEST = ASSET_DIR / "minecraft-1.8.9-client.jar"
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
    print(f"Run with: cargo run -p recraft_app -- --assets {DEST}")


if __name__ == "__main__":
    main()
