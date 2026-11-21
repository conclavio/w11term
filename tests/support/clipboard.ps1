# Read the same CF_UNICODETEXT handle the GUI publishes. Windows PowerShell's
# Get-Clipboard uses OLE and can fail to marshal data on a hosted desktop.
Add-Type @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class FixtureClipboard {
    [DllImport("user32.dll")] public static extern uint GetClipboardSequenceNumber();
    [DllImport("user32.dll", SetLastError=true)] static extern bool OpenClipboard(IntPtr owner);
    [DllImport("user32.dll")] static extern bool CloseClipboard();
    [DllImport("user32.dll")] static extern IntPtr GetClipboardData(uint format);
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr GlobalLock(IntPtr handle);
    [DllImport("kernel32.dll")] static extern bool GlobalUnlock(IntPtr handle);
    public static bool ContainsNewText(uint before, string expected) {
        if (GetClipboardSequenceNumber() == before) return false;
        if (!OpenClipboard(IntPtr.Zero)) {
            int error = Marshal.GetLastWin32Error();
            if (error == 5 || error == 170) return false; // in use, retry within Wait-Until
            throw new Win32Exception(error);
        }
        try {
            IntPtr handle = GetClipboardData(13); // CF_UNICODETEXT
            if (handle == IntPtr.Zero) return false;
            IntPtr text = GlobalLock(handle);
            if (text == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
            try { return String.Equals(Marshal.PtrToStringUni(text), expected, StringComparison.Ordinal); }
            finally { GlobalUnlock(handle); }
        } finally { CloseClipboard(); }
    }
}
'@
