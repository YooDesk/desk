param([string]$FixtureDirectory=(Join-Path $env:LOCALAPPDATA 'YooDesk-Stability'))
$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$stage=$FixtureDirectory
New-Item -ItemType Directory -Path $stage -Force|Out-Null
$form=New-Object System.Windows.Forms.Form
$form.Text='YooDesk isolated stability animation'
$form.ClientSize=New-Object System.Drawing.Size(640,320)
$form.TopMost=$true
$timer=New-Object System.Windows.Forms.Timer
$timer.Interval=33
$script:tick=0
$script:paints=0
$script:changes=0
$script:previous=$null
$clock=[Diagnostics.Stopwatch]::StartNew()
$pixel=New-Object Drawing.Bitmap 1,1
$graphics=[Drawing.Graphics]::FromImage($pixel)
$form.Add_Paint({$script:paints++})
$probe=New-Object Windows.Forms.Timer
$probe.Interval=100
$probe.Add_Tick({
 if(-not $form.Visible -or $form.WindowState -eq 'Minimized'){return}
 $point=$form.PointToScreen([Drawing.Point]::new(320,160))
 try {
  $graphics.CopyFromScreen($point.X,$point.Y,0,0,[Drawing.Size]::new(1,1))
  $color=$pixel.GetPixel(0,0).ToArgb()
  if($color -ne $script:previous){$script:changes++;$script:previous=$color}
 } catch { return }
 [ordered]@{elapsedMs=$clock.ElapsedMilliseconds;ticks=$script:tick;paints=$script:paints;pixelChanges=$script:changes} |
  ConvertTo-Json -Compress | Add-Content -Encoding ASCII -LiteralPath (Join-Path $stage 'motion-heartbeat.jsonl')
})
$timer.Add_Tick({
 if(Test-Path -LiteralPath (Join-Path $stage 'motion.stop')){$form.Close();return}
 $script:tick++
 $form.BackColor=[System.Drawing.Color]::FromArgb(($script:tick*5)%256,($script:tick*7)%256,($script:tick*11)%256)
})
@{pid=$PID;sessionId=(Get-Process -Id $PID).SessionId}|ConvertTo-Json|Set-Content -Encoding UTF8 -LiteralPath (Join-Path $stage 'motion.json')
$timer.Start()
$probe.Start()
try{[System.Windows.Forms.Application]::Run($form)}finally{
 $probe.Stop();$probe.Dispose();$graphics.Dispose();$pixel.Dispose()
 $timer.Stop();$timer.Dispose();$form.Dispose()
}
