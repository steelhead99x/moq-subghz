//! `elf2uf2`: a minimal ELF to UF2 converter for the RP2040 (family 0xe48bff56): the PT_LOAD
//! segments by physical address in 256-byte blocks.

use anyhow::{bail, ensure, Context, Result};
use std::collections::BTreeMap;

fn u16at(b: &[u8], o: usize) -> Result<usize> {
    Ok(u16::from_le_bytes(
        b.get(o..o.checked_add(2).context("ELF header cut short")?)
            .context("ELF header cut short")?
            .try_into()?,
    ) as usize)
}

fn u32at(b: &[u8], o: usize) -> Result<usize> {
    Ok(u32::from_le_bytes(
        b.get(o..o.checked_add(4).context("ELF header cut short")?)
            .context("ELF header cut short")?
            .try_into()?,
    ) as usize)
}

/// The UF2 file for a 32-bit little-endian ELF, and a one-line description of it.
pub fn convert(elf: &[u8]) -> Result<(Vec<u8>, String)> {
    ensure!(
        elf.len() > 52 && &elf[..4] == b"\x7fELF" && elf[4] == 1,
        "not a 32-bit ELF"
    );
    let phoff = u32at(elf, 28)?;
    let (phentsize, phnum) = (u16at(elf, 42)?, u16at(elf, 44)?);
    let mut mem: BTreeMap<usize, u8> = BTreeMap::new();
    for i in 0..phnum {
        let p = i
            .checked_mul(phentsize)
            .and_then(|x| x.checked_add(phoff))
            .filter(|&p| p <= elf.len())
            .context("program header table out of range")?;
        let (t, off, paddr, filesz) = (
            u32at(elf, p)?,
            u32at(elf, p + 4)?,
            u32at(elf, p + 12)?,
            u32at(elf, p + 16)?,
        );
        if t == 1 && filesz > 0 {
            ensure!(
                (0x1000_0000..0x1020_0000).contains(&paddr)
                    && paddr.checked_add(filesz).is_some_and(|e| e <= 0x1020_0000),
                "segment at {paddr:#x} (+{filesz}) is outside the RP2040 flash"
            );
            let seg = off
                .checked_add(filesz)
                .and_then(|end| elf.get(off..end))
                .context("segment runs past the end of the file")?;
            for (j, b) in seg.iter().enumerate() {
                mem.insert(paddr + j, *b);
            }
        }
    }
    let pages: Vec<usize> = mem
        .keys()
        .map(|a| a & !255)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if pages.is_empty() {
        bail!("no loadable segments");
    }
    let mut out = Vec::with_capacity(pages.len() * 512);
    for (n, &p) in pages.iter().enumerate() {
        for w in [
            0x0A32_4655u32,
            0x9E5D_5157,
            0x2000,
            p as u32,
            256,
            n as u32,
            pages.len() as u32,
            0xe48b_ff56,
        ] {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out.extend((0..256).map(|k| mem.get(&(p + k)).copied().unwrap_or(0)));
        out.extend([0u8; 220]);
        out.extend_from_slice(&0x0AB1_6F30u32.to_le_bytes());
    }
    let last = pages[pages.len() - 1];
    Ok((
        out,
        format!(
            "{} blocks, {} bytes, 0x{:08x}..0x{:08x}",
            pages.len(),
            pages.len() * 256,
            pages[0],
            last + 256
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-segment ELF32: the payload at 0x10000000 + `at`.
    fn elf(at: u32, payload: &[u8]) -> Vec<u8> {
        let mut e = vec![0u8; 52 + 32];
        e[..5].copy_from_slice(b"\x7fELF\x01");
        e[28..32].copy_from_slice(&52u32.to_le_bytes());
        e[42..44].copy_from_slice(&32u16.to_le_bytes());
        e[44..46].copy_from_slice(&1u16.to_le_bytes());
        let ph = [
            1u32,
            84,
            0x2000_0000,
            0x1000_0000 + at,
            payload.len() as u32,
            payload.len() as u32,
            5,
            4,
        ];
        for (i, w) in ph.iter().enumerate() {
            e[52 + i * 4..56 + i * 4].copy_from_slice(&w.to_le_bytes());
        }
        e.extend_from_slice(payload);
        e
    }

    #[test]
    fn one_block_per_256_bytes() {
        let (uf2, line) = convert(&elf(0x100, &[7u8; 300])).unwrap();
        assert_eq!(uf2.len(), 2 * 512);
        assert_eq!(line, "2 blocks, 512 bytes, 0x10000100..0x10000300");
        assert_eq!(&uf2[..4], &0x0A32_4655u32.to_le_bytes());
        assert_eq!(&uf2[508..512], &0x0AB1_6F30u32.to_le_bytes());
        assert_eq!(uf2[32], 7);
        // second block: 44 bytes of payload, then zeros
        assert_eq!((uf2[512 + 32 + 43], uf2[512 + 32 + 44]), (7, 0));
    }

    #[test]
    fn refuses_what_it_cannot_place() {
        assert!(convert(b"nope").is_err());
        let mut e = elf(0, &[1]);
        e[52 + 12..52 + 16].copy_from_slice(&0x2000_0000u32.to_le_bytes());
        assert!(convert(&e).is_err());
    }

    #[test]
    fn convert_never_panics_on_damaged_elfs() {
        let mut rng = crate::testutil::Rng(13);
        // A minimal 32-bit ELF with one PT_LOAD of 8 bytes into flash.
        let mut e = vec![0u8; 52 + 32 + 8];
        e[..4].copy_from_slice(b"\x7fELF");
        e[4] = 1;
        e[28..32].copy_from_slice(&52u32.to_le_bytes());
        e[42..44].copy_from_slice(&32u16.to_le_bytes());
        e[44..46].copy_from_slice(&1u16.to_le_bytes());
        let ph = 52;
        e[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes());
        e[ph + 4..ph + 8].copy_from_slice(&84u32.to_le_bytes());
        e[ph + 12..ph + 16].copy_from_slice(&0x1000_0000u32.to_le_bytes());
        e[ph + 16..ph + 20].copy_from_slice(&8u32.to_le_bytes());
        assert!(convert(&e).is_ok());
        for _ in 0..5000 {
            let _ = convert(&rng.mutate(&e));
        }
        // A segment that starts in flash and runs past its end is refused, not expanded.
        let mut big = e.clone();
        big[ph + 16..ph + 20].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
        assert!(convert(&big).is_err());
    }

    fn elf_with(paddr: u32, filesz: u32, data: usize) -> Vec<u8> {
        let mut e = vec![0u8; 84 + data];
        e[..4].copy_from_slice(b"\x7fELF");
        e[4] = 1;
        e[28..32].copy_from_slice(&52u32.to_le_bytes());
        e[42..44].copy_from_slice(&32u16.to_le_bytes());
        e[44..46].copy_from_slice(&1u16.to_le_bytes());
        e[52..56].copy_from_slice(&1u32.to_le_bytes());
        e[56..60].copy_from_slice(&84u32.to_le_bytes());
        e[64..68].copy_from_slice(&paddr.to_le_bytes());
        e[68..72].copy_from_slice(&filesz.to_le_bytes());
        e
    }

    #[test]
    fn segments_are_checked_over_their_whole_range() {
        let end = 0x1020_0000u32;
        // ends exactly at the end of flash: fine
        assert!(convert(&elf_with(end - 16, 16, 16)).is_ok());
        // one byte past
        let e = convert(&elf_with(end - 16, 17, 17))
            .unwrap_err()
            .to_string();
        assert!(e.contains("outside the RP2040 flash"), "{e}");
        // starts at the first byte of flash
        assert!(convert(&elf_with(0x1000_0000, 4, 4)).is_ok());
        // one byte before flash
        assert!(convert(&elf_with(0x0fff_ffff, 4, 4)).is_err());
        // wrap-around addresses
        assert!(convert(&elf_with(0xffff_fff0, 0x20, 0x20)).is_err());
        assert!(convert(&elf_with(0x1000_0000, 0xffff_ffff, 4)).is_err());
        // the file range runs past the file
        assert!(convert(&elf_with(0x1000_0000, 64, 4))
            .unwrap_err()
            .to_string()
            .contains("past the end"));
    }
}
