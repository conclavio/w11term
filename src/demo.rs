use base64::Engine;

/// Real terminal protocol traffic, not UI widgets pretending to be a terminal.
pub fn bytes() -> Vec<u8> {
    let mut output = String::from("\x1b[2J\x1b[H\x1b]0;w11term · terminal lab\x07\x1b[?25h");
    output.push_str("\x1b[1;38;2;126;204;250mw11term\x1b[0m  Rust · Windows Reactor · DirectWrite / Direct3D\r\n\r\n");
    output.push_str("A real VT engine, ready for a remote shell.\r\n");
    output.push_str("Run: w11term ssh user@host --identity C:\\path\\to\\key\r\n\r\n");
    output.push_str("\x1b[1mANSI colors\x1b[0m\r\n");
    for index in 0..16 {
        output.push_str(&format!("\x1b[48;5;{index}m    "));
    }
    output.push_str("\x1b[0m\r\n\r\n\x1b[1mTrue color\x1b[0m\r\n");
    for index in 0..64 {
        output.push_str(&format!(
            "\x1b[48;2;{};{};{}m ",
            40 + index * 3,
            180 - index,
            240 - index * 2
        ));
    }
    output.push_str("\x1b[0m\r\n\r\n");
    output.push_str("\x1b[1mBold\x1b[0m   \x1b[4mUnderline\x1b[0m   \x1b[9mStrikethrough\x1b[0m   \x1b[7m Reverse \x1b[0m\r\n");
    output.push_str("Unicode: café  e\u{301}  Ελληνικά  日本語  한글  👩‍💻\r\n");
    output.push_str("Box drawing: ┌──────────┬──────────┐\r\n");
    output.push_str("             │ terminal │  engine  │\r\n");
    output.push_str("             └──────────┴──────────┘\r\n\r\n");
    output.push_str("\x1b[1mKitty graphics · in-band RGBA transfer\x1b[0m\r\n");
    let mut pixels = Vec::new();
    for y in 0..64u8 {
        for x in 0..128u8 {
            pixels.extend_from_slice(&[40 + x, 80 + y * 2, 230 - x, 255]);
        }
    }
    let payload = base64::engine::general_purpose::STANDARD.encode(pixels);
    // Kitty limits each base64 chunk to 4096 bytes.
    let chunks: Vec<_> = payload.as_bytes().chunks(4096).collect();
    for (index, chunk) in chunks.iter().enumerate() {
        let more = u8::from(index + 1 < chunks.len());
        if index == 0 {
            output.push_str(&format!(
                "\x1b_Ga=T,f=32,s=128,v=64,c=24,r=4,i=1,q=2,m={more};"
            ));
        } else {
            output.push_str(&format!("\x1b_Gm={more};"));
        }
        output.push_str(std::str::from_utf8(chunk).unwrap());
        output.push_str("\x1b\\");
    }
    output.push_str("\r\n\r\n\x1b[38;2;126;204;250mlab> \x1b[0m");
    output.into_bytes()
}
