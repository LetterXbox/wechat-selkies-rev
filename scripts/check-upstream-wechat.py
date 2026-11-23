#!/usr/bin/env python3
"""Check official Tencent WeChat Linux .deb packages and update versions/wechat.json."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile
import urllib.request

OFFICIAL_URLS = {
    "amd64": "https://dldir1v6.qq.com/weixin/Universal/Linux/WeChatLinux_x86_64.deb",
    "arm64": "https://dldir1v6.qq.com/weixin/Universal/Linux/WeChatLinux_arm64.deb",
}


def fetch_and_inspect(url: str) -> dict[str, str]:
    req = urllib.request.Request(url, headers={"User-Agent": "wechat-selkies-rev-tracker/1.0"})
    hasher = hashlib.sha256()
    with tempfile.NamedTemporaryFile(suffix=".deb", delete=True) as tmp:
        with urllib.request.urlopen(req, timeout=180) as resp:
            while True:
                chunk = resp.read(1024 * 1024)
                if not chunk:
                    break
                hasher.update(chunk)
                tmp.write(chunk)
        tmp.flush()
        version = subprocess.check_output(
            ["dpkg-deb", "-f", tmp.name, "Version"], text=True
        ).strip()
        if not version:
            raise RuntimeError(f"Empty dpkg Version field for {url}")
    return {
        "url": url,
        "version": version,
        "sha256": hasher.hexdigest(),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest",
        type=pathlib.Path,
        default=pathlib.Path("versions/wechat.json"),
    )
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()

    existing = {}
    if args.manifest.exists():
        existing = json.loads(args.manifest.read_text(encoding="utf-8"))

    architectures = {
        arch: fetch_and_inspect(url) for arch, url in OFFICIAL_URLS.items()
    }
    changed = existing.get("architectures") != architectures
    payload = {
        "schema_version": 1,
        "checked_at": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "architectures": architectures,
    }

    if args.write and changed:
        args.manifest.parent.mkdir(parents=True, exist_ok=True)
        args.manifest.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")

    summary = {
        "changed": changed,
        "amd64_version": architectures["amd64"]["version"],
        "arm64_version": architectures["arm64"]["version"],
        "amd64_sha256": architectures["amd64"]["sha256"],
        "arm64_sha256": architectures["arm64"]["sha256"],
    }
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
