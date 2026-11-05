#!/usr/bin/env python3
"""Sign an Android update payload with the DPAPI-protected Ed25519 key.

The private key is decrypted in memory through Windows DPAPI and never written
to disk or printed. Run this on the same Windows user/machine that owns the
protected material.
"""

import argparse
import base64
import ctypes
import json
from ctypes import wintypes
from pathlib import Path

from cryptography.hazmat.primitives.serialization import (
    Encoding,
    PublicFormat,
    load_pem_private_key,
)


class DATA_BLOB(ctypes.Structure):
    _fields_ = [
        ("cbData", wintypes.DWORD),
        ("pbData", ctypes.POINTER(ctypes.c_byte)),
    ]


def unprotect_dpapi(blob: bytes) -> bytes:
    crypt32 = ctypes.WinDLL("crypt32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    crypt32.CryptUnprotectData.argtypes = [
        ctypes.POINTER(DATA_BLOB),
        ctypes.POINTER(wintypes.LPWSTR),
        ctypes.POINTER(DATA_BLOB),
        ctypes.c_void_p,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.POINTER(DATA_BLOB),
    ]
    crypt32.CryptUnprotectData.restype = wintypes.BOOL
    kernel32.LocalFree.argtypes = [ctypes.c_void_p]
    kernel32.LocalFree.restype = ctypes.c_void_p

    buffer = ctypes.create_string_buffer(blob)
    data_in = DATA_BLOB(
        len(blob),
        ctypes.cast(buffer, ctypes.POINTER(ctypes.c_byte)),
    )
    data_out = DATA_BLOB()
    if not crypt32.CryptUnprotectData(
        ctypes.byref(data_in), None, None, None, None, 0, ctypes.byref(data_out)
    ):
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        return ctypes.string_at(data_out.pbData, data_out.cbData)
    finally:
        kernel32.LocalFree(data_out.pbData)


def b64url(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--payload", required=True, help="raw payload JSON file")
    parser.add_argument("--out", required=True, help="signed envelope output")
    parser.add_argument(
        "--private-key",
        default=str(Path.home() / ".android" / "Grandettoken" / "manifest-private.dpapi"),
    )
    parser.add_argument(
        "--public-key",
        default=str(Path.home() / ".android" / "Grandettoken" / "manifest-public.txt"),
    )
    parser.add_argument("--key-id", default="grandettoken-android-updates-v1")
    args = parser.parse_args()

    encrypted = bytes.fromhex(Path(args.private_key).read_text(encoding="ascii").strip())
    pem_text = unprotect_dpapi(encrypted).decode("utf-16le").strip()
    private_key = load_pem_private_key(pem_text.encode("ascii"), password=None)
    actual_public = private_key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
    expected_public = base64.b64decode(Path(args.public_key).read_text(encoding="ascii").strip())
    if actual_public != expected_public:
        raise SystemExit("private key does not match manifest-public.txt")

    payload = Path(args.payload).read_bytes()
    signature = private_key.sign(payload)
    envelope = {
        "keyId": args.key_id,
        "payload": b64url(payload),
        "signature": b64url(signature),
    }
    Path(args.out).write_text(
        json.dumps(envelope, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    print(f"signed {args.out} ({len(payload)} payload bytes)")


if __name__ == "__main__":
    main()
