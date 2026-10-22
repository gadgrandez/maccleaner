//! Reads the entitlements embedded in a Mach-O code signature, without running `codesign`.
//!
//! Only the first slice of a universal binary is read: every slice of a signed app carries
//! the same entitlements.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const FAT_MAGIC: u32 = 0xcafe_babe;
const FAT_MAGIC_64: u32 = 0xcafe_babf;
const MH_MAGIC_64: u32 = 0xfeed_facf;
const LC_CODE_SIGNATURE: u32 = 0x1d;
const CSMAGIC_EMBEDDED_SIGNATURE: u32 = 0xfade_0cc0;
const CSMAGIC_EMBEDDED_ENTITLEMENTS: u32 = 0xfade_7171;
const CSSLOT_ENTITLEMENTS: u32 = 5;
/// Load commands and signatures are small; anything bigger is not a valid binary.
const MAX_BLOB: u32 = 4 << 20;

fn u32_at(b: &[u8], at: usize, big_endian: bool) -> Option<u32> {
    let bytes: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
    Some(if big_endian { u32::from_be_bytes(bytes) } else { u32::from_le_bytes(bytes) })
}

fn read_at(f: &mut (impl Read + Seek), offset: u64, len: u32) -> Option<Vec<u8>> {
    if len > MAX_BLOB {
        return None;
    }
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Offset of the first 64-bit Mach-O image (the file itself or the first slice).
fn first_slice(f: &mut (impl Read + Seek)) -> Option<u64> {
    let head = read_at(f, 0, 32)?;
    match u32_at(&head, 0, true)? {
        FAT_MAGIC => u32_at(&head, 16, true).map(u64::from),
        FAT_MAGIC_64 => {
            let hi = u32_at(&head, 16, true)?;
            let lo = u32_at(&head, 20, true)?;
            Some((u64::from(hi) << 32) | u64::from(lo))
        }
        _ => (u32_at(&head, 0, false)? == MH_MAGIC_64).then_some(0),
    }
}

/// XML of the embedded entitlements, if the binary is signed with any.
pub fn entitlements_xml(f: &mut (impl Read + Seek)) -> Option<Vec<u8>> {
    let base = first_slice(f)?;
    let header = read_at(f, base, 32)?;
    if u32_at(&header, 0, false)? != MH_MAGIC_64 {
        return None;
    }
    let ncmds = u32_at(&header, 16, false)? as usize;
    let commands = read_at(f, base + 32, u32_at(&header, 20, false)?)?;
    let mut at = 0;
    let mut signature = None;
    for _ in 0..ncmds {
        let cmd = u32_at(&commands, at, false)?;
        let size = u32_at(&commands, at + 4, false)? as usize;
        if cmd == LC_CODE_SIGNATURE {
            signature = Some((u32_at(&commands, at + 8, false)?, u32_at(&commands, at + 12, false)?));
            break;
        }
        if size < 8 {
            return None;
        }
        at += size;
    }
    let (offset, size) = signature?;
    let blob = read_at(f, base + u64::from(offset), size)?;
    if u32_at(&blob, 0, true)? != CSMAGIC_EMBEDDED_SIGNATURE {
        return None;
    }
    let count = u32_at(&blob, 8, true)? as usize;
    (0..count.min(64)).find_map(|i| {
        let index = 12 + i * 8;
        if u32_at(&blob, index, true)? != CSSLOT_ENTITLEMENTS {
            return None;
        }
        let start = u32_at(&blob, index + 4, true)? as usize;
        if u32_at(&blob, start, true)? != CSMAGIC_EMBEDDED_ENTITLEMENTS {
            return None;
        }
        let len = u32_at(&blob, start + 4, true)? as usize;
        blob.get(start + 8..start.checked_add(len)?).map(<[u8]>::to_vec)
    })
}

/// App groups (`com.apple.security.application-groups`) an executable is entitled to.
pub fn app_groups(exe: &Path) -> Vec<String> {
    let Some(xml) = File::open(exe).ok().and_then(|mut f| entitlements_xml(&mut f)) else { return Vec::new() };
    let Ok(plist::Value::Dictionary(d)) = plist::Value::from_reader_xml(xml.as_slice()) else { return Vec::new() };
    d.get("com.apple.security.application-groups")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_string()).map(str::to_owned).collect())
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    /// Thin 64-bit Mach-O with an LC_CODE_SIGNATURE whose superblob holds `xml` as entitlements.
    pub(crate) fn signed_macho(xml: &[u8]) -> Vec<u8> {
        let ent_len = 8 + xml.len() as u32;
        let mut blob = Vec::new();
        blob.extend(CSMAGIC_EMBEDDED_SIGNATURE.to_be_bytes());
        blob.extend((12 + 8 + ent_len).to_be_bytes());
        blob.extend(1u32.to_be_bytes());
        blob.extend(CSSLOT_ENTITLEMENTS.to_be_bytes());
        blob.extend(20u32.to_be_bytes());
        blob.extend(CSMAGIC_EMBEDDED_ENTITLEMENTS.to_be_bytes());
        blob.extend(ent_len.to_be_bytes());
        blob.extend(xml);

        let mut m = Vec::new();
        m.extend(MH_MAGIC_64.to_le_bytes());
        m.extend([0u8; 12]);
        m.extend(2u32.to_le_bytes()); // ncmds
        m.extend(24u32.to_le_bytes()); // sizeofcmds
        m.extend([0u8; 8]);
        m.extend(0x19u32.to_le_bytes()); // an unrelated command first
        m.extend(8u32.to_le_bytes());
        m.extend(LC_CODE_SIGNATURE.to_le_bytes());
        m.extend(16u32.to_le_bytes());
        m.extend(64u32.to_le_bytes()); // dataoff
        m.extend((blob.len() as u32).to_le_bytes());
        m.resize(64, 0);
        m.extend(blob);
        m
    }

    /// Signed executable entitled to `groups`.
    pub(crate) fn entitled_to(groups: &[&str]) -> Vec<u8> {
        let items: String = groups.iter().map(|g| format!("<string>{g}</string>")).collect();
        signed_macho(
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
                <key>com.apple.security.application-groups</key><array>{items}</array></dict></plist>"#
            )
            .as_bytes(),
        )
    }

    #[test]
    fn reads_entitlements_of_thin_and_fat_binaries() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
            <key>com.apple.security.application-groups</key><array><string>group.com.foo.family</string></array>
            </dict></plist>"#;
        let thin = signed_macho(xml);
        assert_eq!(entitlements_xml(&mut Cursor::new(&thin)).as_deref(), Some(&xml[..]));

        let mut fat = Vec::new();
        fat.extend(FAT_MAGIC.to_be_bytes());
        fat.extend(1u32.to_be_bytes());
        fat.extend(0x0100_000c_u32.to_be_bytes());
        fat.extend(0u32.to_be_bytes());
        fat.extend(4096u32.to_be_bytes()); // offset
        fat.extend((thin.len() as u32).to_be_bytes());
        fat.extend(12u32.to_be_bytes());
        fat.resize(4096, 0);
        fat.extend(&thin);
        assert_eq!(entitlements_xml(&mut Cursor::new(&fat)).as_deref(), Some(&xml[..]));

        assert!(entitlements_xml(&mut Cursor::new(b"#!/bin/sh\n".to_vec())).is_none());
        assert!(entitlements_xml(&mut Cursor::new(&thin[..80])).is_none());
    }

    #[test]
    fn reads_app_groups_of_a_system_app() {
        let groups = app_groups(Path::new("/System/Applications/Notes.app/Contents/MacOS/Notes"));
        assert!(groups.iter().any(|g| g == "group.com.apple.notes"), "{groups:?}");
        assert!(app_groups(Path::new("/nonexistent")).is_empty());
    }
}
