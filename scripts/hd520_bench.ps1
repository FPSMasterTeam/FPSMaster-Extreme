# Run the headed game benchmark inside the logged-on interactive desktop session
# via a scheduled task (so the window/GPU actually work — an SSH session 0 can't
# create a GPU window). Writes the chosen options, runs scripted-smoke, waits for
# it to finish, and prints the profile breakdown. Driven over SSH by the dev box.
param(
    [double]$Scale = 1.0,
    [int]$Seconds = 18,
    [string]$Demo = "landscape",
    [int]$Mipmaps = 4,
    [string]$Graphics = "true",  # "true" = fancy, "false" = fast
    [string]$Window = "",        # e.g. "1280x720" to force a physical window size (CLI)
    [int]$ResW = 0,              # settings resolution width (0 = native)
    [int]$ResH = 0,
    [string]$Fullscreen = "false"
)

$dir = "C:\Users\Super\MiniCraft"
$task = "recraft_bench"

# Persisted options the game loads on start (scripted-smoke forces vsync off).
@"
render_scale=$Scale
vsync=false
fps_cap=260
mipmap_levels=$Mipmaps
fancy_graphics=$Graphics
resolution_w=$ResW
resolution_h=$ResH
fullscreen=$Fullscreen
"@ | Set-Content -Encoding ASCII "$dir\recraft_options.txt"

# The actual command, in a .bat so the scheduled action stays simple.
$winArg = if ($Window) { " --window $Window" } else { "" }
"cd /d $dir`r`n`"$dir\recraft_app.exe`" --demo $Demo --scripted-smoke $Seconds$winArg > `"$dir\prof.txt`" 2>&1" |
    Set-Content -Encoding ASCII "$dir\run_bench.bat"

# (Re)register an interactive task running as the current logged-on user — no
# stored password needed, and the window appears on the real desktop + GPU.
$me = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$action = New-ScheduledTaskAction -Execute "$dir\run_bench.bat"
$principal = New-ScheduledTaskPrincipal -UserId $me -LogonType Interactive
Register-ScheduledTask -TaskName $task -Action $action -Principal $principal -Force | Out-Null

Remove-Item "$dir\prof.txt" -ErrorAction SilentlyContinue
Start-ScheduledTask -TaskName $task

$deadline = (Get-Date).AddSeconds($Seconds + 30)
while ((Get-Date) -lt $deadline) {
    Start-Sleep -Seconds 2
    if ((Get-ScheduledTask -TaskName $task).State -eq "Ready" -and (Test-Path "$dir\prof.txt")) {
        break
    }
}
Start-Sleep -Seconds 1
Write-Output "===== scale=$Scale mipmaps=$Mipmaps graphics=$Graphics ====="
if (Test-Path "$dir\prof.txt") { Get-Content "$dir\prof.txt" } else { Write-Output "NO OUTPUT (task did not produce prof.txt)" }
Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue
