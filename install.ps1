# Install the latest mtop release binary on Windows (x86_64).
#   irm https://raw.githubusercontent.com/Mariano215/mtop/main/install.ps1 | iex
# Downloads the zip and its SHA-256 from GitHub Releases, verifies the digest,
# puts mtop.exe in %LOCALAPPDATA%\Programs\mtop and adds that folder to the
# user PATH. Nothing else is touched. Set $env:MTOP_VERSION to pin a version.
$ErrorActionPreference = "Stop"
$repo = "Mariano215/mtop"
$target = "x86_64-pc-windows-msvc"

$version = $env:MTOP_VERSION
if (-not $version) {
  $version = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
}
$name = "mtop-$version-$target"
$base = "https://github.com/$repo/releases/download/$version"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("mtop-" + [System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  Write-Host "mtop: downloading $name.zip"
  Invoke-WebRequest "$base/$name.zip" -OutFile "$tmp\$name.zip"
  Invoke-WebRequest "$base/$name.zip.sha256" -OutFile "$tmp\$name.zip.sha256"
  $expected = ((Get-Content "$tmp\$name.zip.sha256") -split '\s+')[0].ToLower()
  $actual = (Get-FileHash "$tmp\$name.zip" -Algorithm SHA256).Hash.ToLower()
  if ($expected -ne $actual) { throw "checksum mismatch: expected $expected, got $actual" }
  Write-Host "mtop: checksum verified"
  Expand-Archive "$tmp\$name.zip" -DestinationPath $tmp -Force

  $dir = Join-Path $env:LOCALAPPDATA "Programs\mtop"
  New-Item -ItemType Directory -Path $dir -Force | Out-Null
  Copy-Item "$tmp\$name\mtop.exe" "$dir\mtop.exe" -Force
  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  if (($userPath -split ';') -notcontains $dir) {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$dir", "User")
    Write-Host "mtop: added $dir to your user PATH; open a new terminal"
  }
  Write-Host "mtop: installed $version to $dir\mtop.exe. Run: mtop"
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
