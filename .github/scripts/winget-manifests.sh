#!/bin/sh
# Write the winget manifests for a release into DIR/IrixFans.Origami: the Windows installers,
# which also put the origami command on PATH.
# usage: winget-manifests.sh VERSION SHA256SUMS DIR
set -eu
version=$1 sums=$2 out=$3
repo=https://github.com/jamesbraid/origami
schema=1.10.0
id=IrixFans.Origami
# The installer's AppId plus Inno's "_is1": what it writes to the uninstall key.
product='{1EC46053-31E6-486A-8AE3-C2DE12CE825F}_is1'

sum() {
  hash=$(awk -v f="$1" '$2 == f {print toupper($1)}' "$sums")
  case $hash in
    *[!0-9A-F]* | '') hash= ;;
  esac
  if [ "${#hash}" != 64 ]; then
    echo "$sums has no valid checksum for $1" >&2
    exit 1
  fi
  echo "$hash"
}

dir=$out/$id
mkdir -p "$dir"
{
  echo "# yaml-language-server: \$schema=https://aka.ms/winget-manifest.installer.$schema.schema.json"
  echo "PackageIdentifier: $id"
  echo "PackageVersion: $version"
  echo "InstallerType: inno"
  echo "Scope: user"
  echo "InstallModes:"
  echo "- interactive"
  echo "- silent"
  echo "- silentWithProgress"
  echo "UpgradeBehavior: install"
  echo "MinimumOSVersion: 10.0.17763.0"
  echo "Commands:"
  echo "- origami"
  echo "Installers:"
  for arch in x64:x86_64 arm64:arm64; do
    a=${arch%%:*} f=origami-$version-windows-${arch#*:}-setup.exe
    echo "- Architecture: $a"
    echo "  InstallerUrl: $repo/releases/download/v$version/$f"
    echo "  InstallerSha256: $(sum "$f")"
    echo "  AppsAndFeaturesEntries:"
    echo "  - DisplayName: Origami"
    echo "    Publisher: James Braid"
    echo "    DisplayVersion: $version"
    echo "    ProductCode: '$product'"
  done
  echo "ManifestType: installer"
  echo "ManifestVersion: $schema"
} > "$dir/$id.installer.yaml"
cat > "$dir/$id.yaml" <<EOT
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.version.$schema.schema.json
PackageIdentifier: $id
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: $schema
EOT
cat > "$dir/$id.locale.en-US.yaml" <<EOT
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.defaultLocale.$schema.schema.json
PackageIdentifier: $id
PackageVersion: $version
PackageLocale: en-US
Publisher: James Braid
PublisherUrl: https://github.com/jamesbraid
PackageName: Origami
PackageUrl: https://origami.irix.fans/
License: BSD-3-Clause AND GPL-2.0-only
LicenseUrl: $repo/blob/v$version/LICENSE
ShortDescription: Emulator for SGI MIPS workstations and servers, built on QEMU
Tags:
- emulator
- qemu
- sgi
- irix
- mips
ReleaseNotesUrl: $repo/releases/tag/v$version
ManifestType: defaultLocale
ManifestVersion: $schema
EOT
