use super::{Entry, Interface, Order, SECTION, ip, walk};
use anyhow::{Context, Result};
use std::io::{self, Read, Seek, SeekFrom, Write};

pub(in crate::ssh::triage::packet_capture::local) fn write_excerpt(
    source: &mut (impl Read + Seek),
    bytes: u64,
    output: &mut impl Write,
    mut selected: impl FnMut(Option<i128>, Option<&ip::Packet>) -> bool,
) -> Result<u32> {
    let mut section = None;
    let mut interfaces = Vec::new();
    let mut written_interfaces = 0;
    let mut packets = 0;
    walk(source, bytes, |source, block, entry| {
        match entry {
            Entry::Section(order) => {
                section = Some(order);
                interfaces.clear();
                written_interfaces = 0;
            }
            Entry::Interface(interface) => interfaces.push(interface),
            Entry::Packet {
                interface,
                captured,
                original,
                timestamp,
                ticks,
                decoded,
            } if selected(timestamp, decoded.as_ref()) => {
                let order = section.context("Missing excerpt section")?;
                if written_interfaces == 0 {
                    write_section(output, order)?;
                }
                while written_interfaces <= interface {
                    write_interface(output, order, &interfaces[written_interfaces])?;
                    written_interfaces += 1;
                }

                let padded = u64::from(captured).next_multiple_of(4);
                let (length, payload_offset) = if let Some(ticks) = ticks {
                    let length =
                        u32::try_from(32 + padded).context("The excerpt packet is too large")?;
                    write_fields(
                        output,
                        order,
                        [
                            6,
                            length,
                            interface as u32,
                            (ticks >> 32) as u32,
                            ticks as u32,
                            captured,
                            original,
                        ],
                    )?;
                    (length, 28)
                } else {
                    // A simple packet has no timestamp and uses the section's first interface.
                    let length =
                        u32::try_from(16 + padded).context("The excerpt packet is too large")?;
                    write_fields(output, order, [3, length, original])?;
                    (length, 12)
                };
                source.seek(SeekFrom::Start(block.start + payload_offset))?;
                let copied = io::copy(&mut (&mut *source).take(u64::from(captured)), output)?;
                anyhow::ensure!(
                    copied == u64::from(captured),
                    "The source packet was truncated"
                );
                output.write_all(&[0; 3][..(padded - u64::from(captured)) as usize])?;
                output.write_all(&encode32(order, length))?;
                source.seek(SeekFrom::Start(block.end))?;
                packets += 1;
            }
            _ => (),
        }
        Ok(())
    })?;
    Ok(packets)
}

fn write_section(output: &mut impl Write, order: Order) -> io::Result<()> {
    output.write_all(&SECTION)?;
    output.write_all(&encode32(order, 28))?;
    output.write_all(&encode32(order, 0x1a2b3c4d))?;
    output.write_all(&encode16(order, 1))?;
    output.write_all(&encode16(order, 0))?;
    output.write_all(&[0xff; 8])?;
    output.write_all(&encode32(order, 28))
}

fn write_interface(output: &mut impl Write, order: Order, interface: &Interface) -> io::Result<()> {
    let mut options = Vec::new();
    let mut option = |code, data: &[u8]| {
        options.extend(encode16(order, code));
        options.extend(encode16(order, data.len() as u16));
        options.extend_from_slice(data);
        options.resize(options.len().next_multiple_of(4), 0);
    };
    if let Some(name) = &interface.name {
        option(2, name.as_bytes());
    }
    option(9, &[interface.resolution]);
    if let Some(fcs_length) = interface.fcs_length {
        option(13, &[fcs_length]);
    }
    if interface.offset_seconds != 0 {
        let bytes = match order {
            Order::Little => interface.offset_seconds.to_le_bytes(),
            Order::Big => interface.offset_seconds.to_be_bytes(),
        };
        option(14, &bytes);
    }
    option(0, &[]);
    let length = 20 + options.len() as u32;
    output.write_all(&encode32(order, 1))?;
    output.write_all(&encode32(order, length))?;
    output.write_all(&encode16(order, interface.link_type))?;
    output.write_all(&[0; 2])?;
    output.write_all(&encode32(order, interface.snap))?;
    output.write_all(&options)?;
    output.write_all(&encode32(order, length))
}

