use crate::backend::x86_64::encode::EncodedProgram;

const DOS_STUB_SIZE: usize = 0x80;
const PE_SIGNATURE_SIZE: usize = 4;
const COFF_HEADER_SIZE: usize = 20;
const OPTIONAL_HEADER_SIZE: usize = 240;
const SECTION_HEADER_SIZE: usize = 40;
const FILE_ALIGNMENT: u32 = 0x200;
const SECTION_ALIGNMENT: u32 = 0x1000;
const IMAGE_BASE: u64 = 0x1_4000_0000;
const SECTION_RVA: u32 = 0x1000;
const SUBSYSTEM_WINDOWS_CUI: u16 = 3;

#[derive(Debug, Clone, Copy)]
pub struct DataDirectory {
    pub rva: u32,
    pub size: u32,
}

pub fn build_pe_executable(
    encoded: &EncodedProgram,
    entry_offset: u32,
    import_directory: DataDirectory,
    iat_directory: DataDirectory,
) -> Vec<u8> {
    let headers_unaligned =
        DOS_STUB_SIZE + PE_SIGNATURE_SIZE + COFF_HEADER_SIZE + OPTIONAL_HEADER_SIZE + SECTION_HEADER_SIZE;
    let size_of_headers = align_up(headers_unaligned as u32, FILE_ALIGNMENT);
    let size_of_code = encoded.text.len() as u32;
    let size_of_raw_data = align_up(size_of_code, FILE_ALIGNMENT);
    let size_of_image = SECTION_RVA + align_up(size_of_code, SECTION_ALIGNMENT);
    let pointer_to_raw_data = size_of_headers;
    let mut out = Vec::with_capacity((pointer_to_raw_data + size_of_raw_data) as usize);

    out.extend_from_slice(&build_dos_stub());
    out.extend_from_slice(b"PE\0\0");
    push_u16(&mut out, 0x8664);
    push_u16(&mut out, 1);
    push_u32(&mut out, 0);
    push_u32(&mut out, 0);
    push_u32(&mut out, 0);
    push_u16(&mut out, OPTIONAL_HEADER_SIZE as u16);
    push_u16(&mut out, 0x0022);

    push_u16(&mut out, 0x20B);
    out.push(0);
    out.push(0);
    push_u32(&mut out, size_of_code);
    push_u32(&mut out, 0);
    push_u32(&mut out, 0);
    push_u32(&mut out, SECTION_RVA + entry_offset);
    push_u32(&mut out, SECTION_RVA);
    push_u64(&mut out, IMAGE_BASE);
    push_u32(&mut out, SECTION_ALIGNMENT);
    push_u32(&mut out, FILE_ALIGNMENT);
    push_u16(&mut out, 6);
    push_u16(&mut out, 0);
    push_u16(&mut out, 0);
    push_u16(&mut out, 0);
    push_u16(&mut out, 6);
    push_u16(&mut out, 0);
    push_u32(&mut out, 0);
    push_u32(&mut out, size_of_image);
    push_u32(&mut out, size_of_headers);
    push_u32(&mut out, 0);
    push_u16(&mut out, SUBSYSTEM_WINDOWS_CUI);
    push_u16(&mut out, 0);
    push_u64(&mut out, 0x100000);
    push_u64(&mut out, 0x1000);
    push_u64(&mut out, 0x100000);
    push_u64(&mut out, 0x1000);
    push_u32(&mut out, 0);
    push_u32(&mut out, 16);

    for idx in 0..16 {
        let dir = match idx {
            1 => import_directory,
            12 => iat_directory,
            _ => DataDirectory { rva: 0, size: 0 },
        };
        push_u32(&mut out, dir.rva);
        push_u32(&mut out, dir.size);
    }

    let mut name = [0u8; 8];
    name[..5].copy_from_slice(b".text");
    out.extend_from_slice(&name);
    push_u32(&mut out, size_of_code);
    push_u32(&mut out, SECTION_RVA);
    push_u32(&mut out, size_of_raw_data);
    push_u32(&mut out, pointer_to_raw_data);
    push_u32(&mut out, 0);
    push_u32(&mut out, 0);
    push_u16(&mut out, 0);
    push_u16(&mut out, 0);
    push_u32(&mut out, 0xE0000060);

    while out.len() < size_of_headers as usize {
        out.push(0);
    }
    out.extend_from_slice(&encoded.text);
    while out.len() < (pointer_to_raw_data + size_of_raw_data) as usize {
        out.push(0);
    }
    out
}

fn build_dos_stub() -> [u8; DOS_STUB_SIZE] {
    let mut out = [0u8; DOS_STUB_SIZE];
    out[0] = b'M';
    out[1] = b'Z';
    out[0x3C..0x40].copy_from_slice(&(DOS_STUB_SIZE as u32).to_le_bytes());
    out
}

fn align_up(value: u32, alignment: u32) -> u32 {
    let mask = alignment - 1;
    (value + mask) & !mask
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::x86_64::encode::EncodedProgram;

    fn read_u16(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
    }

    fn read_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn pe_header_tracks_entry_and_data_directories() {
        let image = build_pe_executable(
            &EncodedProgram { text: vec![0xC3] },
            0,
            DataDirectory {
                rva: 0x1234,
                size: 40,
            },
            DataDirectory {
                rva: 0x2345,
                size: 56,
            },
        );

        assert_eq!(&image[0..2], b"MZ");
        let pe_off = read_u32(&image, 0x3C) as usize;
        assert_eq!(&image[pe_off..pe_off + 4], b"PE\0\0");
        assert_eq!(read_u16(&image, pe_off + 4), 0x8664);
        assert_eq!(read_u16(&image, pe_off + 24), 0x20B);
        assert_eq!(read_u32(&image, pe_off + 40), SECTION_RVA);
        assert_eq!(read_u32(&image, pe_off + 24 + 112 + 8), 0x1234);
        assert_eq!(read_u32(&image, pe_off + 24 + 112 + 12), 40);
        assert_eq!(read_u32(&image, pe_off + 24 + 112 + 12 * 8), 0x2345);
        assert_eq!(read_u32(&image, pe_off + 24 + 112 + 12 * 8 + 4), 56);
    }
}
