# Distribution channels

Publishing needs accounts and keys that are not in the build environment. Everything that can be
prepared offline is here; the table says what was actually run.

| Channel | Files | Tested here |
|---|---|---|
| APT repository, signed | `tools/make_apt_repo.py` | Yes: built a repo, verified with `gpgv` and `apt` |
| winget | `tools/winget_manifest.py` | Generator run against fake checksums; not submitted |
| Flathub | `packaging/flatpak/` | No: `flatpak-builder` is missing; sha256 and `cargo-sources.json` are placeholders |
| COPR / RPM | `packaging/rpm/openconverter.spec`, `tools/vendor_source.sh`, `tools/sign_rpm_repo.sh` | No: `rpmbuild` and `createrepo_c` are missing |
| macOS signing and notarization | `tools/macos_sign_notarize.sh` | No: needs macOS and an Apple Developer account |
| OSS-Fuzz | `packaging/oss-fuzz/` | No: needs the OSS-Fuzz image; fuzz targets themselves build in `fuzz/` |
| Windows 11 top-level menu | `packaging/windows/README.md` | Not done |

Credentials are read from the environment, never from the repository.