fn encode16(order: Order, value: u16) -> [u8; 2] {
    match order {
        Order::Little => value.to_le_bytes(),
        Order::Big => value.to_be_bytes(),
    }
}

fn encode32(order: Order, value: u32) -> [u8; 4] {
    match order {
        Order::Little => value.to_le_bytes(),
        Order::Big => value.to_be_bytes(),
    }
}

fn write_fields<const N: usize>(
    output: &mut impl Write,
    order: Order,
    fields: [u32; N],
) -> io::Result<()> {
    for field in fields {
        output.write_all(&encode32(order, field))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn preserves_simple_packets_without_inventing_timestamps() {
        let mut source = Vec::new();
        write_section(&mut source, Order::Little).unwrap();
        write_fields(&mut source, Order::Little, [1, 20, 101, 128, 20]).unwrap();
        write_fields(&mut source, Order::Little, [3, 20, 4, 0x44434241, 20]).unwrap();
        let mut output = Vec::new();
        let size = source.len() as u64;
        assert_eq!(
            write_excerpt(&mut Cursor::new(source), size, &mut output, |time, _| time
                .is_none())
            .unwrap(),
            1
        );
        let bytes = output.len() as u64;
        let mut packets = 0;
        walk(&mut Cursor::new(&output), bytes, |_, block, entry| {
            if let Entry::Packet {
                captured,
                original,
                ticks,
                timestamp,
                ..
            } = entry
            {
                assert_eq!((captured, original, ticks, timestamp), (4, 4, None, None));
                assert_eq!(
                    &output[block.start as usize + 12..block.start as usize + 16],
                    b"ABCD"
                );
                packets += 1;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(packets, 1);
    }

    #[test]
    fn retains_packet_ticks_and_interface_clock_in_a_mixed_endian_source() {
        let mut source = Vec::new();
        write_section(&mut source, Order::Little).unwrap();
        let section_start = source.len();
        for value in [0x0a0d0d0a_u32, 28, 0x1a2b3c4d, 0x00010002, 0, 80, 28] {
            source.extend(value.to_be_bytes());
        }
        for value in [
            1_u32,
            44,
            0x00650000,
            128,
            0x00090001,
            0x09000000,
            0x000e0008,
            u32::MAX,
            u32::MAX,
            0,
            44,
        ] {
            source.extend(value.to_be_bytes());
        }
        for value in [6_u32, 36, 0, 0, 1_234_567_890, 3, 9, 0x41424300, 36] {
            source.extend(value.to_be_bytes());
        }
        let mut output = Vec::new();
        let size = source.len() as u64;
        assert_eq!(
            write_excerpt(&mut Cursor::new(&source), size, &mut output, |ns, _| {
                ns.is_some_and(|ns| (234_567_890..234_567_891).contains(&ns))
            })
            .unwrap(),
            1
        );
        assert_eq!(&output[..12], &source[section_start..section_start + 12]);
        assert_eq!(
            &output[12..24],
            &[0, 1, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255]
        );
        let mut packets = 0;
        let bytes = output.len() as u64;
        walk(&mut Cursor::new(&output), bytes, |_, block, entry| {
            if let Entry::Packet {
                captured,
                original,
                ticks,
                timestamp,
                ..
            } = entry
            {
                assert_eq!((captured, original), (3, 9));
                assert_eq!(ticks, Some(1_234_567_890));
                assert_eq!(timestamp, Some(234_567_890));
                assert_eq!(
                    &output[block.start as usize + 28..block.start as usize + 31],
                    b"ABC"
                );
                packets += 1;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(packets, 1);
    }
}
