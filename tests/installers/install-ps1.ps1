$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$env:XFER_INSTALLER_SOURCE_ONLY = "1"
. (Join-Path $root "scripts\install.ps1")

function Assert-Equal {
    param($Expected, $Actual, [string]$Description)
    if ($Expected -ne $Actual) {
        throw "FAIL: ${Description}: expected '$Expected', got '$Actual'"
    }
}

Assert-Equal "xfer-windows-x86_64.exe" (Get-XferArtifact "AMD64") "Windows x86-64"
Assert-Equal "xfer-windows-aarch64.exe" (Get-XferArtifact "ARM64") "Windows ARM64"
Assert-Equal "v4.0.0" (Get-XferNormalizedVersion "4.0.0") "version normalization"
Assert-Equal "v4.0.0" (Get-XferNormalizedVersion "v4.0.0") "tag preservation"

$unsupported = $false
try {
    Get-XferArtifact "x86" | Out-Null
}
catch {
    $unsupported = $true
}
if (-not $unsupported) {
    throw "FAIL: unsupported Windows architecture was accepted"
}

$insecureUrlRejected = $false
try {
    Copy-XferDownload "http://example.invalid/xfer" (
        Join-Path ([IO.Path]::GetTempPath()) "xfer-should-not-exist"
    )
}
catch {
    $insecureUrlRejected = $true
}
if (-not $insecureUrlRejected) {
    throw "FAIL: insecure network URL was accepted"
}

$temporary = Join-Path ([IO.Path]::GetTempPath()) "xfer-installer-test-$([Guid]::NewGuid())"
$fixture = Join-Path $temporary "release"
$download = Join-Path $fixture "latest\download"
$installDir = Join-Path $temporary "install dir"
New-Item -ItemType Directory -Path $download -Force | Out-Null
try {
    $fixtureUri = ([Uri]::new((Resolve-Path $fixture).Path)).AbsoluteUri.TrimEnd("/")
    $artifact = Get-XferArtifact (Get-XferArchitecture)
    $fakeBinary = Join-Path $download $artifact
    Copy-Item -LiteralPath (Join-Path $root "target\debug\xfer.exe") -Destination $fakeBinary
    $checksum = (Get-FileHash -LiteralPath $fakeBinary -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath "$fakeBinary.sha256" -Value "$checksum  $artifact" -Encoding ASCII

    Install-Xfer `
        -RequestedVersion "latest" `
        -DestinationDirectory $installDir `
        -RequestedRepository "cdenihan/XFER" `
        -RequestedReleaseBaseUrl $fixtureUri `
        -SkipPathUpdate

    $installed = Join-Path $installDir "xfer.exe"
    if (-not (Test-Path -LiteralPath $installed)) {
        throw "FAIL: installer did not create xfer.exe"
    }
    & $installed --version | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "FAIL: installed xfer.exe did not run"
    }

    $before = (Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash
    Set-Content -LiteralPath "$fakeBinary.sha256" -Value "$("0" * 64)  $artifact" -Encoding ASCII
    $rejected = $false
    try {
        Install-Xfer `
            -RequestedVersion "latest" `
            -DestinationDirectory $installDir `
            -RequestedRepository "cdenihan/XFER" `
            -RequestedReleaseBaseUrl $fixtureUri `
            -SkipPathUpdate
    }
    catch {
        $rejected = $true
    }
    if (-not $rejected) {
        throw "FAIL: checksum mismatch was accepted"
    }
    $after = (Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash
    Assert-Equal $before $after "failed install preserves existing binary"

    $mismatchDownload = Join-Path $fixture "download\v9.9.9"
    New-Item -ItemType Directory -Path $mismatchDownload -Force | Out-Null
    $mismatchBinary = Join-Path $mismatchDownload $artifact
    Copy-Item -LiteralPath $fakeBinary -Destination $mismatchBinary
    $mismatchChecksum = (
        Get-FileHash -LiteralPath $mismatchBinary -Algorithm SHA256
    ).Hash.ToLowerInvariant()
    Set-Content `
        -LiteralPath "$mismatchBinary.sha256" `
        -Value "$mismatchChecksum  $artifact" `
        -Encoding ASCII
    $versionRejected = $false
    try {
        Install-Xfer `
            -RequestedVersion "v9.9.9" `
            -DestinationDirectory (Join-Path $temporary "version-mismatch") `
            -RequestedRepository "cdenihan/XFER" `
            -RequestedReleaseBaseUrl $fixtureUri `
            -SkipPathUpdate
    }
    catch {
        $versionRejected = $true
    }
    if (-not $versionRejected) {
        throw "FAIL: mismatched pinned binary version was accepted"
    }
}
finally {
    Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item Env:XFER_INSTALLER_SOURCE_ONLY -ErrorAction SilentlyContinue
}

Write-Host "install.ps1 tests passed"
