#!/usr/bin/env python3
"""Sanity-check generated .nupkg files before they are pushed to nuget.org.

nuget.org rejects a package that declares an SPDX license expression without
the matching ``<licenseUrl>`` element
(https://aka.ms/invalidNuGetLicenseUrl). The packer normally emits it, but this
check turns a server-side 400 into a fast, local failure.

Usage:
    check_nupkg.py nupkg/*.nupkg
"""

from __future__ import annotations

import sys
import zipfile


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def main(paths: list[str]) -> int:
    if not paths:
        fail("no packages given")

    for path in paths:
        try:
            with zipfile.ZipFile(path) as package:
                names = [name for name in package.namelist() if name.endswith(".nuspec")]
                if len(names) != 1:
                    fail(f"{path}: expected exactly one .nuspec, found {len(names)}")
                text = package.read(names[0]).decode("utf-8")
        except (OSError, zipfile.BadZipFile, UnicodeDecodeError) as error:
            fail(f"{path}: {error}")

        if '<license type="expression">' in text:
            expression = text.split('<license type="expression">', 1)[1].split("<", 1)[0]
            expected = "https://licenses.nuget.org/" + expression.replace(" ", "%20")
            if f"<licenseUrl>{expected}</licenseUrl>" not in text:
                fail(
                    f"{path}: license expression {expression!r} needs "
                    f"<licenseUrl>{expected}</licenseUrl>"
                )

        if "<id>" not in text or "<version>" not in text:
            fail(f"{path}: nuspec lacks an id or version")

        print(f"ok {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
