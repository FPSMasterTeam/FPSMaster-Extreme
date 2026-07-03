#!/usr/bin/env python3
"""Download a modern Paper server + Via stack for testing a 1.8.9 client against Grim.

Grim is a modern-server anticheat (api-version 1.13, compiled against paper-api 1.20.6)
and cannot run on a 1.8.x server. To exercise a protocol-47 (1.8.9) client like FPSMaster
against Grim we run Paper 1.20.6 with ViaVersion + ViaBackwards + ViaRewind so the old
client can connect through Via translation -- exactly the "1.8 PvP on a modern server"
scenario Grim is designed for.

Everything lands under local_server/, which is gitignored.
"""

from __future__ import annotations

import json
import pathlib
import urllib.parse
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
SERVER_DIR = ROOT / "local_server" / "paper-grim-1.20.6"
PLUGINS = SERVER_DIR / "plugins"
PAPER_VERSION = "1.20.6"
UA = "FPSMaster Grim test setup"


def get_json(url: str) -> dict:
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read().decode("utf-8"))


def download(url: str, dest: pathlib.Path) -> None:
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=300) as r:
        dest.write_bytes(r.read())
    print(f"  -> {dest.name} ({dest.stat().st_size // 1024} KiB)")


def fetch_paper() -> None:
    base = f"https://api.papermc.io/v2/projects/paper/versions/{PAPER_VERSION}"
    build = max(get_json(base)["builds"])
    meta = get_json(f"{base}/builds/{build}")
    name = meta["downloads"]["application"]["name"]
    url = f"{base}/builds/{build}/downloads/{name}"
    print(f"Paper {PAPER_VERSION} build {build}: {name}")
    download(url, SERVER_DIR / "paper.jar")


def fetch_modrinth(slug: str, game_version: str = PAPER_VERSION) -> None:
    # Pick the newest version that lists this game_version for a Paper-compatible loader.
    q = urllib.parse.urlencode({
        "loaders": json.dumps(["paper", "spigot", "bukkit", "folia", "purpur"]),
        "game_versions": json.dumps([game_version]),
    })
    versions = get_json(f"https://api.modrinth.com/v2/project/{slug}/version?{q}")
    if not versions:
        # Fall back to the newest version overall (some plugins tag game versions loosely).
        versions = get_json(f"https://api.modrinth.com/v2/project/{slug}/version")
    if not versions:
        raise SystemExit(f"No Modrinth versions found for {slug}")
    v = versions[0]
    primary = next((f for f in v["files"] if f.get("primary")), v["files"][0])
    print(f"{slug} {v['version_number']} (mc {','.join(v['game_versions'][-3:])})")
    download(primary["url"], PLUGINS / primary["filename"])


def main() -> None:
    PLUGINS.mkdir(parents=True, exist_ok=True)
    fetch_paper()
    for slug in ("viaversion", "viabackwards", "viarewind"):
        fetch_modrinth(slug)
    print(f"\nServer dir: {SERVER_DIR}")


if __name__ == "__main__":
    main()
