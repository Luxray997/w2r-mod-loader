//! A minimal reader for the loaded game module's PE headers: image size and the sections, so
//! the host can tell "inside the module" from "inside the module's code". Pure parsing on byte
//! slices; [`Image::from_module`] reads the live headers under the crash guard.

use crate::guard::guarded;

pub const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const SECTION_HEADER_LEN: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub va: u32,
    pub size: u32,
    pub executable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub base: usize,
    pub size_of_image: u32,
    pub sections: Vec<Section>,
}

/// `e_lfanew` from a DOS header, if the magic is right.
pub fn pe_offset(dos: &[u8]) -> Option<u32> {
    if dos.len() < 0x40 || dos[..2] != *b"MZ" {
        return None;
    }
    Some(u32::from_le_bytes(dos[0x3C..0x40].try_into().unwrap()))
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().unwrap()))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().unwrap()))
}

/// `SizeOfImage` from an NT headers block (`PE\0\0`, file header, optional header). The field
/// sits at the same offset for PE32 and PE32+.
pub fn size_of_image_in(nt: &[u8]) -> Option<u32> {
    if nt.get(..4)? != b"PE\0\0" {
        return None;
    }
    let magic = u16_at(nt, 0x18)?;
    if magic != 0x10B && magic != 0x20B {
        return None;
    }
    u32_at(nt, 0x18 + 0x38)
}

/// How many bytes of NT headers [`Image::parse`] needs: file header plus optional header plus
/// the section table.
pub fn nt_headers_len(nt_head: &[u8]) -> Option<usize> {
    if nt_head.get(..4)? != b"PE\0\0" {
        return None;
    }
    let sections = u16_at(nt_head, 6)? as usize;
    let optional = u16_at(nt_head, 0x14)? as usize;
    Some(0x18 + optional + sections * SECTION_HEADER_LEN)
}

impl Image {
    /// Parses the headers of a module loaded at `base`.
    pub fn parse(base: usize, dos: &[u8], nt: &[u8]) -> Option<Image> {
        pe_offset(dos)?;
        let size_of_image = size_of_image_in(nt)?;
        let count = u16_at(nt, 6)? as usize;
        let table = 0x18 + u16_at(nt, 0x14)? as usize;
        let mut sections = Vec::with_capacity(count);
        for i in 0..count {
            let h = nt.get(table + i * SECTION_HEADER_LEN..table + (i + 1) * SECTION_HEADER_LEN)?;
            let name_end = h[..8].iter().position(|&b| b == 0).unwrap_or(8);
            let virtual_size = u32_at(h, 8)?;
            let raw_size = u32_at(h, 16)?;
            sections.push(Section {
                name: String::from_utf8_lossy(&h[..name_end]).into_owned(),
                va: u32_at(h, 12)?,
                size: if virtual_size != 0 {
                    virtual_size
                } else {
                    raw_size
                },
                executable: u32_at(h, 36)? & IMAGE_SCN_MEM_EXECUTE != 0,
            });
        }
        Some(Image {
            base,
            size_of_image,
            sections,
        })
    }

    /// Reads the live headers of the module at `base`. A bad or unmapped `base` gives `None`.
    pub fn from_module(base: usize) -> Option<Image> {
        guarded(|| {
            let dos = unsafe { core::slice::from_raw_parts(base as *const u8, 0x40) };
            let nt_at = base + pe_offset(dos)? as usize;
            let head = unsafe { core::slice::from_raw_parts(nt_at as *const u8, 0x18) };
            let len = nt_headers_len(head)?;
            let nt = unsafe { core::slice::from_raw_parts(nt_at as *const u8, len) };
            Image::parse(base, dos, nt)
        })
        .ok()
        .flatten()
    }

    pub fn end(&self) -> usize {
        self.base + self.size_of_image as usize
    }

    /// `[addr, addr+len)` lies inside the mapped image.
    pub fn contains(&self, addr: usize, len: usize) -> bool {
        len > 0 && addr >= self.base && addr.checked_add(len).is_some_and(|end| end <= self.end())
    }

