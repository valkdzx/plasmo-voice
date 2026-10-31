"""Package the mod as a .hnmod, as horizon-mods' native/tools/native.py does, but with the bundle's
.rs and .toml files at every depth: the engine crate lives in a folder of the bundle.

    python tools/package.py            # dist/proximity_voice-<version>.hnmod

The mod hooks one of the game's own functions, so it pins its Horizon commit and names no hook
points' version (`hooks`).
"""
import hashlib
import json
import sys
import tomllib
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SKIP = {"target", ".git"}


def main():
    compat = json.loads((ROOT / "compatibility.json").read_text())
    bundle = next(p for p in (ROOT / "mixins").iterdir() if (p / "horizon-mixin.toml").is_file())
    manifest = tomllib.loads((bundle / "horizon-mixin.toml").read_text())
    mod_id, version = manifest["mod"]["id"], manifest["mod"]["version"]
    pinned = manifest.get("compatibility", {}).get("horizon_revision")
    if pinned != compat["horizon_revision"]:
        sys.exit(f"horizon-mixin.toml pins {pinned}, compatibility.json {compat['horizon_revision']}")
    native = {"schema": 1, "id": mod_id, "version": version, "game": compat["game"],
              "horizon": compat["horizon_revision"], "mixins": compat["horizon_mixins_revision"]}
    files = {"horizon-native.json": (json.dumps(native, indent=2, sort_keys=True) + "\n").encode()}
    for path in sorted(bundle.rglob("*")):
        relative = path.relative_to(bundle)
        if SKIP & set(relative.parts) or not path.is_file():
            continue
        if path.name == "Cargo.lock":
            sys.exit(f"{relative}: horizon-mixins refuses a bundle crate with its own Cargo.lock")
        if path.suffix in (".rs", ".toml"):
            files[f"mixins/{mod_id}/{relative.as_posix()}"] = path.read_bytes().replace(b"\r\n", b"\n")
    out = ROOT / "dist" / f"{mod_id}-{version}.hnmod"
    out.parent.mkdir(exist_ok=True)
    with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, content in sorted(files.items()):
            info = zipfile.ZipInfo(name, (2026, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o100644 << 16
            archive.writestr(info, content)
    print(json.dumps({"file": str(out), "files": len(files), "size": out.stat().st_size,
                      "sha256": hashlib.sha256(out.read_bytes()).hexdigest()}, indent=2))


if __name__ == "__main__":
    main()
