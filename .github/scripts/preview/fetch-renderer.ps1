# Downloads the compose-rust renderer for Windows and verifies its checksum.
#
# The renderer must be generated from the same schema as the compose-rust crate Ember pins, or
# compose-rust's build script refuses it. With -CargoLock the script reads the pinned
# compose-rust rev from that lock file, reads the schema hash at that rev
# (compose-rust/schema-hash.txt), and takes compose-rust's renderer-dev asset for that hash
# (`compose-rust-renderer-schema-<hash>-windows-x64.tar.gz`, published per schema). A pin past
# the last release has no release renderer (em-train-24), so this does not wait for one. When
# renderer-dev has no asset for the hash, the release -Version is used, as before.
param(
  [Parameter(Mandatory)][string]$Version,
  [Parameter(Mandatory)][string]$Out,
  [string]$CargoLock = ''
)
$ErrorActionPreference = 'Stop'
$repo = 'https://github.com/DarkPyonix/compose-rust'
$tmp = Join-Path $env:RUNNER_TEMP 'renderer-dl'
New-Item -ItemType Directory -Force $tmp, $Out | Out-Null

function Get-Asset([string]$base, [string]$name) {
  Invoke-WebRequest "$base/$name" -OutFile "$tmp\$name"
  Invoke-WebRequest "$base/$name.sha256" -OutFile "$tmp\$name.sha256"
  $want = ((Get-Content "$tmp\$name.sha256" -Raw).Trim() -split '\s+')[0].ToLower()
  $got = (Get-FileHash "$tmp\$name" -Algorithm SHA256).Hash.ToLower()
  if ($want -ne $got) { throw "checksum mismatch for ${name}: want $want, got $got" }
  return "$tmp\$name"
}

$archive = $null
$schema = $null
if ($CargoLock) {
  $lock = Get-Content $CargoLock -Raw
  $m = [regex]::Match($lock, 'name = "compose-rust"\s*\r?\nversion = "[^"]*"\s*\r?\nsource = "git\+[^"#]*#([0-9a-f]{40})"')
  if (-not $m.Success) { throw "no compose-rust git source in $CargoLock" }
  $rev = $m.Groups[1].Value
  $hashText = (Invoke-WebRequest "https://raw.githubusercontent.com/DarkPyonix/compose-rust/$rev/compose-rust/schema-hash.txt").Content
  $schema = ([string]$hashText).Trim().ToLower() -replace '^0x', ''
  if ($schema -notmatch '^[0-9a-f]{16}$') { throw "unexpected schema hash at compose-rust ${rev}: $hashText" }
  Write-Host "compose-rust $rev has schema $schema"
  try {
    $archive = Get-Asset "$repo/releases/download/renderer-dev" "compose-rust-renderer-schema-$schema-windows-x64.tar.gz"
    Write-Host "renderer: renderer-dev, schema $schema"
  } catch {
    if ($_.Exception.Message -like 'checksum mismatch*') { throw }
    Write-Host "renderer-dev has no Windows renderer for schema $schema; trying release $Version"
  }
}
if (-not $archive) {
  $archive = Get-Asset "$repo/releases/download/$Version" "compose-rust-renderer-$Version-windows-x64.tar.gz"
  Write-Host "renderer: release $Version"
}
tar -xzf $archive -C $Out
if ($LASTEXITCODE -ne 0) { throw 'tar failed' }
if ($schema) {
  $have = ((Get-Content (Join-Path $Out 'schema-hash.txt') -Raw).Trim().ToLower()) -replace '^0x', ''
  if ($have -ne $schema) { throw "the renderer is for schema $have, the pinned crate is $schema" }
}
Get-ChildItem $Out -Recurse | Select-Object -First 60 FullName
