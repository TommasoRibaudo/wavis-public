param([Parameter(Mandatory=$true)][int]$ProcessId)

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Collections.Generic;
public class WavisWinEnum {
  public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);
  [DllImport("user32.dll")] public static extern int GetWindowThreadProcessId(IntPtr hWnd, out int lpdwProcessId);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr hWnd, StringBuilder lpString, int nMaxCount);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);

  public static List<string> ListForPid(int pid) {
    var results = new List<string>();
    EnumWindows((hWnd, lParam) => {
      int windowPid;
      GetWindowThreadProcessId(hWnd, out windowPid);
      if (windowPid == pid && IsWindowVisible(hWnd)) {
        var sb = new StringBuilder(256);
        GetWindowText(hWnd, sb, 256);
        if (sb.Length > 0) results.Add(hWnd + "|" + sb.ToString());
      }
      return true;
    }, IntPtr.Zero);
    return results;
  }
}
"@

# One "hwnd|title" per line, machine-parseable by driver.mjs.
[WavisWinEnum]::ListForPid($ProcessId) | ForEach-Object { Write-Output $_ }
