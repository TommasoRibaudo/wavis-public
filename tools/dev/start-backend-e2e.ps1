<#
Boots the local docker-compose stack (postgres, redis, livekit, wavis-backend)
built with the test-no-rate-limits cargo feature, mirroring the recipe in
clients/wavis-gui/e2e-tooling/README.md's "Rate limiting" section and
security.md's "Test-Only Rate Limit Bypass" section.

The feature bypasses abuse-layer limiters (join/temp-ban/global/per-IP/WS/
chat) but NOT the auth-layer AuthRateLimiterConfig/RecoveryRateLimiterConfig,
which still need their env overrides raised separately for a full suite run.

Usage:
  powershell -File tools/dev/start-backend-e2e.ps1
  powershell -File tools/dev/start-backend-e2e.ps1 -Restore   # rebuild back to normal (no feature, default rate limits)
#>
[CmdletBinding()]
param(
    [int]$AuthRegisterRateLimit = 200,
    [int]$AuthRecoverRateLimit = 200,
    [switch]$Restore,
    [int]$HealthTimeoutSecs = 60
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Push-Location $repoRoot
try {
    if ($Restore) {
        Write-Output 'Rebuilding wavis-backend WITHOUT test-no-rate-limits (restoring normal enforcement)...'
        docker compose up -d --build
    } else {
        Write-Output "Rebuilding e2e stack with test-no-rate-limits (AUTH_REGISTER_RATE_LIMIT=$AuthRegisterRateLimit, AUTH_RECOVER_RATE_LIMIT=$AuthRecoverRateLimit)..."
        $env:AUTH_REGISTER_RATE_LIMIT = $AuthRegisterRateLimit
        $env:AUTH_RECOVER_RATE_LIMIT = $AuthRecoverRateLimit
        $env:CARGO_FEATURES = 'test-no-rate-limits'
        docker compose up -d --build
    }
    if ($LASTEXITCODE -ne 0) { throw "docker compose up failed with exit code $LASTEXITCODE" }

    Write-Output 'Waiting for backend /health...'
    $deadline = (Get-Date).AddSeconds($HealthTimeoutSecs)
    $healthy = $false
    while ((Get-Date) -lt $deadline) {
        try {
            $resp = Invoke-WebRequest -Uri 'http://localhost:3000/health' -UseBasicParsing -TimeoutSec 3
            if ($resp.StatusCode -eq 200) { $healthy = $true; break }
        } catch {
            Start-Sleep -Seconds 2
        }
    }

    if (-not $healthy) { throw "Backend did not become healthy within $HealthTimeoutSecs s" }
    Write-Output 'Backend is healthy at http://localhost:3000'
    if (-not $Restore) {
        Write-Output 'Remember to run with -Restore when done, so the shared dev backend is not left rate-limit-free.'
    }
} finally {
    Pop-Location
}
