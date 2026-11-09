#!/usr/bin/env python3
"""Package the native csharp-format binaries as RID-specific .NET tools.

The .NET SDK can publish RID-specific tools, but it expects the tool itself to
be built by MSBuild. This script produces the same package shape by hand, so a
native Rust executable can be installed with `dotnet tool install`.

For every RID we create `<id>.<rid>.<version>.nupkg` (package type
DotnetToolRidPackage) containing

    tools/net10.0/<rid>/DotnetToolSettings.xml
    tools/net10.0/<rid>/<executable>

and one pointer package `<id>.<version>.nupkg` (package type DotnetTool) whose
DotnetToolSettings.xml lists the RID packages. The .NET CLI reads that list and
downloads the package matching the user's platform.

Requires .NET SDK 10 or later to install the resulting tool.

Usage:
    pack_dotnet_tool.py --version 1.2.3 --out dist/nuget \
        --binary win-x64=target/x86_64-pc-windows-msvc/release/csharp-format.exe \
        --binary linux-x64=target/x86_64-unknown-linux-gnu/release/csharp-format
"""

from __future__ import annotations

import argparse
import datetime as _dt
import os
import re
import sys
import uuid
import zipfile

CONTENT_TYPES = """<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml" />
  <Default Extension="psmdcp" ContentType="application/vnd.openxmlformats-package.core-properties+xml" />
  <Default Extension="nuspec" ContentType="application/octet" />
  <Default Extension="xml" ContentType="application/octet" />
</Types>
"""


def relationships(nuspec_name: str, psmdcp_name: str) -> str:
    return f"""<?xml version="1.0" encoding="utf-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Type="http://schemas.microsoft.com/packaging/2010/07/manifest" Target="/{nuspec_name}" Id="R{uuid.uuid4().hex.upper()}" />
  <Relationship Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="/{psmdcp_name}" Id="R{uuid.uuid4().hex.upper()}" />
</Relationships>
"""


def core_properties(package_id: str, version: str, description: str) -> str:
    stamp = _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    return f"""<?xml version="1.0" encoding="utf-8"?>
<coreProperties xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns="http://schemas.openxmlformats.org/package/2006/metadata/core-properties">
  <dc:creator>csharp-format</dc:creator>
  <dc:description>{xml_escape(description)}</dc:description>
  <dc:identifier>{xml_escape(package_id)}</dc:identifier>
  <version>{xml_escape(version)}</version>
  <dcterms:created xsi:type="dcterms:W3CDTF">{stamp}</dcterms:created>
  <dcterms:modified xsi:type="dcterms:W3CDTF">{stamp}</dcterms:modified>
</coreProperties>
"""


def license_url(expression: str) -> str:
    """The licenseUrl nuget.org expects alongside a license expression."""
    return "https://licenses.nuget.org/" + expression.replace(" ", "%20")


def xml_escape(value: str) -> str:
    return (
        value.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
    )


def nuspec(
    *,
    package_type: str,
    package_id: str,
    version: str,
    authors: str,
    description: str,
    license: str | None,
    project_url: str | None,
    repository_url: str | None,
    tags: str | None,
) -> str:
    extra = ""
    if license:
        extra += f"    <license type=\"expression\">{xml_escape(license)}</license>\n"
        # nuget.org rejects a license expression without the matching licenseUrl
        # (https://aka.ms/invalidNuGetLicenseUrl). The SDK writes the expression
        # into the URL with spaces percent-encoded, so do the same.
        extra += f"    <licenseUrl>{license_url(license)}</licenseUrl>\n"
    if project_url:
        extra += f"    <projectUrl>{xml_escape(project_url)}</projectUrl>\n"
    if repository_url:
        extra += (
            f'    <repository type="git" url="{xml_escape(repository_url)}" />\n'
        )
    if tags:
        extra += f"    <tags>{xml_escape(tags)}</tags>\n"
    return f"""<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2012/06/nuspec.xsd">
  <metadata>
    <id>{xml_escape(package_id)}</id>
    <version>{xml_escape(version)}</version>
    <authors>{xml_escape(authors)}</authors>
    <description>{xml_escape(description)}</description>
{extra}    <packageTypes>
      <packageType name="{package_type}" />
    </packageTypes>
  </metadata>
</package>
"""


def tool_settings(commands: str, rids: list[tuple[str, str]]) -> str:
    rid_block = ""
    if rids:
        entries = "".join(
            f'    <RuntimeIdentifierPackage RuntimeIdentifier="{xml_escape(rid)}" Id="{xml_escape(pkg)}" />\n'
            for rid, pkg in rids
        )
        rid_block = f"  <RuntimeIdentifierPackages>\n{entries}  </RuntimeIdentifierPackages>\n"
    return f"""<?xml version="1.0" encoding="utf-8"?>
<DotNetCliTool Version="2">
  <Commands>
{commands}  </Commands>
{rid_block}</DotNetCliTool>
"""


