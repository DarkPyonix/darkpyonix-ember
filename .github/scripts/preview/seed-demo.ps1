# Waits for the server, then creates the demo project and one scripted session (idempotent project).
param([string]$Url, [string]$Dir)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Dir | Out-Null
$ok = $false
for ($i = 0; $i -lt 40 -and -not $ok; $i++) {
  try { Invoke-RestMethod "$Url/api/projects" | Out-Null; $ok = $true } catch { Start-Sleep -Milliseconds 500 }
}
if (-not $ok) { Write-Host 'ember-server did not start in time'; exit 1 }
$h = @{ 'Content-Type' = 'application/json' }
Invoke-RestMethod "$Url/api/projects" -Method Post -Headers $h -Body (@{ name = 'demo-project' } | ConvertTo-Json) | Out-Null
$existing = Invoke-RestMethod "$Url/api/sessions"
if (@($existing).Count -eq 0) {
  $body = @{ project = 'demo-project'; agent = 'scripted'; cwd = $Dir; title = 'Demo' } | ConvertTo-Json
  Invoke-RestMethod "$Url/api/sessions" -Method Post -Headers $h -Body $body | Out-Null
}
