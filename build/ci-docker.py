#!/usr/bin/env python3
"""Translate job-container bind paths for the shared Docker daemon."""

from __future__ import annotations

import json
import posixpath
import socket
import subprocess
import sys
from collections.abc import Sequence
from typing import Any


def workspace_mounts() -> list[dict[str, Any]]:
    result = subprocess.run(
        ["docker", "inspect", socket.gethostname(), "--format", "{{json .Mounts}}"],
        check=True,
        capture_output=True,
        text=True,
    )
    mounts = json.loads(result.stdout)
    if not isinstance(mounts, list):
        raise ValueError("Docker returned an invalid job mount list")
    return mounts


def translate_volume(spec: str, mounts: Sequence[dict[str, Any]]) -> str:
    source, separator, target_and_options = spec.partition(":")
    if not separator or not target_and_options:
        raise ValueError(f"unsupported Docker volume specification: {spec!r}")
    if not source.startswith("/"):
        return spec

    source = posixpath.normpath(source)
    matches = []
    for mount in mounts:
        destination = mount.get("Destination")
        host_source = mount.get("Source")
        if (
            mount.get("Type") != "volume"
            or not mount.get("Name")
            or not isinstance(destination, str)
            or not isinstance(host_source, str)
        ):
            continue
        destination = posixpath.normpath(destination)
        if source == destination or source.startswith(destination.rstrip("/") + "/"):
            matches.append((len(destination), destination, host_source))

    if not matches:
        raise ValueError(
            f"Docker bind source is outside a named job workspace mount: {source}"
        )
    _, destination, host_source = max(matches)
    relative = posixpath.relpath(source, destination)
    translated = host_source if relative == "." else posixpath.join(host_source, relative)
    return f"{translated}:{target_and_options}"


def translate_run_args(args: Sequence[str], mounts: Sequence[dict[str, Any]]) -> list[str]:
    translated: list[str] = []
    index = 0
    while index < len(args):
        arg = args[index]
        if arg in ("-v", "--volume"):
            if index + 1 == len(args):
                raise ValueError(f"{arg} requires a volume specification")
            translated.extend((arg, translate_volume(args[index + 1], mounts)))
            index += 2
            continue
        if arg.startswith("--volume="):
            translated.append(
                "--volume=" + translate_volume(arg[len("--volume="):], mounts)
            )
        else:
            translated.append(arg)
        index += 1
    return translated


def docker_command(args: Sequence[str]) -> list[str]:
    if not args:
        raise ValueError("expected docker build or run")
    if args[0] == "build":
        return ["docker", *args]
    if args[0] != "run":
        raise ValueError(f"unsupported Docker command: {args[0]}")
    if any(arg in ("-v", "--volume") or arg.startswith("--volume=") for arg in args):
        mounts = workspace_mounts()
        args = translate_run_args(args, mounts)
    return ["docker", *args]


def main(argv: Sequence[str]) -> int:
    try:
        command = docker_command(argv)
    except (ValueError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        print(f"ci-docker: {error}", file=sys.stderr)
        return 2
    result = subprocess.run(command, check=False).returncode
    return 128 - result if result < 0 else result


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
