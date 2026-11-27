"""Run Windows GDI regressions with the pinned arboard crate's own test lock."""
from pathlib import Path
import os
import shutil
import subprocess
from prepare_arboard import prepare

ROOT = Path(__file__).resolve().parents[1]

if __name__ == "__main__":
    if os.name != "nt":
        raise SystemExit("Windows GDI tests require Windows")
    prepare()
    # Cargo cannot test a non-member dependency with -p. Its own immutable
    # upstream test lock includes the dev dependencies without changing the
    # production engine lock or modifying Cargo's shared source cache.
    cargo = shutil.which("cargo")
    if not cargo:
        raise RuntimeError("Cargo is required for Windows bitmap tests")
    subprocess.run([cargo, "test", "--release", "--locked", "--manifest-path",
                    str(ROOT / "artifacts/arboard-patched/Cargo.toml"), "--target-dir",
                    str(ROOT / "artifacts/arboard-patched/target"), "--lib", "platform::windows"], check=True)
