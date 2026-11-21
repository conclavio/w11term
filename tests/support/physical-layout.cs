using System;
using System.Runtime.InteropServices;

public static class PhysicalLayoutFixture {
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
    [DllImport("user32.dll")] static extern IntPtr GetKeyboardLayout(uint thread);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int ToUnicodeEx(
        uint vk, uint scan, byte[] keys, [Out] char[] text, int length, uint flags, IntPtr layout);

    // Read the target's HKL, and evaluate private hypothetical key states. Flag
    // 4 leaves Windows' dead-key state untouched. No layout or toggles are set.
    public static int[] Printable(IntPtr window, uint expectedPid) {
        uint pid;
        uint thread = GetWindowThreadProcessId(window, out pid);
        if (thread == 0 || pid != expectedPid)
            throw new InvalidOperationException("Fixture input window changed");
        var layout = GetKeyboardLayout(thread);
        for (uint vk = 0x30; vk <= 0x5a; vk++) {
            if (vk > 0x39 && vk < 0x41) continue;
            int scalar = -1;
            bool suitable = true;
            foreach (uint scan in new uint[] {0x15, 0x2c}) {
                for (int locks = 0; locks < 4; locks++) {
                    var keys = new byte[256];
                    keys[0x14] = (byte)(locks & 1);
                    keys[0x90] = (byte)(locks >> 1);
                    var text = new char[8];
                    int n = ToUnicodeEx(vk, scan, keys, text, text.Length, 4, layout);
                    if (n != 1 || char.IsControl(text[0]) || char.IsSurrogate(text[0])
                        || text[0] == 'y' || text[0] == 'z' || (scalar >= 0 && scalar != text[0])) {
                        suitable = false;
                        break;
                    }
                    scalar = text[0];
                }
                if (!suitable) break;
            }
            if (suitable) return new int[] {(int)vk, scalar};
        }
        throw new InvalidOperationException("No stable printable probe for HKL " + layout.ToString("x"));
    }
}
