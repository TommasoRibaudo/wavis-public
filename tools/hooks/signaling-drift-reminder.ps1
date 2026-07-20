$ErrorActionPreference = 'Stop'
$stdin = [Console]::In.ReadToEnd()
try { $input_json = $stdin | ConvertFrom-Json } catch { exit 0 }

$filePath = $input_json.tool_input.file_path
if (-not $filePath) { exit 0 }

$normalized = $filePath -replace '\\', '/'
if ($normalized -notmatch 'shared/src/signaling/') { exit 0 }

$checklist = @"
Signaling schema file changed: $filePath
Before finishing this turn, check for schema drift (per CLAUDE.md):
1. Re-read the edited file: what changed (new/renamed/removed fields, variants, serde attributes)?
2. Search wavis-backend/src/ws/, voice/, auth/, channel/, chat/, abuse/, diagnostics/ - are all match arms complete, no references to removed fields?
3. Search clients/shared/src/ - does the client still correctly construct/destructure the changed types?
4. If a SignalingMessage variant changed: confirm updates in mod.rs, validation.rs, proptest_support.rs, ws.rs, call_session.rs.
Report mismatches with file:line. Do not auto-fix unless trivially obvious.
"@

$output = @{
    hookSpecificOutput = @{
        hookEventName   = "PostToolUse"
        additionalContext = $checklist
    }
} | ConvertTo-Json -Depth 5 -Compress

Write-Output $output
exit 0
