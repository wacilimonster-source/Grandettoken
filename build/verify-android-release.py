#!/usr/bin/env python3
"""Verify a signed Android update manifest against the actual APK.

Used by the Android CI release job after downloading the published manifest/APK,
and locally before uploading. It deliberately re-checks every field the app
checks, so a manifest that signs correctly but points at a different APK still
fails here.
"""

import argparse
import base64
import hashlib
import json
from pathlib import Path

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

KEY_ID = "grandettoken-android-updates-v1"


def b64url_decode(value: str) -> bytes:
    return base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--public-key", required=True)
    parser.add_argument("--apk", required=True)
    parser.add_argument("--expected-version-name", required=True)
    parser.add_argument("--expected-version-code", required=True, type=int)
    parser.add_argument("--expected-download-url", required=True)
    args = parser.parse_args()

    envelope = json.loads(Path(args.manifest).read_text(encoding="utf-8"))
    if envelope.get("keyId") != KEY_ID:
        raise SystemExit(f"keyId mismatch: {envelope.get('keyId')} != {KEY_ID}")
    payload_bytes = b64url_decode(envelope["payload"])
    signature = b64url_decode(envelope["signature"])
    public_raw = base64.b64decode(Path(args.public_key).read_text(encoding="ascii").strip())
    Ed25519PublicKey.from_public_bytes(public_raw).verify(signature, payload_bytes)

    payload = json.loads(payload_bytes)
    checks = {
        "schemaVersion": (payload.get("schemaVersion"), 1),
        "channel": (payload.get("channel"), "stable"),
        "applicationId": (payload.get("applicationId"), "com.wacil.grandettoken.sync"),
        "versionName": (payload.get("versionName"), args.expected_version_name),
        "versionCode": (payload.get("versionCode"), args.expected_version_code),
        "minSdk": (payload.get("minSdk"), 26),
    }
    for name, (actual, expected) in checks.items():
        if actual != expected:
            raise SystemExit(f"{name} mismatch: {actual!r} != {expected!r}")

    artifacts = payload.get("artifacts")
    if not isinstance(artifacts, list) or len(artifacts) != 1:
        raise SystemExit(f"expected exactly one artifact, got {artifacts!r}")
    artifact = artifacts[0]
    apk = Path(args.apk).read_bytes()
    if artifact.get("abi") != "universal":
        raise SystemExit(f"abi mismatch: {artifact.get('abi')!r}")
    if artifact.get("sizeBytes") != len(apk):
        raise SystemExit(f"size mismatch: {artifact.get('sizeBytes')} != {len(apk)}")
    if artifact.get("sha256") != hashlib.sha256(apk).hexdigest():
        raise SystemExit("sha256 mismatch")
    if artifact.get("downloadUrl") != args.expected_download_url:
        raise SystemExit(
            f"downloadUrl mismatch: {artifact.get('downloadUrl')!r} != {args.expected_download_url!r}"
        )

    print("PASS signature")
    for name in checks:
        print(f"PASS {name}={checks[name][1]}")
    print(f"PASS apk size={len(apk)} sha256={artifact['sha256']}")
    print(f"PASS downloadUrl={artifact['downloadUrl']}")


if __name__ == "__main__":
    main()
