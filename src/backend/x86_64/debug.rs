//! Debug-output module (debug.rs).
//!
//! Provides two complementary listings for the x86_64 backend:
//!
//! - **Scheme A**: `dump_asm_listing` — pretty-prints an `AsmProgram` as
//!   human-readable assembly text (roughly nasm/gas flavoured). Handy for
//!   seeing which instructions the compiler actually emitted.
//!
//! - **Scheme B**: `dump_hex_listing` — encodes the same `AsmProgram` into
//!   machine code and emits an offset + bytes + mnemonic dump, suitable for
//!   byte-by-byte verification of the encoder.
//!
//! Used together they make it practical to debug compiler output without
//! relying on `objdump` or `gdb`.
//!
//! ## Example
//!
//! ```rust,ignore
//! use crate::backend::debug;
//!
//! let asm_program = compile_lir_to_asm(&lir);
//!
//! // Scheme A: write assembly text.
//! let asm_text = debug::dump_asm_listing(&asm_program);
//! std::fs::write("output.asm", &asm_text)?;
//!
//! // Scheme B: write the hex listing.
//! let hex_text = debug::dump_hex_listing(&asm_program);
//! std::fs::write("output.lst", &hex_text)?;
//! ```

use std::fmt::Write;

use crate::backend::asm::{AsmInst, AsmLabel, AsmProgram, Reg64};
use crate::backend::x86_64::encode::encode_program_with_inst_map;

/// Return the lowercase x86_64 mnemonic for a `Reg64`.
///
/// `Reg64::Rax` → `"rax"`, `Reg64::R13` → `"r13"`, etc.
fn reg_name(reg: Reg64) -> &'static str {
    match reg {
        Reg64::Rax => "rax",
        Reg64::Rcx => "rcx",
        Reg64::Rdx => "rdx",
        Reg64::Rbx => "rbx",
        Reg64::Rsp => "rsp",
        Reg64::Rbp => "rbp",
        Reg64::Rsi => "rsi",
        Reg64::Rdi => "rdi",
        Reg64::R8 => "r8",
        Reg64::R9 => "r9",
        Reg64::R10 => "r10",
        Reg64::R11 => "r11",
        Reg64::R12 => "r12",
        Reg64::R13 => "r13",
        Reg64::R14 => "r14",
        Reg64::R15 => "r15",
    }
}

fn format_signed_hex_i32(value: i32) -> String {
    if value < 0 {
        format!("-0x{:x}", value.unsigned_abs())
    } else {
        format!("0x{:x}", value as u32)
    }
}

/// Format a label ID as a stable human-readable name.
///
/// Internal labels (high `u32` values) are given symbolic names, e.g.:
/// - `u32::MAX`     → `"__ensure_tape"`
/// - `u32::MAX - 1` → `"__oom_exit"`
/// - other high values → `"__internal_XXXXXXXX"` (hex)
/// - plain user labels → `"L0"`, `"L1"`, ...
fn label_name(label: AsmLabel) -> String {
    // These constants must stay in sync with `codegen.rs`.
    const INTERNAL_LABEL_ENSURE_TAPE_RAW: u32 = u32::MAX;
    const INTERNAL_LABEL_OOM_EXIT_RAW: u32 = u32::MAX - 1;
    const INTERNAL_LABEL_GROW_LOOP_RAW: u32 = u32::MAX - 2;
    const INTERNAL_LABEL_RESERVED_MIN_RAW: u32 = INTERNAL_LABEL_GROW_LOOP_RAW;

    match label.0 {
        INTERNAL_LABEL_ENSURE_TAPE_RAW => "__ensure_tape".to_string(),
        INTERNAL_LABEL_OOM_EXIT_RAW => "__oom_exit".to_string(),
        INTERNAL_LABEL_GROW_LOOP_RAW => "__grow_loop".to_string(),
        // Fixed-semantics internal labels matched above; remaining high IDs are
        // transient internal labels synthesised during codegen.
        raw if (0xFFFF_0000..INTERNAL_LABEL_RESERVED_MIN_RAW).contains(&raw) => {
            format!("__internal_{:08x}", raw)
        }
        // Plain user label (comes from a LIR `LabelId`).
        raw => format!("L{}", raw),
    }
}

