#!/usr/bin/env python3
"""Cross-surface consistency check for the capstone release pass.

Compares page-1 text extraction for the same fixture across:
  - Rust library example,
  - release CLI,
  - C ABI example executable,
  - release HTTP server.

The script writes docs/capstone_surface_consistency.json and exits non-zero if
any required surface is unavailable or returns different normalized text.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path


REPO = Path(__file__).resolve().parents[1]
FIXTURE = REPO / "crates" / "engine" / "tests" / "fixtures" / "basicapi.pdf"
DOC_OUT = REPO / "docs" / "capstone_surface_consistency.json"
RELEASE = REPO / "target" / "release"
IS_WINDOWS = os.name == "nt"
EXE = ".exe" if IS_WINDOWS else ""


def run(cmd: list[str], *, timeout: int = 60, env: dict[str, str] | None = None) -> dict:
    start = time.perf_counter()
    try:
        proc = subprocess.run(
            cmd,
            cwd=REPO,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
        return {
            "ok": proc.returncode == 0,
            "exit_code": proc.returncode,
            "elapsed_ms": round((time.perf_counter() - start) * 1000, 1),
            "stdout": proc.stdout,
            "stderr": proc.stderr[-2000:],
        }
    except Exception as exc:  # noqa: BLE001 - failure is report data.
        return {
            "ok": False,
            "exit_code": None,
            "elapsed_ms": round((time.perf_counter() - start) * 1000, 1),
            "stdout": "",
            "stderr": str(exc),
        }


def normalize(text: str) -> str:
    return "\n".join(line.rstrip() for line in text.replace("\r\n", "\n").strip().splitlines())


def digest(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def health(url: str) -> bool:
    try:
        with urllib.request.urlopen(url, timeout=1) as response:  # noqa: S310 - localhost only.
            return response.status == 200
    except Exception:
        return False


def multipart_body(path: Path, fields: dict[str, str]) -> tuple[bytes, str]:
    boundary = "oxide-capstone-boundary"
    chunks: list[bytes] = []
    for name, value in fields.items():
        chunks.extend(
            [
                f"--{boundary}\r\n".encode(),
                f'Content-Disposition: form-data; name="{name}"\r\n\r\n'.encode(),
                value.encode(),
                b"\r\n",
            ]
        )
    chunks.extend(
        [
            f"--{boundary}\r\n".encode(),
            f'Content-Disposition: form-data; name="file"; filename="{path.name}"\r\n'.encode(),
            b"Content-Type: application/pdf\r\n\r\n",
            path.read_bytes(),
            b"\r\n",
            f"--{boundary}--\r\n".encode(),
        ]
    )
    return b"".join(chunks), boundary


def server_extract(server_bin: Path, port: int) -> dict:
    env = os.environ.copy()
    env.update(
        {
            "OXIDE_PORT": str(port),
            "OXIDE_ALLOW_UNAUTHENTICATED": "true",
            "OXIDE_API_KEYS": "",
            "OXIDE_LOG_LEVEL": "warn",
        }
    )
    proc = subprocess.Popen(
        [str(server_bin)],
        cwd=REPO,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        base = f"http://127.0.0.1:{port}"
        deadline = time.time() + 20
        while time.time() < deadline:
            if health(base + "/health"):
                break
            if proc.poll() is not None:
                raise RuntimeError("server exited before health check")
            time.sleep(0.25)
        else:
            raise RuntimeError("server health check timed out")

        body, boundary = multipart_body(FIXTURE, {"pages": "1", "page_markers": "false"})
        request = urllib.request.Request(
            base + "/api/v1/extract-text",
            data=body,
            method="POST",
            headers={"Content-Type": f"multipart/form-data; boundary={boundary}"},
        )
        start = time.perf_counter()
        with urllib.request.urlopen(request, timeout=30) as response:  # noqa: S310 - localhost only.
            text = response.read().decode("utf-8", "replace")
            return {
                "ok": response.status == 200,
                "exit_code": response.status,
                "elapsed_ms": round((time.perf_counter() - start) * 1000, 1),
                "stdout": text,
                "stderr": "",
            }
    except (urllib.error.URLError, RuntimeError) as exc:
        return {"ok": False, "exit_code": None, "elapsed_ms": None, "stdout": "", "stderr": str(exc)}
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()


def main() -> int:
    cli = RELEASE / f"oxide{EXE}"
    server = RELEASE / f"oxide-server{EXE}"
    capi = RELEASE / f"oxide_capi_extract_text{EXE}"
    required = [cli, server, capi, FIXTURE]

    results: dict[str, dict] = {
        "fixture": str(FIXTURE.relative_to(REPO)),
        "commit": run(["git", "rev-parse", "--short", "HEAD"])["stdout"].strip(),
        "surfaces": {},
    }
    missing = [str(path) for path in required if not path.exists()]
    if missing:
        results["missing"] = missing
        DOC_OUT.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
        print(f"missing required artifact(s): {missing}", file=sys.stderr)
        return 1

    commands = {
        "library": [
            "cargo",
            "run",
            "--quiet",
            "-p",
            "oxide-engine",
            "--example",
            "capstone_extract_text",
            "--",
            str(FIXTURE),
        ],
        "cli": [str(cli), "extract-text", str(FIXTURE), "--pages", "1"],
        "c_abi": [str(capi), str(FIXTURE)],
    }
    for name, cmd in commands.items():
        entry = run(cmd, timeout=120)
        text = normalize(entry["stdout"])
        results["surfaces"][name] = {
            "ok": entry["ok"],
            "exit_code": entry["exit_code"],
            "elapsed_ms": entry["elapsed_ms"],
            "sha256": digest(text) if entry["ok"] else None,
            "text_preview": text[:120],
            "stderr": entry["stderr"],
        }

    entry = server_extract(server, 18111)
    text = normalize(entry["stdout"])
    results["surfaces"]["server"] = {
        "ok": entry["ok"],
        "exit_code": entry["exit_code"],
        "elapsed_ms": entry["elapsed_ms"],
        "sha256": digest(text) if entry["ok"] else None,
        "text_preview": text[:120],
        "stderr": entry["stderr"],
    }

    ok_entries = [v for v in results["surfaces"].values() if v["ok"]]
    hashes = {v["sha256"] for v in ok_entries}
    results["consistent"] = len(ok_entries) == 4 and len(hashes) == 1
    DOC_OUT.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(results, indent=2))
    return 0 if results["consistent"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
