[CmdletBinding()]
param(
    [string]$Version,
    [string]$InstallDir,
    [string]$Repository,
    [string]$ReleaseBaseUrl,
    [switch]$NoModifyPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-XferNormalizedVersion {
    param([Parameter(Mandatory = $true)][string]$Value)

    if ($Value -eq "latest") {
        return "latest"
    }
    if ($Value.StartsWith("v", [StringComparison]::OrdinalIgnoreCase)) {
        return "v$($Value.Substring(1))"
    }
    return "v$Value"
}

function Get-XferArtifact {
    param([Parameter(Mandatory = $true)][string]$Architecture)

    switch ($Architecture.ToUpperInvariant()) {
        "AMD64" { return "xfer-windows-x86_64.exe" }
        "X86_64" { return "xfer-windows-x86_64.exe" }
        "ARM64" { return "xfer-windows-aarch64.exe" }
        "AARCH64" { return "xfer-windows-aarch64.exe" }
        default { throw "Unsupported Windows CPU architecture: $Architecture" }
    }
}

function Get-XferArchitecture {
    $architecture = $env:PROCESSOR_ARCHITEW6432
    if ([string]::IsNullOrWhiteSpace($architecture)) {
        $architecture = $env:PROCESSOR_ARCHITECTURE
    }
    if ([string]::IsNullOrWhiteSpace($architecture)) {
        throw "Could not determine the Windows CPU architecture"
    }
    return $architecture
}

function Copy-XferDownload {
    param(
        [Parameter(Mandatory = $true)][string]$Url,
        [Parameter(Mandatory = $true)][string]$Destination
    )

    $uri = [Uri]$Url
    if ($uri.IsFile) {
        Copy-Item -LiteralPath $uri.LocalPath -Destination $Destination -Force
        return
    }
    if ($uri.Scheme -ne "https") {
        throw "Refusing non-HTTPS download URL: $Url"
    }
    [Net.ServicePointManager]::SecurityProtocol = (
        [Net.ServicePointManager]::SecurityProtocol -bor
        [Net.SecurityProtocolType]::Tls12
    )
    Invoke-WebRequest -Uri $uri -OutFile $Destination -UseBasicParsing
}

function Add-XferToUserPath {
    param([Parameter(Mandatory = $true)][string]$Directory)

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $entries = @()
    if (-not [string]::IsNullOrWhiteSpace($userPath)) {
        $entries = $userPath.Split(";") | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    }
    if ($entries | Where-Object { $_.TrimEnd("\") -ieq $Directory.TrimEnd("\") }) {
        return $false
    }
    $updated = (@($entries) + $Directory) -join ";"
    [Environment]::SetEnvironmentVariable("Path", $updated, "User")
    return $true
}

function Install-Xfer {
    param(
        [string]$RequestedVersion,
        [string]$DestinationDirectory,
        [string]$RequestedRepository,
        [string]$RequestedReleaseBaseUrl,
        [switch]$SkipPathUpdate
    )

    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw "install.ps1 supports Windows; use install.sh on Linux or macOS"
    }

    if ([string]::IsNullOrWhiteSpace($RequestedVersion)) {
        $RequestedVersion = if ($env:XFER_VERSION) { $env:XFER_VERSION } else { "latest" }
    }
    if ([string]::IsNullOrWhiteSpace($RequestedRepository)) {
        $RequestedRepository = if ($env:XFER_REPOSITORY) {
            $env:XFER_REPOSITORY
        }
        else {
            "cdenihan/XFER"
        }
    }
    if ([string]::IsNullOrWhiteSpace($RequestedReleaseBaseUrl)) {
        $RequestedReleaseBaseUrl = if ($env:XFER_RELEASE_BASE_URL) {
            $env:XFER_RELEASE_BASE_URL.TrimEnd("/")
        }
        else {
            "https://github.com/$RequestedRepository/releases"
        }
    }
    if ([string]::IsNullOrWhiteSpace($DestinationDirectory)) {
        $DestinationDirectory = if ($env:XFER_INSTALL_DIR) {
            $env:XFER_INSTALL_DIR
        }
        else {
            Join-Path $env:LOCALAPPDATA "Programs\XFER\bin"
        }
    }

    $normalizedVersion = Get-XferNormalizedVersion $RequestedVersion
    $artifact = Get-XferArtifact (Get-XferArchitecture)
    $downloadBase = if ($normalizedVersion -eq "latest") {
        "$RequestedReleaseBaseUrl/latest/download"
    }
    else {
        "$RequestedReleaseBaseUrl/download/$normalizedVersion"
    }

    $temporary = Join-Path ([IO.Path]::GetTempPath()) "xfer-install-$([Guid]::NewGuid())"
    New-Item -ItemType Directory -Path $temporary | Out-Null
    try {
        $binaryPath = Join-Path $temporary $artifact
        $checksumPath = "$binaryPath.sha256"
        Write-Host "xfer-install: downloading $artifact ($normalizedVersion)"
        Copy-XferDownload "$downloadBase/$artifact" $binaryPath
        Copy-XferDownload "$downloadBase/$artifact.sha256" $checksumPath

        $checksumLine = (Get-Content -LiteralPath $checksumPath -TotalCount 1).Trim()
        $expected = ($checksumLine -split "\s+")[0]
        if ($expected -notmatch "^[0-9A-Fa-f]{64}$") {
            throw "Release checksum file is malformed"
        }
        $actual = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
        if ($actual -ine $expected) {
            throw "SHA-256 checksum verification failed"
        }

        $reportedVersion = ((& $binaryPath --version) -join " ").Trim()
        if ($LASTEXITCODE -ne 0) {
            throw "Downloaded binary could not run on this machine"
        }
        if (-not $reportedVersion.StartsWith("xfer ", [StringComparison]::Ordinal)) {
            throw "Downloaded file did not identify itself as XFER"
        }
        if (
            $normalizedVersion -ne "latest" -and
            $reportedVersion -ne "xfer $($normalizedVersion.Substring(1))"
        ) {
            throw "Downloaded binary version does not match requested release $normalizedVersion"
        }

        New-Item -ItemType Directory -Path $DestinationDirectory -Force | Out-Null
        $destination = Join-Path $DestinationDirectory "xfer.exe"
        $staging = Join-Path $DestinationDirectory ".xfer-install-$([Guid]::NewGuid()).exe"
        try {
            Copy-Item -LiteralPath $binaryPath -Destination $staging -Force
            if (Test-Path -LiteralPath $destination) {
                [IO.File]::Replace($staging, $destination, $null)
            }
            else {
                [IO.File]::Move($staging, $destination)
            }
        }
        finally {
            Remove-Item -LiteralPath $staging -Force -ErrorAction SilentlyContinue
        }

        $installedVersion = (& $destination --version) -join " "
        Write-Host "xfer-install: installed $installedVersion to $destination"
        if (-not $SkipPathUpdate) {
            $changed = Add-XferToUserPath $DestinationDirectory
            if ($changed) {
                Write-Host "xfer-install: added $DestinationDirectory to your user PATH"
                Write-Host "xfer-install: open a new terminal before running xfer"
            }
        }
    }
    finally {
        Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if ($env:XFER_INSTALLER_SOURCE_ONLY -ne "1") {
    Install-Xfer `
        -RequestedVersion $Version `
        -DestinationDirectory $InstallDir `
        -RequestedRepository $Repository `
        -RequestedReleaseBaseUrl $ReleaseBaseUrl `
        -SkipPathUpdate:$NoModifyPath
}
