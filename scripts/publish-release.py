#!/usr/bin/env python3

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib


class ReleaseError(Exception):
    pass


def run(root, *args, capture=True):
    return subprocess.run(
        args, cwd=root, check=True, stdout=subprocess.PIPE if capture else None
    ).stdout


def require_clean(root):
    if run(root, "git", "status", "--porcelain=v1", "--untracked-files=all"):
        raise ReleaseError("The working tree or index has uncommitted changes.")


def bump_version(version, bump_minor):
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version):
        raise ReleaseError(f"Expected a major.minor.patch version, got {version!r}.")

    major, minor, patch = map(int, version.split("."))
    patch += 1

    if bump_minor or patch > 100:
        minor += 1
        patch = 0

    return f"{major}.{minor}.{patch}"


def replace_version(data, old, new):
    pattern = rb"(?m)^(version[ \t]*=[ \t]*)([\"'])" + re.escape(old.encode()) + rb"\2"
    updated, count = re.subn(
        pattern,
        lambda match: match[1] + match[2] + new.encode() + match[2],
        data,
    )

    if count != 1:
        raise ReleaseError("Expected exactly one version assignment in the section.")

    return updated


def expected_manifest(data, old, new):
    sections = re.split(rb"(?m)^(?=\[)", data)

    for index, section in enumerate(sections):
        if re.match(rb"\[workspace\.package\][ \t]*(?:#[^\r\n]*)?\r?\n", section):
            sections[index] = replace_version(section, old, new)

            return b"".join(sections)

    raise ReleaseError("Cargo.toml has no [workspace.package] section.")


def expected_lock(data, names, old, new):
    sections = re.split(rb"(?m)^(?=\[\[package\]\])", data)

    found = set()

    for index, section in enumerate(sections):
        if not section.startswith(b"[[package]]"):
            continue

        package = tomllib.loads(section.decode())["package"][0]
        name = package["name"]

        if name in names and "source" not in package:
            if package["version"] != old:
                raise ReleaseError(f"Cargo.lock has an unexpected version for {name}.")

            sections[index] = replace_version(section, old, new)
            found.add(name)

    if found != names:
        raise ReleaseError(f"Workspace packages missing from Cargo.lock: {names - found}")

    updated = b"".join(sections)

    # Duplicate package names can require versions in dependency references.
    for name in names:
        updated = re.sub(
            rb'(?m)^([ \t]*"' + re.escape(f"{name} {old}".encode()) + rb')(",?\r?$)',
            lambda match: match[1][:-len(old)] + new.encode() + match[2],
            updated,
        )

    return updated


def verify_changes(root, expected, head):
    if run(root, "git", "rev-parse", "HEAD").strip() != head:
        raise ReleaseError("HEAD changed during the release checks.")

    if run(root, "git", "diff", "--cached", "--name-only"):
        raise ReleaseError("The index changed during the release checks.")

    entries = run(
        root, "git", "status", "--porcelain=v1", "-z", "--untracked-files=all"
    ).split(b"\0")

    allowed = {b" M " + name.encode() for name in expected}

    if any(entry and entry not in allowed for entry in entries):
        raise ReleaseError("Changes outside the release version files were detected.")

    for name, content in expected.items():
        if (root / name).read_bytes() != content:
            raise ReleaseError(f"{name} contains changes beyond the version bump.")


def publish(root, bump_minor=False):
    root = root.resolve()

    if Path(run(root, "git", "rev-parse", "--show-toplevel").decode().strip()).resolve() != root:
        raise ReleaseError("The script must be stored in the repository's scripts directory.")

    require_clean(root)

    if run(root, "git", "branch", "--show-current").strip() != b"main":
        raise ReleaseError("Release publishing requires the main branch.")

    head = run(root, "git", "rev-parse", "HEAD").strip()
    manifest = (root / "Cargo.toml").read_bytes()
    lock = (root / "Cargo.lock").read_bytes()
    old = tomllib.loads(manifest.decode())["workspace"]["package"]["version"]
    version = bump_version(old, bump_minor)
    tag = f"v{version}"

    if run(root, "git", "tag", "--list", tag).strip():
        raise ReleaseError(f"Tag {tag} already exists locally.")

    if run(root, "git", "ls-remote", "--tags", "origin", f"refs/tags/{tag}").strip():
        raise ReleaseError(f"Tag {tag} already exists on origin.")

    metadata = json.loads(run(
        root, "cargo", "metadata", "--no-deps", "--locked", "--format-version", "1"
    ))

    names = set()

    for package in metadata["packages"]:
        if package["id"] in metadata["workspace_members"]:
            package_manifest = tomllib.loads(Path(package["manifest_path"]).read_text(encoding="utf-8"))

            if package_manifest["package"].get("version") == {"workspace": True}:
                names.add(package["name"])

    expected = {
        "Cargo.toml": expected_manifest(manifest, old, version),
        "Cargo.lock": expected_lock(lock, names, old, version),
    }

    require_clean(root)

    print(f"Bumping {old} to {version}", flush=True)

    (root / "Cargo.toml").write_bytes(expected["Cargo.toml"])

    run(root, "cargo", "check", capture=False)

    verify_changes(root, expected, head)

    blobs = {
        name: run(root, "git", "hash-object", f"--path={name}", name).strip()
        for name in expected
    }

    run(root, "git", "add", "--", *expected, capture=False)
    run(root, "git", "commit", "-m", f"chore: bump version `{tag}`", capture=False)

    # Hooks may edit files or the index; validate their output before tagging.
    require_clean(root)

    if run(root, "git", "rev-parse", "HEAD^").strip() != head:
        raise ReleaseError("The release commit has an unexpected parent.")

    changed = run(root, "git", "diff", "--name-only", "HEAD^", "HEAD").decode().splitlines()

    if set(changed) != set(expected):
        raise ReleaseError("The release commit changed unexpected files.")

    for name, blob in blobs.items():
        if run(root, "git", "rev-parse", f"HEAD:{name}").strip() != blob:
            raise ReleaseError(f"The release commit contains unexpected changes in {name}.")
    run(root, "git", "tag", tag, "HEAD", capture=False)

    run(
        root, "git", "push", "--atomic", "origin",
        "refs/heads/main:refs/heads/main", f"refs/tags/{tag}:refs/tags/{tag}",
        capture=False,
    )

    print(f"Published {tag} to origin.", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--bump-minor", action="store_true", help="Increment minor and reset patch to zero."
    )

    args = parser.parse_args()

    try:
        publish(Path(__file__).resolve().parent.parent, args.bump_minor)
    except (ReleaseError, subprocess.CalledProcessError, OSError, ValueError, KeyError) as error:
        print(f"Release stopped: {error}", file=sys.stderr)
        print("Local changes, commits, and tags were preserved for inspection.", file=sys.stderr)

        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