def write_package(
    path: str,
    nuspec_name: str,
    nuspec_text: str,
    files: list[tuple[str, bytes, int]],
    *,
    package_id: str,
    version: str,
    description: str,
) -> None:
    """Write an OPC/nupkg zip with NuGet's expected boilerplate.

    ``files`` entries are (archive path, contents, unix mode).
    """
    psmdcp_name = f"package/services/metadata/core-properties/{uuid.uuid4().hex}.psmdcp"
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
        entries = [
            ("[Content_Types].xml", CONTENT_TYPES.encode(), 0o644),
            ("_rels/.rels", relationships(nuspec_name, psmdcp_name).encode(), 0o644),
            (nuspec_name, nuspec_text.encode(), 0o644),
            (psmdcp_name, core_properties(package_id, version, description).encode(), 0o644),
        ]
        for name, data, mode in entries + files:
            info = zipfile.ZipInfo(name)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (0o100000 | mode) << 16
            archive.writestr(info, data)


def executable_name(rid: str) -> str:
    return f"{args.tool_command}{'.exe' if rid.startswith('win-') else ''}"


def main(argv: list[str]) -> int:
    os.makedirs(args.out, exist_ok=True)
    packages: list[tuple[str, str]] = []
    written: list[str] = []

    seen: set[str] = set()
    for spec in args.binary:
        rid, sep, source = spec.partition("=")
        if not sep or not rid or not source:
            fail(f"--binary expects RID=PATH, got {spec!r}")
        if rid in seen:
            fail(f"duplicate RID {rid!r}")
        seen.add(rid)
        if not os.path.isfile(source):
            fail(f"binary for {rid} not found: {source}")

        binary_id = f"{args.package_id}.{rid}"
        packages.append((rid, binary_id))
        entry = executable_name(rid)
        settings = tool_settings(
            f'    <Command Name="{xml_escape(args.tool_command)}" EntryPoint="{xml_escape(entry)}" Runner="executable" />\n',
            [],
        )
        with open(source, "rb") as handle:
            payload = handle.read()
        nuspec_text = nuspec(
            package_type="DotnetToolRidPackage",
            package_id=binary_id,
            version=args.version,
            **common_metadata(),
        )
        target = os.path.join(args.out, f"{binary_id}.{args.version}.nupkg")
        write_package(
            target,
            f"{binary_id}.nuspec",
            nuspec_text,
            [
                (f"tools/net10.0/{rid}/DotnetToolSettings.xml", settings.encode(), 0o644),
                (f"tools/net10.0/{rid}/{entry}", payload, 0o755),
            ],
            package_id=binary_id,
            version=args.version,
            description=args.description,
        )
        written.append(target)

    commands = f'    <Command Name="{xml_escape(args.tool_command)}" />\n'
    pointer_settings = tool_settings(commands, packages)
    pointer_nuspec = nuspec(
        package_type="DotnetTool",
        package_id=args.package_id,
        version=args.version,
        **common_metadata(),
    )
    pointer = os.path.join(args.out, f"{args.package_id}.{args.version}.nupkg")
    write_package(
        pointer,
        f"{args.package_id}.nuspec",
        pointer_nuspec,
        [("tools/any/any/DotnetToolSettings.xml", pointer_settings.encode(), 0o644)],
        package_id=args.package_id,
        version=args.version,
        description=args.description,
    )
    written.append(pointer)

    for path in written:
        print(f"packed {path}")
    return 0


def common_metadata() -> dict[str, str | None]:
    return {
        "authors": args.authors,
        "description": args.description,
        "license": args.license,
        "project_url": args.project_url,
        "repository_url": args.repository_url,
        "tags": args.tags,
    }


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--version", required=True, help="package version (semver)")
    parser.add_argument("--out", required=True, help="output directory for .nupkg files")
    parser.add_argument("--package-id", default="csharp-format")
    parser.add_argument("--tool-command", default="csharp-format")
    parser.add_argument("--authors", default="csharp-format contributors")
    parser.add_argument(
        "--description",
        default="Lossless, whitespace-only C# formatter with ReSharper Reformat Code parity.",
    )
    parser.add_argument("--license", default="MIT")
    parser.add_argument("--project-url", default=None)
    parser.add_argument("--repository-url", default=None)
    parser.add_argument("--tags", default="dotnet tool formatter csharp resharper")
    parser.add_argument(
        "--binary",
        action="append",
        required=True,
        metavar="RID=PATH",
        help="native binary for a RID (repeatable), e.g. win-x64=target/release/csharp-format.exe",
    )
    parsed = parser.parse_args()
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+([-.+][0-9A-Za-z.+-]+)?", parsed.version):
        parser.error(f"invalid version {parsed.version!r}")
    return parsed


if __name__ == "__main__":
    args = parse_args()
    raise SystemExit(main(sys.argv[1:]))
