#!/usr/bin/env python3
"""Verify that the Moon CLI rejects actual native Graph producer fixtures."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def check_fixtures(directory: Path, moon: str) -> dict:
    module = Path(__file__).resolve().parents[1]
    commands = {
        "graph-updates.bin": ["transcode", "decode-updates", "export-jsonschema"],
        "graph-snapshot.bin": ["transcode", "export-deep-json"],
        "graph-shallow.bin": ["transcode", "export-deep-json"],
        "graph-state-only.bin": ["transcode", "export-deep-json"],
        "graph-updates.json": ["encode-jsonschema"],
    }
    # Require every producer output before testing; missing fixtures are failures.
    inputs = {name: (directory / name).read_bytes() for name in commands}
    # Compile separately so compiler diagnostics cannot be mistaken for CLI output.
    compiled = subprocess.run(
        [moon, "run", "--build-only", "--target", "js", "-j", "2", "--no-render",
         "cmd/loro_codec_cli"],
        cwd=module, capture_output=True, text=True, timeout=30,
    )
    if compiled.returncode != 0:
        raise AssertionError(f"Moon CLI compilation failed:\n{compiled.stdout}\n{compiled.stderr}")
    build = module / "_build"
    build.mkdir(exist_ok=True)
    results = []
    with tempfile.TemporaryDirectory(prefix="graph-fixtures-", dir=build) as tmp:
        for name, checks in commands.items():
            data = inputs[name]
            if not data:
                raise AssertionError(f"{name}: fixture is empty")
            for command in checks:
                output = Path(tmp) / f"{name}-{command}.bin"
                args = [
                    moon, "run", "--target", "js", "-j", "2", "--no-render",
                    "cmd/loro_codec_cli", "--", command, str(directory / name),
                ]
                if command in ("transcode", "encode-jsonschema"):
                    args.append(str(output))
                result = subprocess.run(
                    args, cwd=module, capture_output=True, text=True, timeout=30,
                )
                expected = "decode error: unsupported Graph container (type 6)"
                if result.returncode != 2 or result.stdout.strip() != expected:
                    raise AssertionError(
                        f"{name} / {command}: expected unsupported Graph exit 2; "
                        f"got {result.returncode}\n{result.stdout}\n{result.stderr}"
                    )
                if output.exists():
                    raise AssertionError(f"{name} / {command}: wrote output on failure")
            results.append({
                "file": name,
                "bytes": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
                "checks": checks,
            })
    return {"status": "passed", "target": "js", "validate": True, "results": results}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixture_dir", type=Path)
    parser.add_argument("--moon", default=os.environ.get("MOON_BIN", "moon"))
    args = parser.parse_args()
    try:
        print(json.dumps(check_fixtures(args.fixture_dir.resolve(), args.moon), indent=2))
    except (OSError, AssertionError, subprocess.SubprocessError) as error:
        print(str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
