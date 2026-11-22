#!/usr/bin/env python3
"""Packs the probe extension into a .vsix for `--install-extension` (SPEC FR-E12, #32).

    python3 -I ember/ose/regression/probe/pack.py <out.vsix>

A .vsix is a zip with the extension under `extension/`, an `extension.vsixmanifest` and a
`[Content_Types].xml`, as `vsce package` writes it. No Node tooling is needed.
"""
import json, os, sys, zipfile

here = os.path.dirname(os.path.abspath(__file__))


def main(out):
    with open(os.path.join(here, "package.json")) as f:
        pkg = json.load(f)
    manifest = f"""<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011">
  <Metadata>
    <Identity Language="en-US" Id="{pkg['name']}" Version="{pkg['version']}" Publisher="{pkg['publisher']}"/>
    <DisplayName>{pkg['displayName']}</DisplayName>
    <Description xml:space="preserve">{pkg['description']}</Description>
  </Metadata>
  <Installation><InstallationTarget Id="Microsoft.VisualStudio.Code"/></Installation>
  <Dependencies/>
  <Assets><Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true"/></Assets>
</PackageManifest>
"""
    types = """<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension=".json" ContentType="application/json"/>
  <Default Extension=".js" ContentType="application/javascript"/>
  <Default Extension=".vsixmanifest" ContentType="text/xml"/>
</Types>
"""
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", types)
        z.writestr("extension.vsixmanifest", manifest)
        for name in ("package.json", "extension.js"):
            z.write(os.path.join(here, name), f"extension/{name}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
