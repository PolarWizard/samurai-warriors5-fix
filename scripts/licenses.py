"""Generate LICENSES: this project's own license plus every dependency's,
via cargo-about.

Usage:
    python scripts/licenses.py
"""
import subprocess
import sys
import tempfile
from pathlib import Path

FIX_NAME = "SamuraiWarriors5Fix"
ROOT = Path(__file__).resolve().parent.parent


def licenses() -> None:
    about_hbs = ROOT / "about.hbs"
    if not about_hbs.is_file():
        sys.exit(f"Error: {about_hbs} not found.")

    print("Generating third-party license text (cargo about)...")
    # cargo-about refuses to write straight to a pipe on Windows (it can come
    # out re-encoded, e.g. license text going through as UTF-16), so it has to
    # write to a real file via -o rather than being captured from stdout.
    with tempfile.TemporaryDirectory() as tmp:
        third_party_path = Path(tmp) / "third-party.txt"
        result = subprocess.run(
            ["cargo", "about", "generate", about_hbs.name, "-o", str(third_party_path)],
            cwd=ROOT, capture_output=True, text=True,
        )
        if result.returncode != 0:
            sys.exit(
                "Error: `cargo about generate` failed -- is cargo-about installed?\n"
                'Install with: cargo install cargo-about --features="cli"\n\n'
                f"{result.stderr}"
            )
        third_party_text = third_party_path.read_text(encoding="utf-8")

    own_license = (ROOT / "LICENSE").read_text(encoding="utf-8").strip()
    banner = "=" * 80

    # Same layout SamuraiWarriors4DXFix's committed LICENSES file uses --
    # banner, name, banner, raw text, repeated per entry -- except here it's
    # generated on demand instead of hand-maintained, since cargo-about
    # already tracks every dependency's license via Cargo.lock.
    dest = ROOT / "LICENSES"
    dest.write_text(f"{banner}\n{FIX_NAME}\n{banner}\n{own_license}\n\n{third_party_text}", encoding="utf-8")
    print(f"Wrote -> {dest}")


if __name__ == "__main__":
    licenses()
