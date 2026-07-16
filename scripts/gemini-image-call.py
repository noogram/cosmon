#!/usr/bin/env python3
"""Gemini Image edit call — Nano Banana Pro photoreal smoke test.

Reads ~/.playhouse/google.toml for the API key, sends one
generateContent request with text + base image input, writes the
returned image to a target path with a manifest.toml beside it.
"""
from __future__ import annotations

import base64
import hashlib
import json
import pathlib
import sys
import time
import urllib.error
import urllib.request


def blake3_hash(path: pathlib.Path) -> str:
    """Return BLAKE3 hex digest of a file, or fall back to SHA-256 prefixed."""
    try:
        import blake3  # type: ignore
        h = blake3.blake3()
        with path.open("rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        return h.hexdigest()
    except ImportError:
        h = hashlib.sha256()
        with path.open("rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        return f"sha256:{h.hexdigest()}"


def load_api_key() -> str:
    creds = pathlib.Path.home() / ".playhouse" / "google.toml"
    text = creds.read_text(encoding="utf-8")
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("api_key") and "=" in line:
            value = line.split("=", 1)[1].strip()
            return value.strip(' "\'')
    raise RuntimeError("api_key not found in google.toml")


def call_gemini(model_id: str, api_key: str, prompt: str, base_image: pathlib.Path) -> dict:
    endpoint = (
        f"https://generativelanguage.googleapis.com/v1beta/"
        f"models/{model_id}:generateContent?key={api_key}"
    )
    image_b64 = base64.b64encode(base_image.read_bytes()).decode("ascii")
    body = {
        "contents": [
            {
                "role": "user",
                "parts": [
                    {"text": prompt},
                    {
                        "inlineData": {
                            "mimeType": "image/png",
                            "data": image_b64,
                        }
                    },
                ],
            }
        ],
    }
    req = urllib.request.Request(
        endpoint,
        data=json.dumps(body).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=180) as resp:
            return {"status": resp.status, "body": json.loads(resp.read())}
    except urllib.error.HTTPError as e:
        try:
            err_body = json.loads(e.read())
        except Exception:
            err_body = {"raw": str(e)}
        return {"status": e.code, "error": err_body}


def extract_image(payload: dict) -> tuple[bytes | None, str | None]:
    candidates = payload.get("candidates", [])
    for cand in candidates:
        content = cand.get("content", {})
        for part in content.get("parts", []):
            inline = part.get("inlineData") or part.get("inline_data")
            if inline and inline.get("data"):
                mime = inline.get("mimeType") or inline.get("mime_type") or "image/png"
                return base64.b64decode(inline["data"]), mime
    return None, None


def main() -> int:
    if len(sys.argv) < 5:
        print("usage: gemini-image-call.py <model_id> <prompt_file> <base_image> <output_path>", file=sys.stderr)
        return 2
    model_id = sys.argv[1]
    prompt_file = pathlib.Path(sys.argv[2])
    base_image = pathlib.Path(sys.argv[3])
    output_path = pathlib.Path(sys.argv[4])

    api_key = load_api_key()
    prompt = prompt_file.read_text(encoding="utf-8").strip()

    output_path.parent.mkdir(parents=True, exist_ok=True)
    log_path = output_path.with_suffix(".call.json")
    manifest_path = output_path.with_name(output_path.stem + "-manifest.toml")

    t0 = time.time()
    result = call_gemini(model_id, api_key, prompt, base_image)
    t1 = time.time()

    log_path.write_text(json.dumps({
        "model_id": model_id,
        "status": result.get("status"),
        "elapsed_s": round(t1 - t0, 2),
        "error": result.get("error"),
    }, indent=2), encoding="utf-8")

    if "error" in result:
        print(f"ERROR status={result['status']}", file=sys.stderr)
        print(json.dumps(result["error"], indent=2)[:2000], file=sys.stderr)
        return 1

    payload = result["body"]
    image_bytes, mime = extract_image(payload)
    if image_bytes is None:
        print("no inlineData image returned; raw payload:", file=sys.stderr)
        print(json.dumps(payload, indent=2)[:4000], file=sys.stderr)
        # write payload for debugging
        log_path.write_text(json.dumps({
            "model_id": model_id,
            "status": result.get("status"),
            "elapsed_s": round(t1 - t0, 2),
            "payload": payload,
        }, indent=2), encoding="utf-8")
        return 1

    output_path.write_bytes(image_bytes)
    base_hash = blake3_hash(base_image)
    out_hash = blake3_hash(output_path)
    manifest_path.write_text(
        f"""# playhouse photoreal smoke test — Gemini Image
model_id = "{model_id}"
endpoint = "https://generativelanguage.googleapis.com/v1beta"
elapsed_s = {round(t1 - t0, 2)}
mime_type = "{mime}"
prompt_file = "{prompt_file}"
base_image = "{base_image}"
base_image_hash = "{base_hash}"
output_path = "{output_path}"
output_hash = "{out_hash}"
seed = "n/a"  # Gemini Image API does not expose seed
""",
        encoding="utf-8",
    )
    print(f"OK wrote {output_path} ({len(image_bytes)} bytes)")
    print(f"manifest: {manifest_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
