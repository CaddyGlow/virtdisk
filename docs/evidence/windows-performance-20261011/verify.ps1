$ErrorActionPreference='Stop'
$root="C:/virtdisk-streaming/run-$([DateTime]::UtcNow.Ticks)"
New-Item -ItemType Directory $root | Out-Null
Copy-Item C:/virtdisk-streaming/dense.raw,C:/virtdisk-streaming/sparse.raw,C:/virtdisk-streaming/virtdisk.exe $root
$results=@()
$env:VBOX_USER_HOME="$root/vbox-profile-$([DateTime]::UtcNow.Ticks)"
New-Item -ItemType Directory $env:VBOX_USER_HOME | Out-Null
foreach($pattern in @('dense','sparse')) {
 foreach($format in @('vdi','vhdx')) {
  if(Test-Path "$root/$pattern.$format"){Remove-Item "$root/$pattern.$format"}
  & "$root/virtdisk.exe" --working-memory 4194304 --streaming-allocation convert "$root/$pattern.raw" raw "$root/$pattern.$format" $format
  if($LASTEXITCODE -ne 0){throw "candidate conversion failed $pattern.$format $LASTEXITCODE"}
 }
 $out="$root/$pattern.vbox.raw"
 if(Test-Path $out){Remove-Item $out}
 & 'C:/Program Files/Oracle/VirtualBox/VBoxManage.exe' clonemedium disk "$root/$pattern.vdi" $out --format RAW
 if($LASTEXITCODE -ne 0){throw 'VBox clone failed'}
 $expected=(Get-FileHash "$root/$pattern.raw").Hash
 $actual=(Get-FileHash $out).Hash
 if($expected -ne $actual){throw 'VBox payload mismatch'}
 $results+=@{pattern=$pattern;consumer='VirtualBox';expected=$expected;actual=$actual;verified=$true}
 $nativePath=(Get-Item "$root/$pattern.vhdx").FullName
 $before=@(Get-Disk | ForEach-Object Number)
 @("select vdisk file=`"$nativePath`"", "attach vdisk readonly") | Set-Content "$root/attach.txt"
 & diskpart /s "$root/attach.txt"
 if($LASTEXITCODE -ne 0){throw "DiskPart attach failed"}
 try {
  $disk=Get-Disk | Where-Object { $_.Number -notin $before }
  if(@($disk).Count -ne 1){throw "expected one newly attached disk"}
  $stream=[IO.File]::Open("\\.\PhysicalDrive$($disk.Number)",[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::ReadWrite)
  try {
   $bytes=New-Object byte[] (Get-Item "$root/$pattern.raw").Length
   $pos=0
   while($pos -lt $bytes.Length){$count=$stream.Read($bytes,$pos,$bytes.Length-$pos);if($count -eq 0){throw 'short native read'};$pos+=$count}
   $sha=[Security.Cryptography.SHA256]::Create()
   $actual=[BitConverter]::ToString($sha.ComputeHash($bytes)).Replace('-','')
  } finally {$stream.Dispose()}
  if($expected -ne $actual){throw 'native VHDX payload mismatch'}
  $results+=@{pattern=$pattern;consumer='Windows DiskPart readonly attach / PhysicalDrive';expected=$expected;actual=$actual;verified=$true}
 } finally {
 @("select vdisk file=`"$nativePath`"", "detach vdisk") | Set-Content "$root/detach.txt"
 & diskpart /s "$root/detach.txt"
 }
}
$results | ConvertTo-Json | Set-Content "$root/results.json"
$results | ConvertTo-Json
