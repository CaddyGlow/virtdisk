$ErrorActionPreference = 'Stop'
$directory = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
New-Item -ItemType Directory $directory | Out-Null
$image = Join-Path $directory 'physical.vhd'
$commands = Join-Path $directory 'diskpart.txt'
try {
    @("create vdisk file=`"$image`" maximum=8 type=fixed", "select vdisk file=`"$image`"", 'attach vdisk') | Set-Content $commands
    diskpart /s $commands | Out-Host
    $disk = Get-DiskImage -ImagePath $image | Get-Disk
    $device = "\\.\PhysicalDrive$($disk.Number)"
    # Only write the device belonging to the newly created disposable VHD.
    $bytes = New-Object byte[] (8 * 1024 * 1024)
    # Mix the full 512-byte sector number into each eight-byte group.
    for ([long]$i = 0; $i -lt $bytes.Length; $i++) {
        $bytes[$i] = ($i -band 255) -bxor ((($i -shr 9) -shr (8 * ($i % 8))) -band 255)
    }
    $stream = [IO.File]::Open($device, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::ReadWrite)
    try { $stream.Write($bytes, 0, $bytes.Length); $stream.Flush() } finally { $stream.Dispose() }
    @("select vdisk file=`"$image`"", 'detach vdisk', 'attach vdisk readonly') | Set-Content $commands
    diskpart /s $commands | Out-Host
    $disk = Get-DiskImage -ImagePath $image | Get-Disk
    $device = "\\.\PhysicalDrive$($disk.Number)"
    $env:VIRTDISK_TEST_DEVICE = $device
    cargo test --all-features --locked --test physical_disk -- --ignored
    if ($LASTEXITCODE -ne 0) { throw 'Native physical disk tests failed' }
} finally {
    @("select vdisk file=`"$image`"", 'detach vdisk') | Set-Content $commands
    diskpart /s $commands | Out-Host
    Remove-Item Env:VIRTDISK_TEST_DEVICE -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $directory
}
