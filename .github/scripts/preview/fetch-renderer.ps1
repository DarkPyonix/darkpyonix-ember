# Downloads the compose-rust renderer for Windows and verifies its checksum.
param([Parameter(Mandatory)][string]$Version, [Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = 'Stop'
$name = "compose-rust-renderer-$Version-windows-x64.tar.gz"
$base = "https://github.com/DarkPyonix/compose-rust/releases/download/$Version"
$tmp = Join-Path $env:RUNNER_TEMP 'renderer-dl'
New-Item -ItemType Directory -Force $tmp, $Out | Out-Null
Invoke-WebRequest "$base/$name" -OutFile "$tmp\$name"
Invoke-WebRequest "$base/$name.sha256" -OutFile "$tmp\$name.sha256"
$want = ((Get-Content "$tmp\$name.sha256" -Raw).Trim() -split '\s+')[0].ToLower()
$got = (Get-FileHash "$tmp\$name" -Algorithm SHA256).Hash.ToLower()
if ($want -ne $got) { throw "checksum mismatch: want $want, got $got" }
tar -xzf "$tmp\$name" -C $Out
if ($LASTEXITCODE -ne 0) { throw 'tar failed' }
Get-ChildItem $Out -Recurse | Select-Object -First 60 FullName
