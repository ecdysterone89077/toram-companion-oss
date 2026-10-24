#requires -Version 7.0
param(
    [ValidatePattern('^[0-9][A-Za-z0-9.+-]{0,63}$')]
    [string]$Version = "0.96.4-rc.8",
    [string]$Status = "candidate_gameplay_pending",
    [string]$Root = (Split-Path -Parent $MyInvocation.MyCommand.Path),
    [string]$ArtifactRoot
)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path -LiteralPath $Root).Path
if (-not $ArtifactRoot) { $ArtifactRoot = $Root }
$ArtifactRoot = (Resolve-Path -LiteralPath $ArtifactRoot).Path
Set-Location -LiteralPath $Root

$name = "Toram-Companion-v$Version"
$stageDir = Join-Path $Root "dist/$name-win64"
$sourceStage = Join-Path $Root "dist/$name-source"
$winZip = Join-Path $Root "dist/$name-win64.zip"
$srcZip = Join-Path $Root "dist/$name-source.zip"
$sumPath = Join-Path $Root "dist/SHA256SUMS-v$Version.txt"
$receiptPath = Join-Path $Root "dist/RELEASE-v$Version.json"
foreach ($outputPath in @($stageDir, $sourceStage, $winZip, $srcZip, $sumPath, $receiptPath)) {
    if (Test-Path -LiteralPath $outputPath) {
        throw "Release output already exists; preserve it and choose a new version: $outputPath"
    }
}

function Get-Sha256([string]$Path) {
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash
}

function Get-RelPath([string]$Base, [string]$Path) {
    $relative = [System.IO.Path]::GetRelativePath(
        (Resolve-Path -LiteralPath $Base).Path,
        (Resolve-Path -LiteralPath $Path).Path
    ) -replace '\\', '/'
    if ([System.IO.Path]::IsPathRooted($relative) -or $relative -eq '..' -or $relative.StartsWith('../')) {
        throw "Path is outside its declared base: $Path"
    }
    return $relative
}

# --- staged artifacts -------------------------------------------------------
$artifacts = @(
    @{ Path = "companion.exe";    Source = "companion.exe" },
    @{ Path = "src/wcore.dll";  Source = "src/wcore.dll" },
    @{ Path = "src/wboot.exe";  Source = "src/wboot.exe" },
    @{ Path = "observer.exe";   Source = "observer.exe" }
)
foreach ($a in $artifacts) {
    $a.Source = Join-Path $ArtifactRoot $a.Path
    if (-not (Test-Path -LiteralPath $a.Source -PathType Leaf)) { throw "missing staged artifact: $($a.Source)" }
}

# --- source input allowlist -------------------------------------------------
$sourceInputs = @()
$sourceInputs += (Get-ChildItem -Path "crates" -Recurse -File -Include "Cargo.toml","*.rs" |
    Sort-Object FullName | ForEach-Object { Get-RelPath $Root $_.FullName })
$sourceInputs += ".cargo/config.toml"
$sourceInputs += "Cargo.toml"
$sourceInputs += "Cargo.lock"
$sourceInputs += "package-release.ps1"
$sourceInputs += "scripts/package_release_checks.ps1"
$sourceInputs = $sourceInputs | Sort-Object -Unique

$sourceManifest = @()
foreach ($rel in $sourceInputs) {
    if (-not (Test-Path -LiteralPath $rel -PathType Leaf)) { throw "source input missing: $rel" }
    $sourceManifest += [ordered]@{ File = $rel; SHA256 = (Get-Sha256 $rel) }
}

# --- package documentation allowlist ----------------------------------------
$docInputs = @(
    "README.md", "CHANGELOG.md", "LICENSE", "RELEASE_NOTES.md", "VALIDATION.md",
    "MODLOG.md", "SECURITY.md", "CONTRIBUTING.md", "CODE_OF_CONDUCT.md",
    ".gitattributes", ".gitignore"
)
$docInputs = @($docInputs | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf })

