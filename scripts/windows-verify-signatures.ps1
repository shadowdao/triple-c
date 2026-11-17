# windows-verify-signatures.ps1 <path-or-wildcard>... - fail unless everything
# that ships carries a valid, timestamped Authenticode signature.
#
# The check that makes signing load-bearing rather than hopeful: Tauri skips
# signing silently in some configurations (no sign command, --no-sign), and an
# unsigned installer looks exactly like a signed one until SmartScreen blocks
# it on a user's machine. Every pattern must match at least one file, so a
# bundle that was never produced cannot pass either.
#
# Pass the installers, not target\release\triple-c.exe. The app binary users
# get is the copy inside each installer: Tauri patches the loose file with
# bundle-type information before each bundle, signs it, packages it, and
# patches it again, so the loose copy ends up unsigned by design. The MSI is
# unpacked with an administrative install and its binaries checked directly;
# the NSIS installer cannot be unpacked that way, so for it the signing log
# must show the app binary and the uninstaller were signed.
#
# The bundled session-manager-plugin (built from source, signed by us) is
# checked too: present in the MSI, and in both installers validly signed,
# timestamped, and by the same signing identity as the installer itself.

param([Parameter(Mandatory = $true, ValueFromRemainingArguments = $true)][string[]]$Patterns)

$ErrorActionPreference = 'Stop'
if (-not $env:TRIPLE_C_SIGNTOOL) { throw 'TRIPLE_C_SIGNTOOL is not set - run windows-signing-setup.ps1 first' }

$files = foreach ($pattern in $Patterns) {
    $found = @(Get-ChildItem -Path $pattern -File -ErrorAction SilentlyContinue)
    if ($found.Count -eq 0) { throw "Nothing to verify matches $pattern" }
    $found
}

function Test-Signature([IO.FileInfo]$File, [string]$Label) {
    # signtool's own check - chain to a trusted root under the default
    # Authenticode policy - with Stop relaxed for the native call, as in
    # windows-sign.ps1.
    $ErrorActionPreference = 'Continue'
    $output = & $env:TRIPLE_C_SIGNTOOL verify /pa $File.FullName 2>&1 | ForEach-Object { "$_" }
    $signtoolOk = ($LASTEXITCODE -eq 0)
    $ErrorActionPreference = 'Stop'
    if (-not $signtoolOk) { $output | Write-Host }

    # And the timestamp, which signtool verify does not require.
    $sig = Get-AuthenticodeSignature -FilePath $File.FullName
    $timestamped = $null -ne $sig.TimeStamperCertificate
    if ($signtoolOk -and $sig.Status -eq 'Valid' -and $timestamped) {
        Write-Host "OK   $Label - $($sig.SignerCertificate.Subject)"
        return $true
    }
    Write-Host "FAIL $Label - status $($sig.Status), signtool $(if ($signtoolOk) {'ok'} else {'failed'}), timestamped $timestamped"
    return $false
}

# The session-manager-plugin sidecar is built from AWS's source
# (build-ssm-plugin.ps1), so it is unsigned until windows-sign.ps1 signs it. It
# must carry OUR signature: valid, timestamped, and from the same signing
# identity (subject and issuer) as the installer that ships it.
$sidecarName = 'session-manager-plugin.exe'
$sidecarSource = Join-Path (Split-Path -Parent $PSScriptRoot) 'app\src-tauri\binaries\session-manager-plugin-x86_64-pc-windows-msvc.exe'
function Test-OurSigner([IO.FileInfo]$File, [string]$Label, [IO.FileInfo]$Installer) {
    if (-not (Test-Signature $File $Label)) { return $false }
    $ours = (Get-AuthenticodeSignature -FilePath $Installer.FullName).SignerCertificate
    $theirs = (Get-AuthenticodeSignature -FilePath $File.FullName).SignerCertificate
    # Subject + Issuer, not the thumbprint: Azure Artifact Signing issues a
    # fresh short-lived certificate (daily), so the plugin and the installer
    # can legitimately carry different thumbprints within one build.
    if ($ours -and $theirs -and $ours.Subject -eq $theirs.Subject -and $ours.Issuer -eq $theirs.Issuer) { return $true }
    Write-Host "FAIL $Label - signed by '$($theirs.Subject)' (issuer '$($theirs.Issuer)'), not the identity that signed $($Installer.Name)"
    return $false
}

function Get-SignedLog {
    if ($env:TRIPLE_C_SIGN_LOG -and (Test-Path $env:TRIPLE_C_SIGN_LOG)) { return @(Get-Content $env:TRIPLE_C_SIGN_LOG) }
    return @()
}

