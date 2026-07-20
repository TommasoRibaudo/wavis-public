$ErrorActionPreference = 'Stop'
$stdin = [Console]::In.ReadToEnd()
try { $input_json = $stdin | ConvertFrom-Json } catch { exit 0 }

$filePath = $input_json.tool_input.file_path
if (-not $filePath) { exit 0 }

$normalized = $filePath -replace '\\', '/'
if ($normalized -notmatch 'clients/wavis-gui/.*\.(ts|tsx)$') { exit 0 }
if ($normalized -match '/node_modules/') { exit 0 }

# Resolve to a path relative to clients/wavis-gui so eslint's flat config resolves correctly
$repoRoot = $input_json.cwd
if (-not $repoRoot) { $repoRoot = (Get-Location).Path }
$guiRoot = Join-Path $repoRoot 'clients\wavis-gui'

if ([System.IO.Path]::IsPathRooted($filePath)) {
    $absPath = $filePath
} else {
    $absPath = Join-Path $repoRoot $filePath
}

if (-not (Test-Path $guiRoot)) { exit 0 }

Push-Location $guiRoot
try {
    $guiRootFull = (Resolve-Path $guiRoot).Path.TrimEnd('\', '/')
    $absPathFull = (Resolve-Path $absPath).Path
    $relPath = $absPathFull.Substring($guiRootFull.Length).TrimStart('\', '/')
    $eslintOutput = & npx eslint --quiet -- "$relPath" 2>&1
    $exitCode = $LASTEXITCODE
} finally {
    Pop-Location
}

if ($exitCode -eq 0) { exit 0 }

$errorText = ($eslintOutput | Out-String).Trim()
if ($errorText.Length -gt 4000) { $errorText = $errorText.Substring(0, 4000) + "`n... (truncated)" }

$context = "ESLint errors in $filePath (warnings are review triggers, not gates - only errors are shown):`n$errorText"

$output = @{
    hookSpecificOutput = @{
        hookEventName   = "PostToolUse"
        additionalContext = $context
    }
} | ConvertTo-Json -Depth 5 -Compress

Write-Output $output
[Console]::Error.WriteLine($context)
exit 2
