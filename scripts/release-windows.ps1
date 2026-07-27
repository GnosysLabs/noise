param(
    [Parameter(Mandatory = $true)]
    [string]$Revision,

    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,

    [string]$Repository = (Join-Path $env:USERPROFILE "noise"),
    [string]$UpdaterKeyPath = (Join-Path $env:USERPROFILE ".tauri\noise.key"),
    [string]$UpdaterPasswordPath = (Join-Path $env:LOCALAPPDATA "noise-release\updater-password.dpapi"),
    [int]$CargoBuildJobs = 4
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Command,

        [Parameter(ValueFromRemainingArguments = $true)]
        [string[]]$Arguments
    )

    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Command failed with exit code $LASTEXITCODE"
    }
}

function Get-PeSubsystem {
    param([Parameter(Mandatory = $true)][string]$Path)

    $stream = [System.IO.File]::OpenRead($Path)
    $reader = [System.IO.BinaryReader]::new($stream)
    try {
        $stream.Position = 0x3c
        $peOffset = $reader.ReadUInt32()
        $stream.Position = $peOffset
        if ($reader.ReadUInt32() -ne 0x00004550) {
            throw "$Path is not a valid PE executable"
        }
        $stream.Position = $peOffset + 24 + 68
        return $reader.ReadUInt16()
    }
    finally {
        $reader.Dispose()
        $stream.Dispose()
    }
}

foreach ($command in @("git", "node", "pnpm", "cargo", "rustc")) {
    if (-not (Get-Command $command -ErrorAction SilentlyContinue)) {
        throw "Missing required command: $command"
    }
}

if (-not (Test-Path -LiteralPath $Repository -PathType Container)) {
    throw "Windows noise checkout is missing: $Repository"
}
if (-not (Test-Path -LiteralPath $UpdaterKeyPath -PathType Leaf)) {
    throw "Updater signing key is missing: $UpdaterKeyPath"
}
if (-not (Test-Path -LiteralPath "$UpdaterKeyPath.pub" -PathType Leaf)) {
    throw "Updater public key is missing: $UpdaterKeyPath.pub"
}
if (-not (Test-Path -LiteralPath $UpdaterPasswordPath -PathType Leaf)) {
    throw "DPAPI-protected updater password is missing: $UpdaterPasswordPath"
}

$secureUpdaterPassword = Get-Content -LiteralPath $UpdaterPasswordPath -Raw |
    ConvertTo-SecureString
$passwordPointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR(
    $secureUpdaterPassword
)
try {
    $updaterPassword = [Runtime.InteropServices.Marshal]::PtrToStringBSTR(
        $passwordPointer
    )
}
finally {
    [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($passwordPointer)
}

Set-Location $Repository
Invoke-Checked git fetch --prune origin
Invoke-Checked git cat-file -e "$Revision^{commit}"
$resolvedRevision = (git rev-parse "$Revision^{commit}").Trim()
if ($LASTEXITCODE -ne 0 -or -not $resolvedRevision) {
    throw "Could not resolve Windows build revision: $Revision"
}

$shortRevision = $resolvedRevision.Substring(0, 12)
$worktreeRoot = Join-Path $env:LOCALAPPDATA "noise-release-worktrees"
$worktree = Join-Path $worktreeRoot "build-$shortRevision-$PID"
$cargoTarget = Join-Path $Repository "target"
$bundleDirectory = Join-Path $cargoTarget "release\bundle\nsis"
$worktreeAdded = $false

New-Item -ItemType Directory -Force -Path $worktreeRoot | Out-Null

try {
    Invoke-Checked git worktree add --detach $worktree $resolvedRevision
    $worktreeAdded = $true

    $clientDirectory = Join-Path $worktree "apps\client"
    $tauriConfigPath = Join-Path $clientDirectory "src-tauri\tauri.conf.json"
    $packagePath = Join-Path $clientDirectory "package.json"
    $cargoManifestPath = Join-Path $worktree "Cargo.toml"

    $tauriConfig = Get-Content -LiteralPath $tauriConfigPath -Raw | ConvertFrom-Json
    $package = Get-Content -LiteralPath $packagePath -Raw | ConvertFrom-Json
    $cargoManifest = Get-Content -LiteralPath $cargoManifestPath -Raw
    $cargoVersionMatch = [regex]::Match(
        $cargoManifest,
        '(?ms)^\[workspace\.package\]\s+version\s*=\s*"([^"]+)"'
    )
    if (-not $cargoVersionMatch.Success) {
        throw "Could not read the Rust workspace version"
    }

    $version = [string]$tauriConfig.version
    $cargoVersion = $cargoVersionMatch.Groups[1].Value
    if ($package.version -ne $version -or $cargoVersion -ne $version) {
        throw "Version mismatch: Tauri=$version package=$($package.version) Rust=$cargoVersion"
    }

    $configuredPublicKey = ([string]$tauriConfig.plugins.updater.pubkey).Trim()
    $keyPublicKey = (Get-Content -LiteralPath "$UpdaterKeyPath.pub" -Raw).Trim()
    if ($configuredPublicKey -ne $keyPublicKey) {
        throw "The Windows updater key does not match tauri.conf.json"
    }

    Invoke-Checked pnpm --dir $clientDirectory install --frozen-lockfile

    $env:TAURI_SIGNING_PRIVATE_KEY = $UpdaterKeyPath
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $updaterPassword
    $env:CARGO_BUILD_JOBS = [string]$CargoBuildJobs
    $env:CARGO_TARGET_DIR = $cargoTarget

    $preflightFile = Join-Path $env:TEMP "noise-updater-preflight-$PID"
    Set-Content -LiteralPath $preflightFile -Value "noise updater signing preflight"
    try {
        Invoke-Checked pnpm --dir $clientDirectory tauri signer sign $preflightFile
        if (-not (Test-Path -LiteralPath "$preflightFile.sig" -PathType Leaf)) {
            throw "Updater signing preflight did not produce a signature"
        }
    }
    finally {
        Remove-Item -LiteralPath $preflightFile -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath "$preflightFile.sig" -Force -ErrorAction SilentlyContinue
    }

    Remove-Item -LiteralPath $bundleDirectory -Recurse -Force -ErrorAction SilentlyContinue
    Invoke-Checked pnpm --dir $clientDirectory tauri build --bundles nsis

    $installers = @(
        Get-ChildItem -LiteralPath $bundleDirectory -Filter "*-setup.exe" -File
    )
    if ($installers.Count -ne 1) {
        throw "Expected one NSIS installer, found $($installers.Count)"
    }

    $installer = $installers[0]
    $signaturePath = "$($installer.FullName).sig"
    if (-not (Test-Path -LiteralPath $signaturePath -PathType Leaf)) {
        throw "Updater signature is missing: $signaturePath"
    }
    if ((Get-Item -LiteralPath $signaturePath).Length -eq 0) {
        throw "Updater signature is empty: $signaturePath"
    }

    $fileVersion = [System.Diagnostics.FileVersionInfo]::GetVersionInfo(
        $installer.FullName
    ).FileVersion
    if (-not $fileVersion.StartsWith($version)) {
        throw "Installer version $fileVersion does not match release $version"
    }

    $peSubsystem = Get-PeSubsystem -Path $installer.FullName
    if ($peSubsystem -ne 2) {
        throw "Installer PE subsystem is $peSubsystem, expected Windows GUI subsystem 2"
    }

    $worktreeStatus = @(git -C $worktree status --porcelain)
    if ($LASTEXITCODE -ne 0) {
        throw "Could not verify the Windows release worktree"
    }
    if ($worktreeStatus.Count -ne 0) {
        throw "The Windows release build changed tracked source files"
    }

    if (Test-Path -LiteralPath $OutputDirectory) {
        $existingOutput = @(Get-ChildItem -LiteralPath $OutputDirectory -Force)
        if ($existingOutput.Count -ne 0) {
            throw "Windows output directory is not empty: $OutputDirectory"
        }
    }
    else {
        New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
    }

    $outputInstaller = Join-Path $OutputDirectory $installer.Name
    $outputSignature = "$outputInstaller.sig"
    Copy-Item -LiteralPath $installer.FullName -Destination $outputInstaller
    Copy-Item -LiteralPath $signaturePath -Destination $outputSignature

    $installerHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $outputInstaller).Hash.ToLowerInvariant()
    $signatureHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $outputSignature).Hash.ToLowerInvariant()
    $authenticodeStatus = (Get-AuthenticodeSignature -LiteralPath $outputInstaller).Status

    Write-Output "RESULT_REVISION=$resolvedRevision"
    Write-Output "RESULT_VERSION=$version"
    Write-Output "RESULT_INSTALLER=$outputInstaller"
    Write-Output "RESULT_SIGNATURE=$outputSignature"
    Write-Output "RESULT_INSTALLER_SHA256=$installerHash"
    Write-Output "RESULT_SIGNATURE_SHA256=$signatureHash"
    Write-Output "RESULT_AUTHENTICODE_STATUS=$authenticodeStatus"
}
finally {
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY -ErrorAction SilentlyContinue
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
    $updaterPassword = $null
    $secureUpdaterPassword = $null

    if ($worktreeAdded) {
        Set-Location $Repository
        & git worktree remove --force $worktree
        & git worktree prune
    }
}
