# Stages ember-app, ember-server, the renderer files and the launcher, then zips them.
param([Parameter(Mandatory)][string]$Renderer, [Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$stage = 'stage\ember-preview'
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item ember\app\target\release\ember-app.exe $stage
Copy-Item ember\server\target\release\ember-server.exe $stage
# The renderer ships its executable and a lib directory in bin/; they sit beside ember-app.exe.
$bin = Get-ChildItem $Renderer -Recurse -Directory -Filter bin | Select-Object -First 1
if ($bin) { Copy-Item "$($bin.FullName)\*" $stage -Recurse -Force }
else { Copy-Item "$Renderer\*" $stage -Recurse -Force }
Copy-Item "$here\run-preview.cmd" $stage
Copy-Item "$here\seed-demo.ps1" $stage
Copy-Item "$here\README.txt" $stage
New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
if (Test-Path $Out) { Remove-Item $Out }
Compress-Archive -Path "$stage" -DestinationPath $Out
Get-ChildItem -Recurse $stage | Select-Object FullName, Length
