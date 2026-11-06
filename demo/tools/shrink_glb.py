#!/usr/bin/env python3
"""Shrink a shipped GLB: re-encode its textures, leave its geometry alone.

Meshy returns a 2K base colour map per model, which is most of a 4 MB file and roughly eight
times more texture than a 12,000-triangle demo prop can show. This script rewrites the texture
images at a smaller resolution and re-lays the binary chunk, so the mesh, its accessors and its
indices come through byte-for-byte identical.

It is a deliberate part of the pipeline rather than a one-off: `assets/manifest.json` records the
size and fingerprint of what actually ships, and the demo refuses to start on a GLB that has lost
its geometry, so re-running this after a regeneration is the documented step.

    python3 demo/tools/shrink_glb.py demo/assets/npcs/*.glb --max 512
    python3 demo/tools/shrink_glb.py demo/assets/props/*.glb --max 256 --report

No `pygltflib`, no `gltf-transform`: a GLB is a header, a JSON chunk and a binary chunk, and
reading those three things is less code than installing a library to read them.
"""

from __future__ import annotations

import argparse
import io
import json
import struct
import sys
from pathlib import Path

try:
    from PIL import Image
except ImportError:  # pragma: no cover - the pipeline is documented as needing Pillow
    print("shrink_glb: Pillow is required (pip install pillow)", file=sys.stderr)
    raise

GLB_MAGIC = b"glTF"
CHUNK_JSON = 0x4E4F534A
CHUNK_BIN = 0x004E4942


def read_glb(path: Path) -> tuple[dict, bytes]:
    """Split a GLB into its JSON document and its binary chunk."""
    data = path.read_bytes()
    if len(data) < 20 or data[:4] != GLB_MAGIC:
        raise SystemExit(f"{path}: not a GLB")
    version, _length = struct.unpack_from("<II", data, 4)
    if version != 2:
        raise SystemExit(f"{path}: glTF version {version}, expected 2")
    document = None
    binary = b""
    offset = 12
    while offset + 8 <= len(data):
        length, kind = struct.unpack_from("<II", data, offset)
        chunk = data[offset + 8 : offset + 8 + length]
        if kind == CHUNK_JSON:
            document = json.loads(chunk.decode("utf-8"))
        elif kind == CHUNK_BIN:
            binary = chunk
        offset += 8 + length
    if document is None:
        raise SystemExit(f"{path}: no JSON chunk")
    return document, binary


def write_glb(path: Path, document: dict, binary: bytes) -> None:
    """Write a GLB with 4-byte-aligned chunks."""
    text = json.dumps(document, separators=(",", ":")).encode("utf-8")
    text += b" " * (-len(text) % 4)
    binary += b"\x00" * (-len(binary) % 4)
    total = 12 + 8 + len(text) + (8 + len(binary) if binary else 0)
    out = io.BytesIO()
    out.write(GLB_MAGIC)
    out.write(struct.pack("<II", 2, total))
    out.write(struct.pack("<II", len(text), CHUNK_JSON))
    out.write(text)
    if binary:
        out.write(struct.pack("<II", len(binary), CHUNK_BIN))
        out.write(binary)
    path.write_bytes(out.getvalue())


def reencode(image: bytes, max_side: int) -> tuple[bytes, str]:
    """Downscale one image, choosing the container from the pixels rather than the source."""
    with Image.open(io.BytesIO(image)) as opened:
        opened.load()
        has_alpha = opened.mode in ("RGBA", "LA") and opened.getchannel("A").getextrema()[0] < 250
        if max(opened.size) > max_side:
            opened.thumbnail((max_side, max_side), Image.LANCZOS)
        buffer = io.BytesIO()
        if has_alpha:
            opened.save(buffer, format="PNG", optimize=True)
            return buffer.getvalue(), "image/png"
        opened.convert("RGB").save(buffer, format="JPEG", quality=88, optimize=True)
        return buffer.getvalue(), "image/jpeg"


def shrink(path: Path, max_side: int, quality_note: bool) -> tuple[int, int]:
    """Rewrite one file, returning (before, after) byte counts."""
    before = path.stat().st_size
    document, binary = read_glb(path)
    views = document.get("bufferViews", [])
    images = document.get("images", [])

    replacements: dict[int, tuple[bytes, str | None]] = {}
    for image in images:
        view_index = image.get("bufferView")
        if view_index is None or view_index >= len(views):
            continue
        view = views[view_index]
        start = view.get("byteOffset", 0)
        length = view["byteLength"]
        payload = binary[start : start + length]
        try:
            encoded, mime = reencode(payload, max_side)
        except Exception as error:  # noqa: BLE001 - a codec refusal should not lose the asset
            print(f"  {path.name}: kept image {view_index} as-is ({error})", file=sys.stderr)
            continue
        image["mimeType"] = mime
        replacements[view_index] = (encoded, mime)

    if not replacements:
        return before, before

    # Re-lay the binary chunk: every view keeps its index, so accessors and images keep pointing at
    # the right bytes, but the regions move and the image regions change size.
    out = io.BytesIO()
    for index, view in enumerate(views):
        if index in replacements:
            payload = replacements[index][0]
        else:
            start = view.get("byteOffset", 0)
            payload = binary[start : start + view["byteLength"]]
        while out.tell() % 4:
            out.write(b"\x00")
        view["byteOffset"] = out.tell()
        view["byteLength"] = len(payload)
        out.write(payload)
    new_binary = out.getvalue()
    while len(new_binary) % 4:
        new_binary += b"\x00"
    if document.get("buffers"):
        document["buffers"][0]["byteLength"] = len(new_binary)

    write_glb(path, document, new_binary)
    after = path.stat().st_size
    if quality_note:
        triangles = 0
        for mesh in document.get("meshes", []):
            for primitive in mesh.get("primitives", []):
                indices = primitive.get("indices")
                if indices is None:
                    continue
                accessor = document["accessors"][indices]
                triangles += accessor["count"] // 3
        height = 0.0
        for accessor in document.get("accessors", []):
            low, high = accessor.get("min"), accessor.get("max")
            if low and high and len(low) == 3:
                # glTF is Y-up, and Meshy places the origin at the bottom, so max.y is the height.
                height = max(height, float(high[1]) - float(low[1]))
        print(
            f"  {path.name:<22} {before/1e6:5.2f} MB -> {after/1e6:5.2f} MB"
            f"  {triangles:>6} tris  {height:4.2f} m tall"
        )
    return before, after


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("files", nargs="+", type=Path)
    parser.add_argument("--max", type=int, default=512, help="longest texture side in pixels")
    parser.add_argument("--report", action="store_true", help="print size, triangles and height")
    arguments = parser.parse_args()

    total_before = total_after = 0
    for path in arguments.files:
        before, after = shrink(path, arguments.max, arguments.report)
        total_before += before
        total_after += after
    if not arguments.report:
        for path in arguments.files:
            print(f"  {path.name:<22} {path.stat().st_size/1e6:5.2f} MB")
    print(
        f"total {total_before/1e6:.2f} MB -> {total_after/1e6:.2f} MB"
        f" ({100.0 * (1.0 - total_after / max(total_before, 1)):.0f}% smaller)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