$specInputs = @(Get-ChildItem -Path "specs" -File -Filter "*.md" | Sort-Object Name |
    ForEach-Object { Get-RelPath $Root $_.FullName })

# --- stage the win64 directory ----------------------------------------------
New-Item -ItemType Directory -Path $stageDir | Out-Null
New-Item -ItemType Directory -Path (Join-Path $stageDir "src") | Out-Null
New-Item -ItemType Directory -Path (Join-Path $stageDir "specs") | Out-Null

foreach ($a in $artifacts) {
    Copy-Item -LiteralPath $a.Source -Destination (Join-Path $stageDir $a.Path)
}
foreach ($d in $docInputs) {
    Copy-Item -LiteralPath $d -Destination (Join-Path $stageDir $d)
}
foreach ($s in $specInputs) {
    Copy-Item -LiteralPath $s -Destination (Join-Path $stageDir $s)
}

$sourceJson = Join-Path $stageDir "SOURCE_SHA256.json"
$sourceManifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $sourceJson -Encoding UTF8

$binaryManifest = @()
foreach ($a in $artifacts) {
    $staged = Join-Path $stageDir $a.Path
    $binaryManifest += [ordered]@{
        File  = $a.Path
        SHA256 = (Get-Sha256 $staged)
        Bytes = (Get-Item -LiteralPath $staged).Length
    }
}

$rustc = (rustc --version)
$cargo = (cargo --version)
$baseCommit = (git rev-parse HEAD)
$telemetry = (Select-String -Path "crates/protocol/src/lib.rs" -Pattern "TELEMETRY_VERSION: u32 = (\d+)").Matches[0].Groups[1].Value

$buildManifest = [ordered]@{
    schema_version                 = 2
    project                        = "Toram Companion"
    version                        = $Version
    status                         = $Status
    telemetry_layout               = [int]$telemetry
    packaged_at_utc                = (Get-Date).ToUniversalTime().ToString("o")
    target                         = "x86_64-pc-windows-msvc"
    cpu_baseline                   = "x86-64-v2"
    rustc                          = $rustc
    cargo                          = $cargo
    workspace_crates               = @("crates/protocol","crates/wcore","crates/wboot","crates/companion","crates/observer","crates/xtask")
    external_rust_crates           = @()
    git_base_commit                = $baseCommit
    source_state                   = "working-tree snapshot; SOURCE_SHA256.json identifies exact inputs"
    source_binary_correspondence    = "not verified by packager; hashes identify inputs and binaries separately"
    binaries                       = $binaryManifest
    source_input_count             = $sourceManifest.Count
    validation                     = [ordered]@{
        workspace_tests        = "not run by packager; consult the matching verification receipt"
        child_reader_probe     = "not run by packager"
        release_build          = "not run by packager; existing staged binaries copied"
        current_build_live_test = "not run by packager"
        mp_bonus_effectiveness  = "unresolved"
        extended_range_acceptance = "unresolved"
    }
}
$buildManifest | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $stageDir "BUILD_MANIFEST.json") -Encoding UTF8

# --- inner checksum file ----------------------------------------------------
$innerLines = @()
Get-ChildItem -LiteralPath $stageDir -Recurse -File | Sort-Object FullName | ForEach-Object {
    $rel = Get-RelPath $stageDir $_.FullName
    $innerLines += ("{0}  {1}" -f (Get-Sha256 $_.FullName), $rel)
}
$innerLines | Set-Content -LiteralPath (Join-Path $stageDir "SHA256SUMS.txt") -Encoding ASCII

# --- build the source package ------------------------------------------------
New-Item -ItemType Directory -Path $sourceStage | Out-Null
foreach ($rel in ($sourceInputs + $docInputs + $specInputs)) {
    $dest = Join-Path $sourceStage $rel
    $parent = Split-Path -Parent $dest
    if (-not (Test-Path -LiteralPath $parent)) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
    Copy-Item -LiteralPath $rel -Destination $dest
}