/// **Scheme A**: pretty-print an `AsmProgram` as human-readable assembly.
///
/// Output is roughly nasm/gas flavoured, e.g.:
/// ```text
/// ; === Brainfuck x86_64 Assembly Listing ===
/// ; 42 instructions total
///
/// __ensure_tape:
///     mov     rax, 0x9                    ; syscall: mmap
///     mov     rdi, 0x0
///     ...
/// ```
///
/// # Parameters
/// - `program`: the assembly program to format.
///
/// # Returns
/// The formatted assembly text as a single `String`.
pub fn dump_asm_listing(program: &AsmProgram) -> String {
    let mut out = String::new();

    // Header comment.
    writeln!(out, "; === Brainfuck x86_64 Assembly Listing ===").unwrap();
    writeln!(
        out,
        "; {} instructions (including label pseudo-instructions)",
        program.insts.len()
    )
    .unwrap();
    writeln!(out).unwrap();

    // One pass per instruction.
    for inst in &program.insts {
        format_inst_asm(&mut out, inst);
    }

    out
}

/// Format a single `AsmInst` as assembly text and append it to `out`.
///
/// Labels sit at column 0; ordinary instructions are indented by four spaces.
fn format_inst_asm(out: &mut String, inst: &AsmInst) {
    match inst {
        // Pseudo-instruction: label definition.
        AsmInst::Label(label) => {
            writeln!(out, "{}:", label_name(*label)).unwrap();
        }

        // Data movement.
        AsmInst::MovRegImm64(reg, imm) => {
            // Annotate common Linux syscall numbers inline.
            let comment = match imm {
                0 => " ; sys_read",
                1 => " ; sys_write",
                9 => " ; sys_mmap",
                11 => " ; sys_munmap",
                60 => " ; sys_exit",
                _ => "",
            };
            writeln!(
                out,
                "    mov     {}, 0x{:x}{}",
                reg_name(*reg),
                *imm as u64, // show as unsigned hex
                comment
            )
            .unwrap();
        }

        AsmInst::MovRegReg(dst, src) => {
            writeln!(out, "    mov     {}, {}", reg_name(*dst), reg_name(*src)).unwrap();
        }

        // Arithmetic.
        AsmInst::AddRegImm32(reg, imm) => {
            writeln!(
                out,
                "    add     {}, {}",
                reg_name(*reg),
                format_signed_hex_i32(*imm)
            )
            .unwrap();
        }

        AsmInst::AndRegImm32(reg, imm) => {
            writeln!(
                out,
                "    and     {}, {}",
                reg_name(*reg),
                format_signed_hex_i32(*imm)
            )
            .unwrap();
        }

        AsmInst::AddRegReg(dst, src) => {
            writeln!(out, "    add     {}, {}", reg_name(*dst), reg_name(*src)).unwrap();
        }

        AsmInst::SubRegReg(dst, src) => {
            writeln!(out, "    sub     {}, {}", reg_name(*dst), reg_name(*src)).unwrap();
        }

        // Compare.
        AsmInst::CmpRegReg(lhs, rhs) => {
            writeln!(out, "    cmp     {}, {}", reg_name(*lhs), reg_name(*rhs)).unwrap();
        }

        AsmInst::CmpRegImm32(reg, imm) => {
            writeln!(
                out,
                "    cmp     {}, {}",
                reg_name(*reg),
                format_signed_hex_i32(*imm)
            )
            .unwrap();
        }

        // Shift.
        AsmInst::ShrRegImm8(reg, imm) => {
            writeln!(out, "    shr     {}, {}", reg_name(*reg), imm).unwrap();
        }

        AsmInst::LeaRegMem(dst, base, disp) => {
            writeln!(
                out,
                "    lea     {}, [{}{}{}]",
                reg_name(*dst),
                reg_name(*base),
                if *disp < 0 { " - " } else { " + " },
                format_signed_hex_i32(disp.abs())
            )
            .unwrap();
        }

        AsmInst::LeaRegLabel(dst, label) => {
            writeln!(
                out,
                "    lea     {}, [rel {}]",
                reg_name(*dst),
                label_name(*label)
            )
            .unwrap();
        }

        AsmInst::MovMemReg64(base, disp, src) => {
            writeln!(
                out,
                "    mov     qword [{}{}{}], {}",
                reg_name(*base),
                if *disp < 0 { " - " } else { " + " },
                format_signed_hex_i32(disp.abs()),
                reg_name(*src)
            )
            .unwrap();
        }

        AsmInst::MovRegMem64(dst, base, disp) => {
            writeln!(
                out,
                "    mov     {}, qword [{}{}{}]",
                reg_name(*dst),
                reg_name(*base),
                if *disp < 0 { " - " } else { " + " },
                format_signed_hex_i32(disp.abs())
            )
            .unwrap();
        }

        // Memory-byte ops (Brainfuck core).
        AsmInst::AddMem8Imm8(reg, imm) => {
            // BF `+` / `-`: mutate the current cell.
            writeln!(
                out,
                "    add     byte [{}], 0x{:02x}",
                reg_name(*reg),
                *imm as u8
            )
            .unwrap();
        }

        AsmInst::IncMem8(reg) => {
            // BF single `+`: short form of `add [reg], 1`.
            writeln!(out, "    inc     byte [{}]", reg_name(*reg)).unwrap();
        }

        AsmInst::DecMem8(reg) => {
            // BF single `-`: short form of `add [reg], -1`.
            writeln!(out, "    dec     byte [{}]", reg_name(*reg)).unwrap();
        }

        AsmInst::MovMem8Imm8(reg, imm) => {
            // Optimised cell assignment (e.g. `[-]` folded into `set 0`).
            writeln!(out, "    mov     byte [{}], 0x{:02x}", reg_name(*reg), imm).unwrap();
        }

        AsmInst::AddMem8ImmDisp8(reg, disp, imm) => {
            // B4/C3 displacement add: `add byte [reg + disp], imm`.
            writeln!(
                out,
                "    add     byte [{}{}{}], 0x{:02x}",
                reg_name(*reg),
                if *disp < 0 { " - " } else { " + " },
                format_signed_hex_i32(i32::from(disp.abs())),
                *imm as u8
            )
            .unwrap();
        }

        AsmInst::MovMem8ImmDisp8(reg, disp, imm) => {
            // B4/C3 displacement set: `mov byte [reg + disp], imm`.
            writeln!(
                out,
                "    mov     byte [{}{}{}], 0x{:02x}",
                reg_name(*reg),
                if *disp < 0 { " - " } else { " + " },
                format_signed_hex_i32(i32::from(disp.abs())),
                imm
            )
            .unwrap();
        }

        AsmInst::AddMem8ImmDisp32(reg, disp, imm) => {
            // Disp32 counterpart of AddMem8ImmDisp8 for offsets beyond ±127.
            writeln!(
                out,
                "    add     byte [{}{}{}], 0x{:02x}",
                reg_name(*reg),
                if *disp < 0 { " - " } else { " + " },
                format_signed_hex_i32(disp.unsigned_abs() as i32),
                *imm as u8
            )
            .unwrap();
        }

        AsmInst::MovMem8ImmDisp32(reg, disp, imm) => {
            // Disp32 counterpart of MovMem8ImmDisp8 for offsets beyond ±127.
            writeln!(
                out,
                "    mov     byte [{}{}{}], 0x{:02x}",
                reg_name(*reg),
                if *disp < 0 { " - " } else { " + " },
                format_signed_hex_i32(disp.unsigned_abs() as i32),
                imm
            )
            .unwrap();
        }

        AsmInst::CmpMem8Imm8(reg, imm) => {
            // Zero check used by BF `[` / `]`.
            writeln!(out, "    cmp     byte [{}], 0x{:02x}", reg_name(*reg), imm).unwrap();
        }

        // Conditional jumps.
        AsmInst::Jz(label) => {
            writeln!(out, "    jz      {}", label_name(*label)).unwrap();
        }

        AsmInst::Jnz(label) => {
            writeln!(out, "    jnz     {}", label_name(*label)).unwrap();
        }

        AsmInst::Jb(label) => {
            writeln!(
                out,
                "    jb      {}           ; unsigned below",
                label_name(*label)
            )
            .unwrap();
        }

        AsmInst::Jae(label) => {
            writeln!(
                out,
                "    jae     {}           ; unsigned above or equal",
                label_name(*label)
            )
            .unwrap();
        }

        AsmInst::Jl(label) => {
            writeln!(
                out,
                "    jl      {}           ; signed less",
                label_name(*label)
            )
            .unwrap();
        }

        AsmInst::Jge(label) => {
            writeln!(
                out,
                "    jge     {}           ; signed greater or equal",
                label_name(*label)
            )
            .unwrap();
        }

        // Unconditional jump.
        AsmInst::Jmp(label) => {
            writeln!(out, "    jmp     {}", label_name(*label)).unwrap();
        }

        // Short jumps (rel8 forms emitted by branch relaxation).
        AsmInst::JzShort(label) => {
            writeln!(out, "    jz      {}           ; short", label_name(*label)).unwrap();
        }
        AsmInst::JnzShort(label) => {
            writeln!(out, "    jnz     {}           ; short", label_name(*label)).unwrap();
        }
        AsmInst::JbShort(label) => {
            writeln!(
                out,
                "    jb      {}           ; short, unsigned below",
                label_name(*label)
            )
            .unwrap();
        }
        AsmInst::JaeShort(label) => {
            writeln!(
                out,
                "    jae     {}           ; short, unsigned above or equal",
                label_name(*label)
            )
            .unwrap();
        }
        AsmInst::JlShort(label) => {
            writeln!(
                out,
                "    jl      {}           ; short, signed less",
                label_name(*label)
            )
            .unwrap();
        }
        AsmInst::JgeShort(label) => {
            writeln!(
                out,
                "    jge     {}           ; short, signed greater or equal",
                label_name(*label)
            )
            .unwrap();
        }
        AsmInst::JmpShort(label) => {
            writeln!(out, "    jmp     {}           ; short", label_name(*label)).unwrap();
        }

        // Calls and returns.
        AsmInst::Call(label) => {
            writeln!(out, "    call    {}", label_name(*label)).unwrap();
        }

        AsmInst::CallMemLabel(label) => {
            writeln!(out, "    call    qword [rel {}]", label_name(*label)).unwrap();
        }

        AsmInst::Ret => {
            writeln!(out, "    ret").unwrap();
        }

        // String ops.
        AsmInst::Cld => {
            writeln!(
                out,
                "    cld                         ; clear direction flag"
            )
            .unwrap();
        }

        AsmInst::RepMovsb => {
            writeln!(
                out,
                "    rep movsb                   ; copy rcx bytes: [rsi] -> [rdi]"
            )
            .unwrap();
        }

        // System call.
        AsmInst::Syscall => {
            writeln!(out, "    syscall").unwrap();
        }

        AsmInst::RawBytes(bytes) => {
            writeln!(
                out,
                "    ; <raw {} bytes: precomputed -O3 machine code>",
                bytes.len()
            )
            .unwrap();
        }

        AsmInst::Push(reg) => {
            writeln!(out, "    push    {}", reg_name(*reg)).unwrap();
        }
        AsmInst::Pop(reg) => {
            writeln!(out, "    pop     {}", reg_name(*reg)).unwrap();
        }
        AsmInst::MovzxEbxFromMemR13 => {
            writeln!(out, "    movzx   ebx, byte [r13]").unwrap();
        }
        AsmInst::MovEaxEbx => {
            writeln!(out, "    mov     eax, ebx").unwrap();
        }
        AsmInst::ImulEaxEbxImm32(imm) => {
            writeln!(out, "    imul    eax, ebx, {}", imm).unwrap();
        }
        AsmInst::AddMemR13Al => {
            writeln!(out, "    add     byte [r13], al").unwrap();
        }
        AsmInst::MovAlMemR13 => {
            writeln!(out, "    mov     al, byte [r13]").unwrap();
        }
        AsmInst::MovMemRbxAl => {
            writeln!(out, "    mov     byte [rbx], al").unwrap();
        }
    }
}