    /// `[addr, addr+len)` lies inside one executable section.
    pub fn is_code(&self, addr: usize, len: usize) -> bool {
        self.contains(addr, len)
            && self.sections.iter().any(|s| {
                let start = self.base + s.va as usize;
                let end = start + s.size as usize;
                s.executable && addr >= start && addr + len <= end
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic() -> (Vec<u8>, Vec<u8>) {
        let mut dos = vec![0u8; 0x40];
        dos[..2].copy_from_slice(b"MZ");
        dos[0x3C..].copy_from_slice(&0x80u32.to_le_bytes());

        let opt_len = 0xE0usize; // PE32
        let mut nt = vec![0u8; 0x18 + opt_len + 2 * SECTION_HEADER_LEN];
        nt[..4].copy_from_slice(b"PE\0\0");
        nt[6..8].copy_from_slice(&2u16.to_le_bytes());
        nt[0x14..0x16].copy_from_slice(&(opt_len as u16).to_le_bytes());
        nt[0x18..0x1A].copy_from_slice(&0x10Bu16.to_le_bytes());
        nt[0x18 + 0x38..0x18 + 0x3C].copy_from_slice(&0x5000u32.to_le_bytes());
        let text = 0x18 + opt_len;
        nt[text..text + 5].copy_from_slice(b".text");
        nt[text + 8..text + 12].copy_from_slice(&0x1800u32.to_le_bytes());
        nt[text + 12..text + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        nt[text + 36..text + 40]
            .copy_from_slice(&(IMAGE_SCN_MEM_EXECUTE | 0x6000_0020).to_le_bytes());
        let data = text + SECTION_HEADER_LEN;
        nt[data..data + 5].copy_from_slice(b".data");
        nt[data + 8..data + 12].copy_from_slice(&0u32.to_le_bytes()); // falls back to raw size
        nt[data + 16..data + 20].copy_from_slice(&0x200u32.to_le_bytes());
        nt[data + 12..data + 16].copy_from_slice(&0x3000u32.to_le_bytes());
        nt[data + 36..data + 40].copy_from_slice(&0xC000_0040u32.to_le_bytes());
        (dos, nt)
    }

    #[test]
    fn parses_sections_and_classifies_ranges() {
        let (dos, nt) = synthetic();
        assert_eq!(nt_headers_len(&nt[..0x18]), Some(nt.len()));
        let img = Image::parse(0x40_0000, &dos, &nt).unwrap();
        assert_eq!(img.size_of_image, 0x5000);
        assert_eq!(
            img.sections,
            vec![
                Section {
                    name: ".text".into(),
                    va: 0x1000,
                    size: 0x1800,
                    executable: true
                },
                Section {
                    name: ".data".into(),
                    va: 0x3000,
                    size: 0x200,
                    executable: false
                },
            ]
        );
        assert!(img.contains(0x40_0000, 1));
        assert!(img.contains(0x40_4FFF, 1));
        assert!(!img.contains(0x40_5000, 1));
        assert!(!img.contains(0x40_4FFF, 2));
        assert!(!img.contains(0x40_1000, 0));
        assert!(!img.contains(0x3F_FFFF, 2));
        assert!(img.is_code(0x40_1000, 5));
        assert!(img.is_code(0x40_27FB, 5));
        assert!(!img.is_code(0x40_27FC, 5), "runs past .text");
        assert!(!img.is_code(0x40_3000, 5), ".data");
        assert!(!img.is_code(0x40_0000, 5), "headers");
    }

    #[test]
    fn rejects_bad_headers() {
        let (dos, nt) = synthetic();
        assert!(Image::parse(0, &[0; 0x40], &nt).is_none());
        let mut bad = nt.clone();
        bad[0x18] = 0; // optional magic
        assert!(Image::parse(0, &dos, &bad).is_none());
        assert!(
            Image::parse(0, &dos, &nt[..nt.len() - 1]).is_none(),
            "truncated section table"
        );
        assert_eq!(pe_offset(&[0; 0x40]), None);
        assert_eq!(size_of_image_in(&[0; 0x60]), None);
    }

    #[cfg(windows)]
    #[test]
    fn reads_this_process_and_tells_code_from_data() {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        let base = unsafe { GetModuleHandleW(core::ptr::null()) } as usize;
        let img = Image::from_module(base).expect("own headers");
        assert!(img.size_of_image > 0x1000 && img.size_of_image % 0x1000 == 0);
        assert!(
            img.sections
                .iter()
                .any(|s| s.name == ".text" && s.executable)
        );
        let code = reads_this_process_and_tells_code_from_data as fn() as usize;
        assert!(img.is_code(code, 5), "{code:#x} base {base:#x}");
        static DATA: [u8; 8] = [1; 8];
        let data = DATA.as_ptr() as usize;
        assert!(img.contains(data, 8));
        assert!(!img.is_code(data, 8));
        assert!(Image::from_module(0x10).is_none());
    }
}
