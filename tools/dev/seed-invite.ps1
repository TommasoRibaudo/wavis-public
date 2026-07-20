<#
Seeds (or updates) a multi-use alpha invite directly in the local dev
postgres container, for use as ALPHA_INVITE_CODE in e2e specs / manual
registration testing. Mirrors wavis-backend/src/auth/alpha_invite.rs's
normalize_invite_code + hash_invite_code (HMAC-SHA256(pepper,
"alpha-invite:v1:" + normalized_code)).

Usage:
  powershell -File tools/dev/seed-invite.ps1 -Code E2EALPHA174
  powershell -File tools/dev/seed-invite.ps1 -Code E2EALPHA174 -MaxRedemptions 5
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Code,
    [int]$MaxRedemptions = 100000,
    # Must match ALPHA_INVITE_CODE_PEPPER on the running backend (docker-compose.yml default below).
    [string]$Pepper = 'dev-alpha-invite-pepper-32b!!XXX',
    [string]$Container = 'wavis-postgres',
    [string]$DbUser = 'wavis',
    [string]$DbName = 'wavis'
)

$ErrorActionPreference = 'Stop'

# normalize_invite_code: strip ASCII whitespace and '-', uppercase.
$normalized = ($Code.Trim() -replace '[\s-]', '').ToUpperInvariant()
if ([string]::IsNullOrEmpty($normalized)) { throw "Code '$Code' normalizes to empty string" }

$nodeScript = "const c=require('crypto');process.stdout.write(c.createHmac('sha256',process.argv[1]).update('alpha-invite:v1:'+process.argv[2]).digest('hex'))"
$hex = & node -e $nodeScript -- $Pepper $normalized
if (-not $hex) { throw 'Failed to compute HMAC hash via node' }

$sql = "INSERT INTO alpha_invites (code_hash, max_redemptions) VALUES (decode('$hex','hex'), $MaxRedemptions) ON CONFLICT (code_hash) DO UPDATE SET max_redemptions=$MaxRedemptions, redemption_count=0, disabled_at=NULL, expires_at=NULL;"

docker exec $Container psql -U $DbUser -d $DbName -c $sql
if ($LASTEXITCODE -ne 0) { throw "psql insert failed with exit code $LASTEXITCODE" }

Write-Output "Seeded invite '$normalized' (max_redemptions=$MaxRedemptions). Use ALPHA_INVITE_CODE=$normalized"
