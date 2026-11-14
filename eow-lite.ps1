$ErrorActionPreference = 'Stop'

if ($args.Count -ne 1 -or $args[0] -cnotin @('link', 'unlink')) {
    [Console]::Error.WriteLine('Usage: eow-lite link | eow-lite unlink')
    exit 2
}

$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
if (-not [IO.Path]::IsPathRooted($target)) {
    $target = Join-Path $PSScriptRoot $target
}

$supervisor = foreach ($profile in 'release', 'debug') {
    $candidate = Join-Path $target "$profile/eow-lite-supervisor.exe"
    if (Test-Path -LiteralPath $candidate -PathType Leaf) {
        $candidate
        break
    }
}
if (-not $supervisor) {
    [Console]::Error.WriteLine('eow-lite-supervisor.exe was not found. Run cargo build --workspace --bins.')
    exit 1
}

$config = Join-Path $PSScriptRoot 'glimpse_of_the_white/lite/eow_lite'
& $supervisor $args[0] $config
exit $LASTEXITCODE
