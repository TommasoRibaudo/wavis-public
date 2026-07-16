param(
  [Parameter(Mandatory=$true)][long]$Hwnd,
  [Parameter(Mandatory=$true)][string]$OutPath
)

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class WavisWinCapture {
  [DllImport("user32.dll")]
  public static extern bool GetClientRect(IntPtr hWnd, out RECT lpRect);
  [DllImport("user32.dll")]
  public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint nFlags);
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@

# PrintWindow captures the window's own paint buffer directly rather than a
# screen-coordinate region - unlike CopyFromScreen, this is correct even if
# the window has moved, is partially covered by other windows, or is off
# the visible desktop area.
$hwndPtr = [IntPtr]$Hwnd
$rect = New-Object WavisWinCapture+RECT
[WavisWinCapture]::GetClientRect($hwndPtr, [ref]$rect) | Out-Null
$width = $rect.Right - $rect.Left
$height = $rect.Bottom - $rect.Top

if ($width -le 0 -or $height -le 0) {
  Write-Error "Window $Hwnd has zero/invalid client size ($width x $height) - is it minimized?"
  exit 1
}

$bmp = New-Object System.Drawing.Bitmap $width, $height
$graphics = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $graphics.GetHdc()
# PW_RENDERFULLCONTENT (0x2) is required for modern DirectComposition-backed
# windows like WebView2 - the legacy PrintWindow path (flag 0) produces a
# blank/white capture for these.
$ok = [WavisWinCapture]::PrintWindow($hwndPtr, $hdc, 2)
$graphics.ReleaseHdc($hdc)
$bmp.Save($OutPath, [System.Drawing.Imaging.ImageFormat]::Png)
$graphics.Dispose()
$bmp.Dispose()

if (-not $ok) {
  Write-Error "PrintWindow returned false for hwnd $Hwnd - capture may be blank"
}
Write-Output "Saved $OutPath ($width x $height)"
