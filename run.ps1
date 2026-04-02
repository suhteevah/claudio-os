# ClaudioOS launcher with credential persistence via QEMU fw_cfg
$credFile = "target\session.txt"

$fwCfgArgs = @()
if ((Test-Path $credFile) -and ((Get-Content $credFile -Raw).Trim().Length -gt 0)) {
    Write-Host "[run] Loaded saved session from $credFile"
    $fwCfgArgs = @("-fw_cfg", "name=opt/claudio/session,file=$credFile")
} else {
    Write-Host "[run] No saved session - will need to authenticate"
}

# Run QEMU with graphical window (framebuffer + PS/2 keyboard)
# Serial output goes to stdout for logging
# -display gtk,grab-on-hover=on ensures keyboard input reaches PS/2 controller
& "C:\Program Files\qemu\qemu-system-x86_64.exe" `
    -cpu Haswell `
    -drive "if=pflash,format=raw,readonly=on,file=C:\Program Files\qemu\share\edk2-x86_64-code.fd" `
    -drive "format=raw,file=target\x86_64-claudio\debug\claudio-os-uefi.img" `
    -device virtio-net-pci,netdev=net0 `
    -netdev user,id=net0 `
    -serial stdio `
    -display gtk,grab-on-hover=on `
    -m 1G `
    -no-reboot `
    @fwCfgArgs
