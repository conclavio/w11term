# Check the same keyboard messages used by the GUI fixtures, without showing or focusing a window.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class FixtureKeyQueue {
    [StructLayout(LayoutKind.Sequential)]
    private struct Message {
        public IntPtr hwnd;
        public uint message;
        public UIntPtr key;
        public IntPtr detail;
        public uint time;
        public int x, y;
        public uint reserved;
    }
    [DllImport("user32.dll")] private static extern bool PeekMessageW(out Message msg, IntPtr hwnd, uint first, uint last, uint remove);
    [DllImport("user32.dll")] private static extern bool TranslateMessage(ref Message msg);
    public static void Verify(IntPtr hwnd, uint key, bool extended, int expectedCharacter) {
        Message msg;
        foreach (uint kind in new uint[] { 0x100, 0x101 }) {
            if (!PeekMessageW(out msg, hwnd, kind, kind, 1) || msg.key.ToUInt64() != key)
                throw new Exception("Missing fixture key event " + kind);
            uint bits = unchecked((uint)msg.detail.ToInt64());
            if ((bits & 0xffff) != 1 || ((bits >> 16) & 0xff) == 0 ||
                ((bits & 0x01000000) != 0) != extended || (bits >> 30) != (kind == 0x101 ? 3u : 0u))
                throw new Exception("Invalid key metadata for event " + kind + ": " + bits.ToString("X8"));
            TranslateMessage(ref msg);
        }
        if (expectedCharacter >= 0 && (!PeekMessageW(out msg, hwnd, 0x102, 0x102, 1) || msg.key.ToUInt64() != (uint)expectedCharacter))
            throw new Exception("Missing translated character");
        if (PeekMessageW(out msg, hwnd, 0x102, 0x102, 1))
            throw new Exception("Unexpected translated character " + msg.key.ToUInt64());
    }
}
'@
$owner = [System.Windows.Forms.NativeWindow]::new()
try {
    $owner.CreateHandle([System.Windows.Forms.CreateParams]::new())
    $window = $owner.Handle
    Press-Key 13
    [FixtureKeyQueue]::Verify($window, 13, $false, 13)
    Press-Key 0x25 # dedicated Left, with its E0 prefix
    [FixtureKeyQueue]::Verify($window, 0x25, $true, -1)
} finally {
    $owner.DestroyHandle()
}