$failed = @()
$nsisBuilt = $false
foreach ($file in $files) {
    if (-not (Test-Signature $file $file.Name)) { $failed += $file.Name }
    if ($file.FullName -match '\\bundle\\nsis\\') { $nsisBuilt = $true; $nsisInstaller = $file }

    if ($file.Extension -eq '.msi') {
        $extract = Join-Path ([IO.Path]::GetTempPath()) ('msi-verify-' + [guid]::NewGuid().ToString('N'))
        $proc = Start-Process msiexec.exe -Wait -PassThru `
            -ArgumentList '/a', "`"$($file.FullName)`"", '/qn', "TARGETDIR=`"$extract`""
        $inner = @()
        if ($proc.ExitCode -eq 0) { $inner = @(Get-ChildItem -Path $extract -Recurse -File -Include *.exe, *.dll) }
        if ($proc.ExitCode -ne 0) {
            Write-Host "FAIL $($file.Name) - administrative extract exited $($proc.ExitCode)"
            $failed += "$($file.Name) (extract)"
        } elseif ($inner.Count -eq 0) {
            Write-Host "FAIL $($file.Name) - contains no executable to check"
            $failed += "$($file.Name) (no executable)"
        }
        foreach ($f in $inner) {
            if (-not (Test-Signature $f "$($file.Name) > $($f.Name)")) { $failed += "$($file.Name) > $($f.Name)" }
        }
        # Its licence notices travel with it (bundle.resources).
        if ($proc.ExitCode -eq 0 -and -not @(Get-ChildItem -Path $extract -Recurse -File -Filter 'THIRD_PARTY_NOTICES').Count) {
            Write-Host "FAIL $($file.Name) - carries no THIRD_PARTY_NOTICES"
            $failed += "$($file.Name) > THIRD_PARTY_NOTICES (missing)"
        }
        # The bundled plugin must be in there, under our signature.
        if ($proc.ExitCode -eq 0) {
            $plugin = @($inner | Where-Object { $_.Name -eq $sidecarName })
            if ($plugin.Count -ne 1) {
                Write-Host "FAIL $($file.Name) - carries $($plugin.Count) $sidecarName, expected 1"
                $failed += "$($file.Name) > $sidecarName (missing)"
            } elseif (-not (Test-OurSigner $plugin[0] "$($file.Name) > $sidecarName" $file)) {
                $failed += "$($file.Name) > $sidecarName (signer)"
            }
        }
        Remove-Item -Recurse -Force $extract -ErrorAction SilentlyContinue
    }
}

# What the NSIS installer carries but cannot be unpacked here. windows-sign.ps1
# logs every file it signs. The app binary is signed in place under
# target\release; the uninstaller is the file makensis wrote under the job's
# temp directory (see windows-signing-setup.ps1) - and makensis ignores the
# sign command's exit code for it, so without this a failure there is silent.
if ($nsisBuilt) {
    $log = Get-SignedLog
    $appSigned = @($log | Where-Object { $_ -match '\\target\\release\\[^\\]+\.exe$' })
    if ($appSigned.Count -eq 0) {
        Write-Host 'FAIL app binary - no signature was logged for it before packaging'
        $failed += 'app binary'
    } else {
        Write-Host "OK   app binary - signed before packaging ($($appSigned.Count)x)"
    }
    # makensis packs the sidecar straight from app\src-tauri\binaries, where
    # Tauri had it signed in place before any bundle: so that file, and a
    # logged signature for it, are what NSIS shipped.
    if (-not (Test-Path -LiteralPath $sidecarSource)) {
        Write-Host "FAIL $sidecarName - $sidecarSource is missing"
        $failed += "NSIS > $sidecarName (missing)"
    } elseif (-not (Test-OurSigner (Get-Item -LiteralPath $sidecarSource) "NSIS > $sidecarName (as packed)" $nsisInstaller)) {
        $failed += "NSIS > $sidecarName (signer)"
    } elseif (-not @($log | Where-Object { $_ -match '\\src-tauri\\binaries\\session-manager-plugin-x86_64-pc-windows-msvc\.exe$' }).Count) {
        Write-Host "FAIL $sidecarName - no signature was logged for it"
        $failed += "NSIS > $sidecarName (not logged)"
    }
    $tmp = $env:TRIPLE_C_SIGN_TMP
    $uninstaller = @()
    if ($tmp) { $uninstaller = @($log | Where-Object { $_.StartsWith($tmp, [StringComparison]::OrdinalIgnoreCase) }) }
    if ($uninstaller.Count -eq 0) {
        Write-Host 'FAIL NSIS uninstaller - no signature was logged for it'
        $failed += 'NSIS uninstaller'
    } else {
        Write-Host "OK   NSIS uninstaller - signed as $($uninstaller[-1])"
    }
}

if ($failed.Count -gt 0) { throw "Not validly signed: $($failed -join ', ')" }
Write-Host 'Everything that ships is signed and timestamped.'
