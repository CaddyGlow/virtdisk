$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$env:PATH = 'C:\bench\qemu;C:\Program Files\Oracle\VirtualBox;' + $env:PATH
$env:VBOX_USER_HOME = 'C:\bench\vbox-registry'
$root = 'C:/bench/results'
New-Item -ItemType Directory -Force $root | Out-Null
Start-Transcript -Path C:/bench/benchmarks.log -Force
try {
    $metadata = [ordered]@{
        OS = Get-CimInstance Win32_OperatingSystem | Select-Object Caption,Version,BuildNumber,TotalVisibleMemorySize
        CPU = Get-CimInstance Win32_Processor | Select-Object Name,NumberOfCores,NumberOfLogicalProcessors
        Disk = Get-CimInstance Win32_DiskDrive | Select-Object Model,Size,InterfaceType
        Volume = Get-Volume -DriveLetter C | Select-Object FileSystem,Size,SizeRemaining
        Features = Get-WindowsOptionalFeature -Online -FeatureName '*Hyper-V*' | Select-Object FeatureName,State
        VMMS = Get-Service vmms | Select-Object Name,Status
        Tools = Get-FileHash C:/bench/virtdisk.exe,C:/bench/io-bench.exe,C:/bench/benchmark-providers.py -Algorithm SHA256
        Python = & C:/bench/python/python.exe --version
    }
    $metadata | ConvertTo-Json -Depth 6 | Set-Content "$root/environment.json"
    foreach ($pattern in @('dense','sparse')) {
        foreach ($format in @('qcow2','vdi','vmdk','vhdx')) {
            $providers = @('virtdisk','qemu')
            $inputFormat = 'raw'
            if ($format -in @('vdi','vmdk')) { $providers += 'virtualbox' }
            if ($format -eq 'vhdx') { $providers += 'hyperv'; $inputFormat = 'vhdx' }
            Write-Output "Starting $pattern $inputFormat -> $format"
            & C:/bench/python/python.exe C:/bench/benchmark-providers.py --virtdisk C:/bench/virtdisk.exe --providers $providers --input-format $inputFormat --format $format --size-mib 256 --samples 5 --pattern $pattern --work-dir C:/bench --output "$root/$format-$pattern.json"
            if ($LASTEXITCODE) { throw "Benchmark failed: $format $pattern" }
        }
    }
    & C:/bench/io-bench.exe | Set-Content "$root/rust-io.csv"
    if ($LASTEXITCODE) { throw 'Rust benchmark failed' }
    'complete' | Set-Content "$root/status.txt"
} catch {
    $_ | Out-String | Set-Content "$root/error.txt"
    'failed' | Set-Content "$root/status.txt"
    throw
} finally { Stop-Transcript }
