"""Build the fix and drop it into a SAMURAI WARRIORS 5 install.

Usage:
    python scripts/install.py <path to 'SAMURAI WARRIORS 5' game folder> [--debug]
"""
import argparse
import shutil
import subprocess
import sys
from pathlib import Path

FIX_NAME = "samurai_warriors5_fix"
ROOT = Path(__file__).resolve().parent.parent


def install(game_folder: str, debug: bool) -> None:
    game_folder = Path(game_folder)
    if not game_folder.is_dir():
        sys.exit(f"Error: '{game_folder}' does not exist or is not a directory.")

    profile = "debug" if debug else "release"
    print(f"Building the fix ({profile})...")
    cmd = ["cargo", "build"]
    if not debug:
        cmd.append("--release")
    subprocess.run(cmd, cwd=ROOT, check=True)

    out = ROOT / "target" / profile
    built = out / f"{FIX_NAME}.dll"
    if not built.is_file():
        sys.exit(f"Error: expected build output at {built}")

    scripts_dir = game_folder / "scripts"
    scripts_dir.mkdir(parents=True, exist_ok=True)

    # No reload path any more (see Cargo.toml), so a locked .asi means the game
    # is running with the old build and there's no way to update it in place --
    # unlike the old host/core split, there's nothing left to overwrite instead.
    dest_asi = scripts_dir / f"{FIX_NAME}.asi"
    try:
        shutil.copy(built, dest_asi)
        print(f"Installed -> {dest_asi}")
    except PermissionError:
        sys.exit(f"Error: {dest_asi} is in use -- close the game fully before installing.")

    # Only when missing, unlike the .asi above: this is a template, and the
    # whole point of testing an override is that it survives the next
    # `install` -- unconditionally overwriting it would silently wipe out
    # whatever resolution you were testing on every rebuild.
    toml_src = ROOT / f"{FIX_NAME}.toml"
    toml_dest = scripts_dir / toml_src.name
    if toml_dest.is_file():
        print(f"Config already exists, leaving it as-is -> {toml_dest}")
    elif toml_src.is_file():
        shutil.copy(toml_src, toml_dest)
        print(f"Copied config -> {toml_dest}")
    else:
        print(f"Warning: no config template found at {toml_src}, skipping")

    print("Done!")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("game_folder", help="path to the 'SAMURAI WARRIORS 5' game folder")
    parser.add_argument("--debug", action="store_true", help="install the debug build instead of release")
    args = parser.parse_args()
    install(args.game_folder, args.debug)


if __name__ == "__main__":
    main()
