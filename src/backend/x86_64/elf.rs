use crate::backend::x86_64::encode::EncodedProgram;

const ELF64_EHDR_SIZE: usize = 64;
const ELF64_PHDR_SIZE: usize = 56;
const BASE_VADDR: u64 = 0x400000;
const PAGE_ALIGN: u64 = 0x1000;

pub fn build_elf_executable(encoded: &EncodedProgram) -> Vec<u8> {
    let text_off = (ELF64_EHDR_SIZE + ELF64_PHDR_SIZE) as u64;
    let entry = BASE_VADDR + text_off;

    let file_size = text_off as usize + encoded.text.len();
    let file_size_u64 = file_size as u64;

    let mut out = Vec::with_capacity(file_size);

    // === ELF header ===
    // e_ident
    out.extend_from_slice(&[
        0x7F, b'E', b'L', b'F', // magic
        2,    // ELFCLASS64
        1,    // ELFDATA2LSB
        1,    // EV_CURRENT
        0,    // ELFOSABI_SYSV
        0,    // ABI version
        0, 0, 0, 0, 0, 0, 0, // padding
    ]);

    // e_type = ET_EXEC
    push_u16(&mut out, 2);
    // e_machine = EM_X86_64
    push_u16(&mut out, 62);
    // e_version = EV_CURRENT
    push_u32(&mut out, 1);
    // e_entry
    push_u64(&mut out, entry);
    // e_phoff
    push_u64(&mut out, ELF64_EHDR_SIZE as u64);
    // e_shoff = 0
    push_u64(&mut out, 0);
    // e_flags
    push_u32(&mut out, 0);
    // e_ehsize
    push_u16(&mut out, ELF64_EHDR_SIZE as u16);
    // e_phentsize
    push_u16(&mut out, ELF64_PHDR_SIZE as u16);
    // e_phnum
    push_u16(&mut out, 1);
    // e_shentsize
    push_u16(&mut out, 0);
    // e_shnum
    push_u16(&mut out, 0);
    // e_shstrndx
    push_u16(&mut out, 0);

    // === Program header ===
    // p_type = PT_LOAD
    push_u32(&mut out, 1);
    // p_flags = PF_R | PF_W | PF_X
    push_u32(&mut out, 0x4 | 0x2 | 0x1);
    // p_offset
    push_u64(&mut out, 0);
    // p_vaddr
    push_u64(&mut out, BASE_VADDR);
    // p_paddr
    push_u64(&mut out, BASE_VADDR);
    // p_filesz
    push_u64(&mut out, file_size_u64);
    // p_memsz
    push_u64(&mut out, file_size_u64);
    // p_align
    push_u64(&mut out, PAGE_ALIGN);

    assert_eq!(out.len(), ELF64_EHDR_SIZE + ELF64_PHDR_SIZE);

    // === text ===
    out.extend_from_slice(&encoded.text);
    out
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