# --- archive ------------------------------------------------------------------
Add-Type -AssemblyName System.IO.Compression.FileSystem
[System.IO.Compression.ZipFile]::CreateFromDirectory($stageDir, $winZip)
[System.IO.Compression.ZipFile]::CreateFromDirectory($sourceStage, $srcZip)

# --- read-back verification ----------------------------------------------------
Add-Type -AssemblyName System.IO.Compression.FileSystem
function Test-Archive([string]$Archive, [string]$Against) {
    $zip = [System.IO.Compression.ZipFile]::OpenRead((Resolve-Path -LiteralPath $Archive).Path)
    try {
        $checked = 0
        foreach ($entry in $zip.Entries) {
            if ($entry.FullName.EndsWith("/")) { continue }
            $stream = $entry.Open()
            $sha = [System.Security.Cryptography.SHA256]::Create()
            try {
                $hash = [BitConverter]::ToString($sha.ComputeHash($stream)).Replace("-", "")
            } finally {
                $sha.Dispose()
                $stream.Dispose()
            }
            $onDisk = Join-Path $Against ($entry.FullName -replace '/', '\')
            if (-not (Test-Path -LiteralPath $onDisk)) { throw "archive entry not in staging: $($entry.FullName)" }
            if ((Get-Sha256 $onDisk) -ne $hash) { throw "hash mismatch for $($entry.FullName)" }
            $checked++
        }
        return $checked
    } finally { $zip.Dispose() }
}

$winChecked = Test-Archive $winZip $stageDir
$srcChecked = Test-Archive $srcZip $sourceStage
if ($winChecked -ne $binaryManifest.Count + $docInputs.Count + $specInputs.Count + 3) {
    throw "win64 entry count mismatch: $winChecked"
}
if ($srcChecked -ne ($sourceInputs.Count + $docInputs.Count + $specInputs.Count)) {
    throw "source entry count mismatch: $srcChecked"
}

# --- release receipt -------------------------------------------------------------
$winBytes = (Get-Item -LiteralPath $winZip).Length
$srcBytes = (Get-Item -LiteralPath $srcZip).Length
$winHash = Get-Sha256 $winZip
$srcHash = Get-Sha256 $srcZip

$sums = @(
    "{0}  {1}" -f $winHash, "$name-win64.zip"
    "{0}  {1}" -f $srcHash, "$name-source.zip"
)
$sums | Set-Content -LiteralPath $sumPath -Encoding ASCII

$receipt = [ordered]@{
    version   = $Version
    status    = $Status
    preflight = [ordered]@{
        binary = [ordered]@{ files_checked = $binaryManifest.Count; failures = 0; method = "staged release artifacts, SHA-256 per file" }
        source = [ordered]@{ files_checked = $sourceManifest.Count; failures = 0; method = "allowlisted source inputs, SHA-256 per file" }
        archive_matches_staged_binaries = $true
        source_binary_correspondence = "not verified by packager"
        complete_source_inputs = $sourceManifest.Count
    }
    archives  = @(
        [ordered]@{ file = "$name-win64.zip"; bytes = $winBytes; sha256 = $winHash; entries = $winChecked; roundtrip_verified = $true }
        [ordered]@{ file = "$name-source.zip"; bytes = $srcBytes; sha256 = $srcHash; entries = $srcChecked; roundtrip_verified = $true }
    )
    no_external_publication_performed = $true
}
$receipt | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $receiptPath -Encoding UTF8

Write-Output "PACKAGED v$Version"
Write-Output ("  {0}  {1} entries" -f $winHash, $winChecked)
Write-Output ("  {0}  {1} entries" -f $srcHash, $srcChecked)
Write-Output ("  source inputs: {0}" -f $sourceManifest.Count)
