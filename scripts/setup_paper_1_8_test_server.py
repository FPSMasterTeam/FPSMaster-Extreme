#!/usr/bin/env python3
"""Download a local Paper 1.8.8 test server for protocol-47 client testing.

Paper 1.8.8 uses Minecraft protocol 47, the same protocol version used by a
1.8.9 client. The downloaded jar and generated world are placed under
`local_server/`, which is intentionally ignored by Git.
"""

from __future__ import annotations

import json
import pathlib
import urllib.request

PROJECT = "paper"
VERSION = "1.8.8"
ROOT = pathlib.Path(__file__).resolve().parents[1]
SERVER_DIR = ROOT / "local_server" / "paper-1.8-protocol47"
USER_AGENT = "FPSMaster local test setup"


def request_json(url: str) -> dict:
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=30) as response:
        return json.loads(response.read().decode("utf-8"))


def download(url: str, dest: pathlib.Path) -> None:
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=120) as response:
        dest.write_bytes(response.read())


def main() -> None:
    SERVER_DIR.mkdir(parents=True, exist_ok=True)
    # PaperMC's legacy v2 API was retired (HTTP 410); use the v3 "Fill" API.
    builds_url = f"https://fill.papermc.io/v3/projects/{PROJECT}/versions/{VERSION}/builds"
    builds = request_json(builds_url)
    latest = max(builds, key=lambda b: b["id"])
    download_meta = latest["downloads"]["server:default"]
    jar_name = download_meta["name"]
    jar_url = download_meta["url"]
    jar_path = SERVER_DIR / jar_name

    if not jar_path.exists():
        print(f"Downloading {jar_url}")
        download(jar_url, jar_path)
    else:
        print(f"Already exists: {jar_path}")

    (SERVER_DIR / "eula.txt").write_text("eula=true\n", encoding="utf-8")
    (SERVER_DIR / "server.properties").write_text(
        "online-mode=false\n"
        "server-port=25565\n"
        "gamemode=0\n"
        "difficulty=1\n"
        "spawn-protection=0\n"
        "view-distance=8\n",
        encoding="utf-8",
    )
    run_script = SERVER_DIR / "run.sh"
    run_script.write_text(f"#!/usr/bin/env bash\ncd \"$(dirname \"$0\")\"\njava -Xms512M -Xmx1G -jar {jar_name} nogui\n", encoding="utf-8")
    run_script.chmod(0o755)
    print(f"Server ready: {SERVER_DIR}")
    print(f"Run: {run_script}")


if __name__ == "__main__":
    main()
