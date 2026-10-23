#!/usr/bin/env python3
"""Write winget manifests (schema 1.6.0) for a release.

  tools/winget_manifest.py <version> <SHA256SUMS> <out-dir>

Reads the release's SHA256SUMS (as written by `cargo xtask checksums`) for the portable CLI
archive, and writes the three YAML files winget-pkgs expects under
manifests/y/Yusuf6411/OpenConverter/<version>/. Submit them with `wingetcreate submit` or a
pull request to microsoft/winget-pkgs; this script does not touch the network.
"""
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__)
        return 2
    version, sums, out = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3])
    name = f"openconverter-{version}-x86_64-pc-windows-msvc.zip"
    digest = None
    for line in sums.read_text().splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].lstrip("*") == name:
            digest = parts[0].upper()
    if digest is None:
        print(f"{name} is not listed in {sums}", file=sys.stderr)
        return 1
    pkg = "Yusuf6411.OpenConverter"
    d = out / "manifests" / "y" / "Yusuf6411" / "OpenConverter" / version
    d.mkdir(parents=True, exist_ok=True)
    url = f"https://github.com/Yusuf6411/OpenConverter/releases/download/v{version}/{name}"
    (d / f"{pkg}.yaml").write_text(
        f"""# yaml-language-server: $schema=https://aka.ms/winget-manifest.version.1.6.0.schema.json
PackageIdentifier: {pkg}
PackageVersion: {version}
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.6.0
"""
    )
    (d / f"{pkg}.installer.yaml").write_text(
        f"""# yaml-language-server: $schema=https://aka.ms/winget-manifest.installer.1.6.0.schema.json
PackageIdentifier: {pkg}
PackageVersion: {version}
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
  - RelativeFilePath: openconverter.exe
    PortableCommandAlias: openconverter
Installers:
  - Architecture: x64
    InstallerUrl: {url}
    InstallerSha256: {digest}
ManifestType: installer
ManifestVersion: 1.6.0
"""
    )
    (d / f"{pkg}.locale.en-US.yaml").write_text(
        f"""# yaml-language-server: $schema=https://aka.ms/winget-manifest.defaultLocale.1.6.0.schema.json
PackageIdentifier: {pkg}
PackageVersion: {version}
PackageLocale: en-US
Publisher: OpenConverter contributors
PublisherUrl: https://github.com/Yusuf6411/OpenConverter
PackageName: OpenConverter
PackageUrl: https://github.com/Yusuf6411/OpenConverter
License: MIT
LicenseUrl: https://github.com/Yusuf6411/OpenConverter/blob/main/LICENSE
ShortDescription: Convert files on your own computer, with no uploads.
Tags:
  - converter
  - image
  - audio
  - video
  - document
ManifestType: defaultLocale
ManifestVersion: 1.6.0
"""
    )
    print(f"winget manifests written to {d}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