/// **Scheme B**: encode an `AsmProgram` into machine code and produce a hex
/// listing with offsets.
///
/// The machine-code bytes come directly from the production encoder in
/// `encode.rs`, so the `.lst` output always matches the real ELF `.text`.
pub fn dump_hex_listing(program: &AsmProgram) -> String {
    let (encoded, inst_map) = encode_program_with_inst_map(program);
    let mut out = String::new();

    writeln!(out, "; === Brainfuck x86_64 Hex Listing ===").unwrap();
    writeln!(
        out,
        "; {} instructions, {} encoded bytes",
        program.insts.len(),
        encoded.text.len()
    )
    .unwrap();
    writeln!(out).unwrap();
    writeln!(out, "{:<9} {:<44} Assembly", "Offset", "Hex").unwrap();
    writeln!(
        out,
        "{} {} {}",
        "-".repeat(9),
        "-".repeat(44),
        "-".repeat(40)
    )
    .unwrap();

    for (inst, encoded_inst) in program.insts.iter().zip(inst_map.iter()) {
        let mut mnemonic_buf = String::new();
        format_inst_asm(&mut mnemonic_buf, inst);
        let mnemonic = mnemonic_buf.trim();

        if matches!(inst, AsmInst::Label(_)) {
            writeln!(out, "         {:<44} {}", "", mnemonic).unwrap();
            continue;
        }

        if encoded_inst.len == 0 {
            continue;
        }

        let bytes = &encoded.text[encoded_inst.offset..encoded_inst.offset + encoded_inst.len];
        for (line_idx, chunk) in bytes.chunks(14).enumerate() {
            let hex_str = chunk
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");

            if line_idx == 0 {
                writeln!(
                    out,
                    "0x{:04x}:  {:<44} {}",
                    encoded_inst.offset, hex_str, mnemonic
                )
                .unwrap();
            } else {
                writeln!(out, "         {}", hex_str).unwrap();
            }
        }
    }

    writeln!(out).unwrap();
    writeln!(out, "; total {} bytes of machine code", encoded.text.len()).unwrap();

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::x86_64::encode::encode_program;

    fn parse_hex_listing_bytes(listing: &str) -> Vec<u8> {
        let mut bytes = Vec::new();

        for line in listing.lines() {
            if let Some(rest) = line.strip_prefix("0x") {
                let Some((_, after_colon)) = rest.split_once(':') else {
                    continue;
                };
                let Some((hex_field, _)) = after_colon.trim_start().split_once("  ") else {
                    continue;
                };
                extend_hex_bytes(&mut bytes, hex_field);
                continue;
            }

            if let Some(rest) = line.strip_prefix("         ") {
                let trimmed = rest.trim();
                if !trimmed.is_empty() && trimmed.bytes().all(is_hex_listing_char) {
                    extend_hex_bytes(&mut bytes, trimmed);
                }
            }
        }

        bytes
    }

    fn extend_hex_bytes(out: &mut Vec<u8>, field: &str) {
        for byte in field.split_whitespace() {
            out.push(u8::from_str_radix(byte, 16).unwrap());
        }
    }

    fn is_hex_listing_char(byte: u8) -> bool {
        byte.is_ascii_hexdigit() || byte == b' '
    }

    #[test]
    fn hex_listing_bytes_match_production_encoder_output() {
        let program = AsmProgram {
            insts: vec![
                AsmInst::MovRegImm64(Reg64::Rax, 9),
                AsmInst::MovMem8Imm8(Reg64::R13, 0),
                AsmInst::Label(AsmLabel(7)),
                AsmInst::CmpMem8Imm8(Reg64::R13, 0),
                AsmInst::Jz(AsmLabel(7)),
                AsmInst::Ret,
            ],
        };

        let encoded = encode_program(&program);
        let listing = dump_hex_listing(&program);

        assert_eq!(parse_hex_listing_bytes(&listing), encoded.text);
    }
}
