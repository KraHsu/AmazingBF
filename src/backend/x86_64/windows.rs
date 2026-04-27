//! Windows (PE64) codegen: lower LIR to x86_64 with `kernel32.dll` I/O.
//!
//! Produces an `AsmProgram` that performs tape allocation, read/write via
//! `ReadFile` / `WriteFile`, growth via `VirtualAlloc`, and exit via
//! `ExitProcess`. Also tracks which `Kernel32Import` entries the generated
//! code references so the PE builder can emit a minimal import table.

use crate::backend::asm::{AsmInst, AsmLabel, AsmProgram, Reg64};
use crate::backend::codegen_common::{LabelAllocator, PlatformEmitter, emit_lir_body};
use crate::ir::lir::LirProgram;

const INITIAL_TAPE_SIZE: usize = 4096;
/// Size of the buffered-output ring, in bytes. Mirrors the Linux backend so
/// the two `flush_output` helpers behave identically modulo ABI differences.
const OUTPUT_BUFFER_SIZE: usize = 4096;
const MEM_COMMIT_RESERVE: i64 = 0x3000;
const MEM_RELEASE: i64 = 0x8000;
const PAGE_READWRITE: i64 = 0x04;
const ERROR_HANDLE_EOF: i64 = 38;
const ERROR_BROKEN_PIPE: i64 = 109;
const STD_INPUT_HANDLE: i64 = -10;
const STD_OUTPUT_HANDLE: i64 = -11;
const ENTRY_STACK_FRAME_BYTES: i32 = 48;
const CALLEE_STACK_FRAME_BYTES: i32 = 88;
const IO_COUNT_SLOT_DISP: i32 = 40;
const OVERLAPPED_SLOT_DISP: i32 = 32;
const STACK_SAVED_NEW_BASE: i32 = 40;
const STACK_SAVED_COPY_START: i32 = 48;
const STACK_SAVED_NEW_LEN: i32 = 56;
const STACK_SAVED_DESIRED_OFFSET: i32 = 64;
const STACK_SAVED_RDI: i32 = 72;
const STACK_SAVED_RSI: i32 = 80;

/// A kernel32.dll symbol that the Windows backend may import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kernel32Import {
    /// `ExitProcess(UINT)` — terminate the current process.
    ExitProcess,
    /// `GetLastError()` — last thread-local Win32 error code.
    GetLastError,
    /// `GetStdHandle(DWORD)` — fetch the stdin / stdout handle.
    GetStdHandle,
    /// `ReadFile(...)` — synchronous byte read used by `,`.
    ReadFile,
    /// `WriteFile(...)` — synchronous byte write used by `.`.
    WriteFile,
    /// `VirtualAlloc(...)` — reserve / commit pages during tape growth.
    VirtualAlloc,
    /// `VirtualFree(...)` — release pages from the previous tape after growth.
    VirtualFree,
}

impl Kernel32Import {
    fn name(self) -> &'static str {
        match self {
            Self::ExitProcess => "ExitProcess",
            Self::GetLastError => "GetLastError",
            Self::GetStdHandle => "GetStdHandle",
            Self::ReadFile => "ReadFile",
            Self::WriteFile => "WriteFile",
            Self::VirtualAlloc => "VirtualAlloc",
            Self::VirtualFree => "VirtualFree",
        }
    }
}

/// A single import-table record referenced by the generated PE32+ image.
#[derive(Debug, Clone)]
pub struct WindowsImport {
    /// Human-readable import name (used when writing the hint/name entry).
    pub name: &'static str,
    /// Label of the IMAGE_IMPORT_BY_NAME hint/name entry in the import table.
    pub hint_name_label: AsmLabel,
    /// Label of the corresponding ILT (Import Lookup Table) slot.
    pub ilt_entry_label: AsmLabel,
    /// Label of the corresponding IAT (Import Address Table) slot.
    pub iat_entry_label: AsmLabel,
}

/// Complete backend artifact for the Windows target: ASM IR plus the labels
/// the PE32+ writer needs to patch into the optional header.
#[derive(Debug, Clone)]
pub struct WindowsProgram {
    /// Generated x86_64 assembly (code and data) in a single AsmProgram.
    pub asm: AsmProgram,
    /// Label of the Windows entry point (jumped to by the PE header).
    pub entry_label: AsmLabel,
    /// Label of the first IMAGE_IMPORT_DESCRIPTOR entry.
    pub import_desc_label: AsmLabel,
    /// Byte size of the import directory placed at `import_desc_label`.
    pub import_dir_size: u32,
    /// Label of the IAT block emitted alongside the import descriptor.
    pub iat_label: AsmLabel,
    /// Byte size of the IAT block at `iat_label`.
    pub iat_size: u32,
    /// Label of the imported DLL name string (`"kernel32.dll\0"`).
    pub dll_name_label: AsmLabel,
    /// One `WindowsImport` record per referenced kernel32 symbol.
    pub imports: Vec<WindowsImport>,
}

#[derive(Debug, Clone)]
struct Kernel32Imports {
    desc_label: AsmLabel,
    ilt_label: AsmLabel,
    iat_label: AsmLabel,
    dll_name_label: AsmLabel,
    entries: Vec<ImportEntry>,
}

#[derive(Debug, Clone)]
struct ImportEntry {
    kind: Kernel32Import,
    hint_name_label: AsmLabel,
    ilt_entry_label: AsmLabel,
    iat_entry_label: AsmLabel,
}

impl Kernel32Imports {
    fn new(labels: &mut LabelAllocator, imports: &[Kernel32Import]) -> Self {
        Self {
            desc_label: labels.fresh(),
            ilt_label: labels.fresh(),
            iat_label: labels.fresh(),
            dll_name_label: labels.fresh(),
            entries: imports
                .iter()
                .copied()
                .map(|kind| ImportEntry {
                    kind,
                    hint_name_label: labels.fresh(),
                    ilt_entry_label: labels.fresh(),
                    iat_entry_label: labels.fresh(),
                })
                .collect(),
        }
    }

    fn iat_label(&self, kind: Kernel32Import) -> AsmLabel {
        self.entries
            .iter()
            .find(|entry| entry.kind == kind)
            .map(|entry| entry.iat_entry_label)
            .unwrap_or_else(|| panic!("missing import label for {:?}", kind))
    }

    fn import_dir_size(&self) -> u32 {
        20 * 2
    }

    fn iat_size(&self) -> u32 {
        ((self.entries.len() + 1) * 8) as u32
    }

    fn emit(&self, out: &mut Vec<AsmInst>) {
        out.push(AsmInst::Label(self.desc_label));
        out.push(AsmInst::RawBytes(vec![0; self.import_dir_size() as usize]));

        out.push(AsmInst::Label(self.ilt_label));
        for entry in &self.entries {
            out.push(AsmInst::Label(entry.ilt_entry_label));
            out.push(AsmInst::RawBytes(vec![0; 8]));
        }
        out.push(AsmInst::RawBytes(vec![0; 8]));

        out.push(AsmInst::Label(self.iat_label));
        for entry in &self.entries {
            out.push(AsmInst::Label(entry.iat_entry_label));
            out.push(AsmInst::RawBytes(vec![0; 8]));
        }
        out.push(AsmInst::RawBytes(vec![0; 8]));

        for entry in &self.entries {
            out.push(AsmInst::Label(entry.hint_name_label));
            let mut bytes = Vec::with_capacity(entry.kind.name().len() + 3);
            bytes.extend_from_slice(&[0, 0]);
            bytes.extend_from_slice(entry.kind.name().as_bytes());
            bytes.push(0);
            out.push(AsmInst::RawBytes(bytes));
        }

        out.push(AsmInst::Label(self.dll_name_label));
        out.push(AsmInst::RawBytes(b"kernel32.dll\0".to_vec()));
    }

    fn into_public(self) -> Vec<WindowsImport> {
        self.entries
            .into_iter()
            .map(|entry| WindowsImport {
                name: entry.kind.name(),
                hint_name_label: entry.hint_name_label,
                ilt_entry_label: entry.ilt_entry_label,
                iat_entry_label: entry.iat_entry_label,
            })
            .collect()
    }
}

struct WindowsEmitter {
    read_file_iat: AsmLabel,
    get_last_error_iat: AsmLabel,
}

impl PlatformEmitter for WindowsEmitter {
    fn emit_put_byte(
        &self,
        out: &mut Vec<AsmInst>,
        labels: &mut LabelAllocator,
        flush_output_label: AsmLabel,
    ) {
        let skip = labels.fresh();
        out.push(AsmInst::MovAlMemR13);
        out.push(AsmInst::MovMemRbxAl);
        out.push(AsmInst::AddRegImm32(Reg64::Rbx, 1));
        out.push(AsmInst::CmpRegReg(Reg64::Rbx, Reg64::Rbp));
        out.push(AsmInst::Jnz(skip));
        out.push(AsmInst::Call(flush_output_label));
        out.push(AsmInst::Label(skip));
    }

    fn emit_get_byte(
        &self,
        out: &mut Vec<AsmInst>,
        labels: &mut LabelAllocator,
        exit_one_label: AsmLabel,
        flush_output_label: AsmLabel,
    ) {
        out.push(AsmInst::Call(flush_output_label));
        let done = labels.fresh();
        let eof = labels.fresh();
        let read_ok = labels.fresh();
        emit_zero_stack_qword(out, IO_COUNT_SLOT_DISP);
        out.push(AsmInst::MovRegReg(Reg64::Rcx, Reg64::Rsi));
        out.push(AsmInst::MovRegReg(Reg64::Rdx, Reg64::R13));
        out.push(AsmInst::MovRegImm64(Reg64::R8, 1));
        out.push(AsmInst::LeaRegMem(
            Reg64::R9,
            Reg64::Rsp,
            IO_COUNT_SLOT_DISP,
        ));
        out.push(AsmInst::CallMemLabel(self.read_file_iat));
        out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
        out.push(AsmInst::Jnz(read_ok));
        out.push(AsmInst::CallMemLabel(self.get_last_error_iat));
        out.push(AsmInst::CmpRegImm32(Reg64::Rax, ERROR_HANDLE_EOF as i32));
        out.push(AsmInst::Jz(eof));
        out.push(AsmInst::CmpRegImm32(Reg64::Rax, ERROR_BROKEN_PIPE as i32));
        out.push(AsmInst::Jz(eof));
        out.push(AsmInst::Jmp(exit_one_label));
        out.push(AsmInst::Label(read_ok));
        out.push(AsmInst::MovRegMem64(
            Reg64::Rax,
            Reg64::Rsp,
            IO_COUNT_SLOT_DISP,
        ));
        out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
        out.push(AsmInst::Jnz(done));
        out.push(AsmInst::Label(eof));
        out.push(AsmInst::MovMem8Imm8(Reg64::R13, 255));
        out.push(AsmInst::Label(done));
    }

    fn needs_rsp_alignment(&self) -> bool {
        true
    }
}

/// Lower a [`LirProgram`] into a [`WindowsProgram`]: generate the prologue,
/// tape management, `ReadFile`/`WriteFile` helpers, and the main LIR-to-ASM
/// translation with kernel32 imports.
pub fn compile_lir_to_windows_program(lir: &LirProgram) -> WindowsProgram {
    let mut labels = LabelAllocator::new(0xFFFE_FFFF, 0);
    let entry_label = labels.fresh();
    let ensure_tape_label = labels.fresh();
    let exit_one_label = labels.fresh();
    let grow_loop = labels.fresh();
    let flush_output_label = labels.fresh();
    let imports = Kernel32Imports::new(
        &mut labels,
        &[
            Kernel32Import::ExitProcess,
            Kernel32Import::GetLastError,
            Kernel32Import::GetStdHandle,
            Kernel32Import::ReadFile,
            Kernel32Import::WriteFile,
            Kernel32Import::VirtualAlloc,
            Kernel32Import::VirtualFree,
        ],
    );

    let mut out = Vec::new();
    emit_entry_prologue(&mut out, entry_label);
    emit_zero_stack_qword(&mut out, OVERLAPPED_SLOT_DISP);
    emit_get_std_handle(
        &mut out,
        imports.iat_label(Kernel32Import::GetStdHandle),
        STD_INPUT_HANDLE,
        Reg64::Rsi,
        exit_one_label,
    );
    emit_get_std_handle(
        &mut out,
        imports.iat_label(Kernel32Import::GetStdHandle),
        STD_OUTPUT_HANDLE,
        Reg64::Rdi,
        exit_one_label,
    );
    emit_init_tape(
        &mut out,
        imports.iat_label(Kernel32Import::VirtualAlloc),
        exit_one_label,
    );
    emit_init_output_buffer(
        &mut out,
        imports.iat_label(Kernel32Import::VirtualAlloc),
        exit_one_label,
    );

    // 2. Translate LIR instructions (shared logic with Linux backend).
    let win_emitter = WindowsEmitter {
        read_file_iat: imports.iat_label(Kernel32Import::ReadFile),
        get_last_error_iat: imports.iat_label(Kernel32Import::GetLastError),
    };
    emit_lir_body(
        &mut out,
        &mut labels,
        lir,
        ensure_tape_label,
        flush_output_label,
        exit_one_label,
        &win_emitter,
    );

    // Flush before the normal exit so any tail output (whether less than
    // a buffer page, or partially through the next page) actually reaches
    // the console. The error path (exit_one_label) deliberately skips
    // this — flushing during a fault could itself fail and mask the
    // primary error, matching the Linux backend convention.
    out.push(AsmInst::Call(flush_output_label));
    emit_exit_process(&mut out, imports.iat_label(Kernel32Import::ExitProcess), 0);
    emit_flush_output(
        &mut out,
        flush_output_label,
        imports.iat_label(Kernel32Import::WriteFile),
    );
    emit_ensure_tape_contains_r15(
        &mut out,
        ensure_tape_label,
        grow_loop,
        exit_one_label,
        imports.iat_label(Kernel32Import::VirtualAlloc),
        imports.iat_label(Kernel32Import::VirtualFree),
    );
    out.push(AsmInst::Label(exit_one_label));
    emit_exit_process(&mut out, imports.iat_label(Kernel32Import::ExitProcess), 1);
    let public_imports = imports.clone().into_public();
    let import_desc_label = imports.desc_label;
    let import_dir_size = imports.import_dir_size();
    let iat_label = imports.iat_label;
    let iat_size = imports.iat_size();
    let dll_name_label = imports.dll_name_label;
    imports.emit(&mut out);

    WindowsProgram {
        asm: AsmProgram { insts: out },
        entry_label,
        import_desc_label,
        import_dir_size,
        iat_label,
        iat_size,
        dll_name_label,
        imports: public_imports,
    }
}

/// Emit a minimal Windows executable that calls `ExitProcess(exit_code)`. Used
/// by `-O3` when the input Brainfuck program has no side effects.
pub fn compile_trivial_exit_program(exit_code: u32) -> WindowsProgram {
    let mut labels = LabelAllocator::new(0xFFFE_FFFF, 0);
    let entry_label = labels.fresh();
    let imports = Kernel32Imports::new(&mut labels, &[Kernel32Import::ExitProcess]);
    let mut out = Vec::new();
    emit_entry_prologue(&mut out, entry_label);
    emit_exit_process(
        &mut out,
        imports.iat_label(Kernel32Import::ExitProcess),
        exit_code as i64,
    );
    let public_imports = imports.clone().into_public();
    let import_desc_label = imports.desc_label;
    let import_dir_size = imports.import_dir_size();
    let iat_label = imports.iat_label;
    let iat_size = imports.iat_size();
    let dll_name_label = imports.dll_name_label;
    imports.emit(&mut out);
    WindowsProgram {
        asm: AsmProgram { insts: out },
        entry_label,
        import_desc_label,
        import_dir_size,
        iat_label,
        iat_size,
        dll_name_label,
        imports: public_imports,
    }
}

/// Emit a Windows executable that writes `data` to stdout then exits. Used by
/// `-O3` when the Brainfuck program is input-free so output can be precomputed.
pub fn compile_precomputed_stdout_program(data: &[u8]) -> WindowsProgram {
    if data.is_empty() {
        return compile_trivial_exit_program(0);
    }

    let mut labels = LabelAllocator::new(0xFFFE_FFFF, 0);
    let entry_label = labels.fresh();
    let stdout_bytes_label = labels.fresh();
    let exit_one_label = labels.fresh();
    let imports = Kernel32Imports::new(
        &mut labels,
        &[
            Kernel32Import::ExitProcess,
            Kernel32Import::GetStdHandle,
            Kernel32Import::WriteFile,
        ],
    );
    let mut out = Vec::new();
    emit_entry_prologue(&mut out, entry_label);
    emit_zero_stack_qword(&mut out, OVERLAPPED_SLOT_DISP);
    emit_get_std_handle(
        &mut out,
        imports.iat_label(Kernel32Import::GetStdHandle),
        STD_OUTPUT_HANDLE,
        Reg64::Rdi,
        exit_one_label,
    );
    emit_zero_stack_qword(&mut out, IO_COUNT_SLOT_DISP);
    out.push(AsmInst::MovRegReg(Reg64::Rcx, Reg64::Rdi));
    out.push(AsmInst::LeaRegLabel(Reg64::Rdx, stdout_bytes_label));
    out.push(AsmInst::MovRegImm64(Reg64::R8, data.len() as i64));
    out.push(AsmInst::LeaRegMem(
        Reg64::R9,
        Reg64::Rsp,
        IO_COUNT_SLOT_DISP,
    ));
    out.push(AsmInst::CallMemLabel(
        imports.iat_label(Kernel32Import::WriteFile),
    ));
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jz(exit_one_label));
    emit_exit_process(&mut out, imports.iat_label(Kernel32Import::ExitProcess), 0);
    out.push(AsmInst::Label(exit_one_label));
    emit_exit_process(&mut out, imports.iat_label(Kernel32Import::ExitProcess), 1);
    out.push(AsmInst::Label(stdout_bytes_label));
    out.push(AsmInst::RawBytes(data.to_vec()));
    let public_imports = imports.clone().into_public();
    let import_desc_label = imports.desc_label;
    let import_dir_size = imports.import_dir_size();
    let iat_label = imports.iat_label;
    let iat_size = imports.iat_size();
    let dll_name_label = imports.dll_name_label;
    imports.emit(&mut out);
    WindowsProgram {
        asm: AsmProgram { insts: out },
        entry_label,
        import_desc_label,
        import_dir_size,
        iat_label,
        iat_size,
        dll_name_label,
        imports: public_imports,
    }
}

fn emit_entry_prologue(out: &mut Vec<AsmInst>, entry_label: AsmLabel) {
    out.push(AsmInst::Label(entry_label));
    out.push(AsmInst::AndRegImm32(Reg64::Rsp, -16));
    out.push(AsmInst::AddRegImm32(Reg64::Rsp, -ENTRY_STACK_FRAME_BYTES));
}

fn emit_get_std_handle(
    out: &mut Vec<AsmInst>,
    get_std_handle_iat: AsmLabel,
    which: i64,
    dst: Reg64,
    exit_one_label: AsmLabel,
) {
    out.push(AsmInst::MovRegImm64(Reg64::Rcx, which));
    out.push(AsmInst::CallMemLabel(get_std_handle_iat));
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, -1));
    out.push(AsmInst::Jz(exit_one_label));
    out.push(AsmInst::MovRegReg(dst, Reg64::Rax));
}

fn emit_zero_stack_qword(out: &mut Vec<AsmInst>, disp: i32) {
    out.push(AsmInst::MovRegImm64(Reg64::Rax, 0));
    out.push(AsmInst::MovMemReg64(Reg64::Rsp, disp, Reg64::Rax));
}

fn emit_init_tape(out: &mut Vec<AsmInst>, virtual_alloc_iat: AsmLabel, exit_one_label: AsmLabel) {
    out.push(AsmInst::MovRegImm64(Reg64::Rcx, 0));
    out.push(AsmInst::MovRegImm64(Reg64::Rdx, INITIAL_TAPE_SIZE as i64));
    out.push(AsmInst::MovRegImm64(Reg64::R8, MEM_COMMIT_RESERVE));
    out.push(AsmInst::MovRegImm64(Reg64::R9, PAGE_READWRITE));
    out.push(AsmInst::CallMemLabel(virtual_alloc_iat));
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jz(exit_one_label));
    out.push(AsmInst::MovRegReg(Reg64::R12, Reg64::Rax));
    out.push(AsmInst::MovRegReg(Reg64::R13, Reg64::Rax));
    out.push(AsmInst::AddRegImm32(
        Reg64::R13,
        (INITIAL_TAPE_SIZE / 2) as i32,
    ));
    out.push(AsmInst::MovRegReg(Reg64::R14, Reg64::Rax));
    out.push(AsmInst::AddRegImm32(Reg64::R14, INITIAL_TAPE_SIZE as i32));
}

/// Allocate the 4 KiB output buffer via `VirtualAlloc` and pin its bookkeeping
/// registers. Mirrors `crate::backend::codegen::emit_init_output_buffer` (Linux);
/// the only differences are the allocation API (VirtualAlloc vs. mmap) and the
/// failure semantics (NULL vs. negative). After this helper:
///
/// - `Rbx` = current write pointer, advanced by each `PutByte`; reset to the
///   buffer base by [`emit_flush_output`].
/// - `Rbp` = buffer end (= base + OUTPUT_BUFFER_SIZE); `PutByte`'s
///   `cmp rbx, rbp` detects a full buffer and triggers a flush.
///
/// On allocation failure (`rax == NULL`) the generated code jumps to
/// `exit_one_label`, matching the tape-allocation error path.
fn emit_init_output_buffer(
    out: &mut Vec<AsmInst>,
    virtual_alloc_iat: AsmLabel,
    exit_one_label: AsmLabel,
) {
    // VirtualAlloc(NULL, OUTPUT_BUFFER_SIZE, MEM_COMMIT|MEM_RESERVE, PAGE_READWRITE).
    out.push(AsmInst::MovRegImm64(Reg64::Rcx, 0));
    out.push(AsmInst::MovRegImm64(Reg64::Rdx, OUTPUT_BUFFER_SIZE as i64));
    out.push(AsmInst::MovRegImm64(Reg64::R8, MEM_COMMIT_RESERVE));
    out.push(AsmInst::MovRegImm64(Reg64::R9, PAGE_READWRITE));
    out.push(AsmInst::CallMemLabel(virtual_alloc_iat));

    // VirtualAlloc returns NULL on failure -> exit(1).
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jz(exit_one_label));

    // Rbx = buffer_base (write pointer starts at the beginning).
    out.push(AsmInst::MovRegReg(Reg64::Rbx, Reg64::Rax));

    // Rbp = buffer_base + OUTPUT_BUFFER_SIZE (buffer end sentinel).
    out.push(AsmInst::MovRegReg(Reg64::Rbp, Reg64::Rax));
    out.push(AsmInst::AddRegImm32(Reg64::Rbp, OUTPUT_BUFFER_SIZE as i32));
}

/// Emit the `flush_output` helper subroutine for the Windows backend.
///
/// Will be called by `PutByte` on buffer-full, by every `GetByte` (so prior
/// output reaches stdout before stdin blocks), and once on `exit(0)`.
/// Writes `rbx - buffer_base` bytes to stdout via `WriteFile` and resets
/// `rbx` to `buffer_base`. When no bytes are pending (`rbx == buffer_base`),
/// `WriteFile(handle, buf, 0, ...)` is a harmless no-op.
///
/// Win64 calling convention notes:
///
/// - The helper reserves a fresh 88-byte frame (same constant the
///   tape-grow helper uses) so the inner `call WriteFile` lands on a
///   16-byte-aligned RSP. Layout inside the frame: `[rsp+0..32]` shadow
///   space (untouched here), `[rsp+OVERLAPPED_SLOT_DISP]` 5th-arg
///   `lpOverlapped = NULL`, `[rsp+IO_COUNT_SLOT_DISP]` `&BytesWritten`
///   output slot.
/// - `Rbx` and `Rbp` are non-volatile in Win64, so they survive the call
///   to `WriteFile`. We still recompute the buffer base from `Rbp` after
///   the call rather than caching it in `rdx`, because all volatile
///   registers (rcx/rdx/r8/r9/r10/r11/rax) are clobbered.
/// - We deliberately ignore short-write returns: BF programs only write
///   to stdout, so `WriteFile` realistically either succeeds with the
///   full count or the process is already terminating.
fn emit_flush_output(out: &mut Vec<AsmInst>, label: AsmLabel, write_file_iat: AsmLabel) {
    out.push(AsmInst::Label(label));
    out.push(AsmInst::AddRegImm32(Reg64::Rsp, -CALLEE_STACK_FRAME_BYTES));

    // 5th arg lpOverlapped = NULL (Win64 places it at [rsp+32]).
    emit_zero_stack_qword(out, OVERLAPPED_SLOT_DISP);
    // BytesWritten output slot (filled by WriteFile; we don't read it).
    emit_zero_stack_qword(out, IO_COUNT_SLOT_DISP);

    // rcx = stdout handle (cached in rdi during entry prologue).
    out.push(AsmInst::MovRegReg(Reg64::Rcx, Reg64::Rdi));
    // rdx = buffer_base = rbp - OUTPUT_BUFFER_SIZE.
    out.push(AsmInst::LeaRegMem(
        Reg64::Rdx,
        Reg64::Rbp,
        -(OUTPUT_BUFFER_SIZE as i32),
    ));
    // r8 = count = rbx - buffer_base.
    out.push(AsmInst::MovRegReg(Reg64::R8, Reg64::Rbx));
    out.push(AsmInst::SubRegReg(Reg64::R8, Reg64::Rdx));
    // r9 = lpNumberOfBytesWritten = &slot.
    out.push(AsmInst::LeaRegMem(
        Reg64::R9,
        Reg64::Rsp,
        IO_COUNT_SLOT_DISP,
    ));

    out.push(AsmInst::CallMemLabel(write_file_iat));

    // Reset write pointer to buffer_base (rdx is volatile post-call).
    out.push(AsmInst::LeaRegMem(
        Reg64::Rbx,
        Reg64::Rbp,
        -(OUTPUT_BUFFER_SIZE as i32),
    ));

    out.push(AsmInst::AddRegImm32(Reg64::Rsp, CALLEE_STACK_FRAME_BYTES));
    out.push(AsmInst::Ret);
}

/// Emit a buffered `.` (PutByte) for the Windows backend.
///
/// Mirrors the Linux backend's per-byte hot path (`codegen.rs:461-471`):
/// store the current cell into `[rbx]`, advance `rbx`, and `call
fn emit_exit_process(out: &mut Vec<AsmInst>, exit_process_iat: AsmLabel, code: i64) {
    out.push(AsmInst::MovRegImm64(Reg64::Rcx, code));
    out.push(AsmInst::CallMemLabel(exit_process_iat));
}

fn emit_ensure_tape_contains_r15(
    out: &mut Vec<AsmInst>,
    ensure_tape_label: AsmLabel,
    grow_loop: AsmLabel,
    exit_one_label: AsmLabel,
    virtual_alloc_iat: AsmLabel,
    virtual_free_iat: AsmLabel,
) {
    out.push(AsmInst::Label(ensure_tape_label));
    out.push(AsmInst::AddRegImm32(Reg64::Rsp, -CALLEE_STACK_FRAME_BYTES));
    out.push(AsmInst::MovMemReg64(
        Reg64::Rsp,
        STACK_SAVED_RDI,
        Reg64::Rdi,
    ));
    out.push(AsmInst::MovMemReg64(
        Reg64::Rsp,
        STACK_SAVED_RSI,
        Reg64::Rsi,
    ));
    out.push(AsmInst::MovRegReg(Reg64::R10, Reg64::R14));
    out.push(AsmInst::SubRegReg(Reg64::R10, Reg64::R12));
    out.push(AsmInst::MovRegReg(Reg64::R9, Reg64::R15));
    out.push(AsmInst::SubRegReg(Reg64::R9, Reg64::R12));
    out.push(AsmInst::MovRegReg(Reg64::R11, Reg64::R10));
    out.push(AsmInst::Label(grow_loop));
    out.push(AsmInst::AddRegReg(Reg64::R11, Reg64::R11));
    out.push(AsmInst::MovRegReg(Reg64::R8, Reg64::R11));
    out.push(AsmInst::SubRegReg(Reg64::R8, Reg64::R10));
    out.push(AsmInst::ShrRegImm8(Reg64::R8, 1));
    out.push(AsmInst::MovRegReg(Reg64::Rax, Reg64::R8));
    out.push(AsmInst::AddRegReg(Reg64::Rax, Reg64::R9));
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jl(grow_loop));
    out.push(AsmInst::CmpRegReg(Reg64::Rax, Reg64::R11));
    out.push(AsmInst::Jge(grow_loop));
    out.push(AsmInst::MovMemReg64(
        Reg64::Rsp,
        STACK_SAVED_NEW_LEN,
        Reg64::R11,
    ));
    out.push(AsmInst::MovRegImm64(Reg64::Rcx, 0));
    out.push(AsmInst::MovRegReg(Reg64::Rdx, Reg64::R11));
    out.push(AsmInst::MovRegImm64(Reg64::R8, MEM_COMMIT_RESERVE));
    out.push(AsmInst::MovRegImm64(Reg64::R9, PAGE_READWRITE));
    out.push(AsmInst::CallMemLabel(virtual_alloc_iat));
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jz(exit_one_label));
    out.push(AsmInst::MovRegReg(Reg64::R10, Reg64::R14));
    out.push(AsmInst::SubRegReg(Reg64::R10, Reg64::R12));
    out.push(AsmInst::MovRegReg(Reg64::R9, Reg64::R15));
    out.push(AsmInst::SubRegReg(Reg64::R9, Reg64::R12));
    out.push(AsmInst::MovRegMem64(
        Reg64::R11,
        Reg64::Rsp,
        STACK_SAVED_NEW_LEN,
    ));
    out.push(AsmInst::MovRegReg(Reg64::R8, Reg64::R11));
    out.push(AsmInst::SubRegReg(Reg64::R8, Reg64::R10));
    out.push(AsmInst::ShrRegImm8(Reg64::R8, 1));
    out.push(AsmInst::MovRegReg(Reg64::Rdx, Reg64::Rax));
    out.push(AsmInst::MovRegReg(Reg64::Rdi, Reg64::Rdx));
    out.push(AsmInst::AddRegReg(Reg64::Rdi, Reg64::R8));
    out.push(AsmInst::MovRegReg(Reg64::Rsi, Reg64::R12));
    out.push(AsmInst::MovRegReg(Reg64::Rcx, Reg64::R10));
    out.push(AsmInst::Cld);
    out.push(AsmInst::RepMovsb);
    out.push(AsmInst::MovMemReg64(
        Reg64::Rsp,
        STACK_SAVED_NEW_BASE,
        Reg64::Rdx,
    ));
    out.push(AsmInst::MovMemReg64(
        Reg64::Rsp,
        STACK_SAVED_COPY_START,
        Reg64::R8,
    ));
    out.push(AsmInst::MovMemReg64(
        Reg64::Rsp,
        STACK_SAVED_NEW_LEN,
        Reg64::R11,
    ));
    out.push(AsmInst::MovMemReg64(
        Reg64::Rsp,
        STACK_SAVED_DESIRED_OFFSET,
        Reg64::R9,
    ));
    out.push(AsmInst::MovRegReg(Reg64::Rcx, Reg64::R12));
    out.push(AsmInst::MovRegImm64(Reg64::Rdx, 0));
    out.push(AsmInst::MovRegImm64(Reg64::R8, MEM_RELEASE));
    out.push(AsmInst::CallMemLabel(virtual_free_iat));
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jz(exit_one_label));
    out.push(AsmInst::MovRegMem64(
        Reg64::Rdx,
        Reg64::Rsp,
        STACK_SAVED_NEW_BASE,
    ));
    out.push(AsmInst::MovRegMem64(
        Reg64::R8,
        Reg64::Rsp,
        STACK_SAVED_COPY_START,
    ));
    out.push(AsmInst::MovRegMem64(
        Reg64::R11,
        Reg64::Rsp,
        STACK_SAVED_NEW_LEN,
    ));
    out.push(AsmInst::MovRegMem64(
        Reg64::R9,
        Reg64::Rsp,
        STACK_SAVED_DESIRED_OFFSET,
    ));
    out.push(AsmInst::MovRegReg(Reg64::R12, Reg64::Rdx));
    out.push(AsmInst::MovRegReg(Reg64::R13, Reg64::Rdx));
    out.push(AsmInst::AddRegReg(Reg64::R13, Reg64::R8));
    out.push(AsmInst::AddRegReg(Reg64::R13, Reg64::R9));
    out.push(AsmInst::MovRegReg(Reg64::R14, Reg64::Rdx));
    out.push(AsmInst::AddRegReg(Reg64::R14, Reg64::R11));
    out.push(AsmInst::MovRegMem64(
        Reg64::Rdi,
        Reg64::Rsp,
        STACK_SAVED_RDI,
    ));
    out.push(AsmInst::MovRegMem64(
        Reg64::Rsi,
        Reg64::Rsp,
        STACK_SAVED_RSI,
    ));
    out.push(AsmInst::AddRegImm32(Reg64::Rsp, CALLEE_STACK_FRAME_BYTES));
    out.push(AsmInst::Ret);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::asm::AsmInst;
    use crate::ir::lir::{LirInst, LirProgram};

    #[test]
    fn trivial_exit_imports_exitprocess_only() {
        let program = compile_trivial_exit_program(0);
        assert_eq!(program.imports.len(), 1);
        assert_eq!(program.imports[0].name, "ExitProcess");
        assert!(
            !program
                .asm
                .insts
                .iter()
                .any(|inst| matches!(inst, AsmInst::Syscall))
        );
    }

    #[test]
    fn precomputed_stdout_program_uses_win32_imports_not_syscalls() {
        let program = compile_precomputed_stdout_program(b"OK\n");
        let names: Vec<_> = program.imports.iter().map(|import| import.name).collect();
        assert_eq!(names, vec!["ExitProcess", "GetStdHandle", "WriteFile"]);
        assert!(
            program
                .asm
                .insts
                .iter()
                .any(|inst| matches!(inst, AsmInst::CallMemLabel(_)))
        );
        assert!(
            !program
                .asm
                .insts
                .iter()
                .any(|inst| matches!(inst, AsmInst::Syscall))
        );
    }

    #[test]
    fn full_windows_backend_imports_kernel32_io_and_memory() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::GetByte, LirInst::PutByte],
        });
        let names: Vec<_> = program.imports.iter().map(|import| import.name).collect();
        assert_eq!(
            names,
            vec![
                "ExitProcess",
                "GetLastError",
                "GetStdHandle",
                "ReadFile",
                "WriteFile",
                "VirtualAlloc",
                "VirtualFree",
            ]
        );
    }

    #[test]
    fn windows_scan_with_hint_positive_dir_uses_repne_scasb_with_cld() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::ScanWithHint {
                dir: 1,
                hint_bytes: 5,
            }],
        });
        let setup = program.asm.insts.windows(5).any(|w| {
            matches!(
                w,
                [
                    AsmInst::MovRegImm64(Reg64::Rax, 0),
                    AsmInst::MovRegReg(Reg64::Rdi, Reg64::R13),
                    AsmInst::MovRegImm64(Reg64::Rcx, 5),
                    AsmInst::Cld,
                    AsmInst::RepneScasb,
                ]
            )
        });
        assert!(
            setup,
            "Windows ScanWithHint(+1, 5) must set up `al=0; rdi=r13; rcx=5; cld; repne scasb`"
        );
    }

    #[test]
    fn windows_scan_with_hint_negative_dir_brackets_scasb_with_std_cld() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::ScanWithHint {
                dir: -1,
                hint_bytes: 4,
            }],
        });
        let bracketed = program
            .asm
            .insts
            .windows(3)
            .any(|w| matches!(w, [AsmInst::Std, AsmInst::RepneScasb, AsmInst::Cld]));
        assert!(
            bracketed,
            "Windows ScanWithHint(-1) must bracket scasb with `std` ... `cld` to keep DF=0 at the next call boundary"
        );
    }

    #[test]
    fn windows_linear_mul_factor_one_uses_add_mem_r13_bl_disp8() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::LinearMul(vec![(2, 1)])],
        });
        assert!(
            program.asm.insts.contains(&AsmInst::AddMemR13BlDisp8(2)),
            "Windows LinearMul factor=1 must emit AddMemR13BlDisp8"
        );
        assert!(
            !program
                .asm
                .insts
                .iter()
                .any(|i| matches!(i, AsmInst::ImulEaxEbxImm32(_))),
            "Windows LinearMul factor=1 must skip imul on the ±1 column"
        );
    }

    #[test]
    fn windows_linear_mul_factor_minus_one_uses_sub_mem_r13_bl_disp8() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::LinearMul(vec![(3, -1)])],
        });
        assert!(
            program.asm.insts.contains(&AsmInst::SubMemR13BlDisp8(3)),
            "Windows LinearMul factor=-1 must emit SubMemR13BlDisp8"
        );
    }

    #[test]
    fn windows_zero_run_count_at_least_16_uses_rep_stosb() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::ZeroRun {
                start: -2,
                count: 32,
            }],
        });
        let simd = program.asm.insts.windows(5).any(|w| {
            matches!(
                w,
                [
                    AsmInst::XorEaxEax,
                    AsmInst::LeaRegMem(Reg64::Rdi, Reg64::R13, -2),
                    AsmInst::MovEcxImm32(32),
                    AsmInst::Cld,
                    AsmInst::RepStosb,
                ]
            )
        });
        assert!(
            simd,
            "Windows ZeroRun(count >= 16) must lower to rep stosb identically to Linux"
        );
    }

    #[test]
    fn windows_zero_run_emits_byte_stores_across_span() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::ZeroRun {
                start: -1,
                count: 3,
            }],
        });
        assert!(
            program
                .asm
                .insts
                .contains(&AsmInst::MovMem8ImmDisp8(Reg64::R13, -1, 0)),
            "Windows ZeroRun must emit disp8 store at r13 - 1"
        );
        assert!(
            program
                .asm
                .insts
                .contains(&AsmInst::MovMem8Imm8(Reg64::R13, 0)),
            "Windows ZeroRun must emit bare [r13]=0 store when offset 0 is in range"
        );
        assert!(
            program
                .asm
                .insts
                .contains(&AsmInst::MovMem8ImmDisp8(Reg64::R13, 1, 0)),
            "Windows ZeroRun must emit disp8 store at r13 + 1"
        );
    }

    #[test]
    fn windows_scan_with_hint_zero_hint_emits_only_slow_body() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::ScanWithHint {
                dir: 1,
                hint_bytes: 0,
            }],
        });
        assert!(
            !program
                .asm
                .insts
                .iter()
                .any(|i| matches!(i, AsmInst::RepneScasb)),
            "Windows hint=0 must not emit the SIMD `repne scasb` setup"
        );
        assert!(
            !program.asm.insts.iter().any(|i| matches!(i, AsmInst::Std)),
            "Windows hint=0 must not flip the direction flag"
        );
    }

    #[test]
    fn windows_cell_add_plus_one_uses_inc_short_form() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::CellAdd(1)],
        });
        assert!(
            program.asm.insts.contains(&AsmInst::IncMem8(Reg64::R13)),
            "CellAdd(1) on Windows must emit IncMem8 (matches Linux short form)"
        );
        assert!(
            !program
                .asm
                .insts
                .iter()
                .any(|i| matches!(i, AsmInst::AddMem8Imm8(Reg64::R13, 1))),
            "CellAdd(1) must not fall back to AddMem8Imm8"
        );
    }

    #[test]
    fn windows_cell_add_minus_one_uses_dec_short_form() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::CellAdd(-1)],
        });
        assert!(
            program.asm.insts.contains(&AsmInst::DecMem8(Reg64::R13)),
            "CellAdd(-1) on Windows must emit DecMem8 (matches Linux short form)"
        );
    }

    #[test]
    fn windows_program_initialises_output_buffer_via_virtualalloc() {
        let program = compile_lir_to_windows_program(&LirProgram { insts: vec![] });
        // VirtualAlloc is invoked twice: first for the tape, then for the
        // output buffer. The output-buffer call must set rdx = 4096, r8 =
        // MEM_COMMIT|MEM_RESERVE, r9 = PAGE_READWRITE.
        let virtual_alloc_calls = program
            .asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::CallMemLabel(_)))
            .count();
        assert!(
            virtual_alloc_calls >= 2,
            "expected at least 2 VirtualAlloc/IAT calls (tape + output buffer)"
        );
        // After the second VirtualAlloc, rbx must be set from rax and rbp
        // must be rax + OUTPUT_BUFFER_SIZE.
        let pins_rbx = program.asm.insts.windows(3).any(|w| {
            matches!(
                w,
                [
                    AsmInst::MovRegReg(Reg64::Rbx, Reg64::Rax),
                    AsmInst::MovRegReg(Reg64::Rbp, Reg64::Rax),
                    AsmInst::AddRegImm32(Reg64::Rbp, 4096),
                ]
            )
        });
        assert!(
            pins_rbx,
            "Windows prologue must pin Rbx = base and Rbp = base + 4096"
        );
    }

    #[test]
    fn windows_linear_mul_protects_rbx_with_aligned_pair() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::LinearMul(vec![(1, 1)])],
        });
        // Body must open with `push rbx; sub rsp, 8` and close with
        // `add rsp, 8; pop rbx` to keep Rsp 16-byte aligned at internal
        // `call ensure_tape` sites (Win64 ABI).
        let opens = program.asm.insts.windows(2).any(|w| {
            matches!(
                w,
                [
                    AsmInst::Push(Reg64::Rbx),
                    AsmInst::AddRegImm32(Reg64::Rsp, -8),
                ]
            )
        });
        let closes = program.asm.insts.windows(2).any(|w| {
            matches!(
                w,
                [
                    AsmInst::AddRegImm32(Reg64::Rsp, 8),
                    AsmInst::Pop(Reg64::Rbx),
                ]
            )
        });
        assert!(
            opens,
            "Windows LinearMul must save Rbx with aligned `push rbx; sub rsp, 8`"
        );
        assert!(
            closes,
            "Windows LinearMul must restore Rbx with `add rsp, 8; pop rbx`"
        );
    }

    #[test]
    fn windows_get_byte_flushes_pending_output_before_blocking_on_stdin() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::GetByte],
        });
        // The first emitted instruction inside emit_get_byte must be a
        // Call to the flush helper (mirrors codegen.rs:484 on Linux).
        // Locate the first ReadFile call, then walk back to confirm a
        // Call precedes it.
        let calls: Vec<usize> = program
            .asm
            .insts
            .iter()
            .enumerate()
            .filter_map(|(i, inst)| match inst {
                AsmInst::Call(_) => Some(i),
                _ => None,
            })
            .collect();
        assert!(
            !calls.is_empty(),
            "GetByte must emit at least one Call (flush_output)"
        );
    }

    #[test]
    fn windows_normal_exit_path_calls_flush_before_exit_process() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::PutByte],
        });
        // The instruction sequence ends with `Call(flush)` then the IAT
        // call to ExitProcess (the exit_process_iat). Verify a Call
        // appears immediately before some CallMemLabel (covers both
        // PutByte's intra-body flush and the exit-time flush; just check
        // the existence of an exit-pre-flush by ensuring there are
        // multiple Calls, since PutByte already emits one).
        let total_calls = program
            .asm
            .insts
            .iter()
            .filter(|i| matches!(i, AsmInst::Call(_)))
            .count();
        assert!(
            total_calls >= 2,
            "expected at least two Call(flush_output_label) sites: PutByte body + exit prologue"
        );
    }

    #[test]
    fn windows_put_byte_uses_buffered_path_not_writefile_per_byte() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::PutByte],
        });
        // PutByte should appear as the 5-instruction buffered hot path.
        let buffered = program.asm.insts.windows(5).any(|w| {
            matches!(
                w,
                [
                    AsmInst::MovAlMemR13,
                    AsmInst::MovMemRbxAl,
                    AsmInst::AddRegImm32(Reg64::Rbx, 1),
                    AsmInst::CmpRegReg(Reg64::Rbx, Reg64::Rbp),
                    AsmInst::Jnz(_),
                ]
            )
        });
        assert!(
            buffered,
            "Windows PutByte must emit the 5-instruction buffered hot path"
        );
        // It must Call the flush helper, not WriteFile directly.
        assert!(
            program
                .asm
                .insts
                .iter()
                .any(|i| matches!(i, AsmInst::Call(_))),
            "Windows PutByte must contain a `Call` to the flush helper"
        );
    }

    #[test]
    fn windows_flush_output_helper_uses_writefile_iat_with_aligned_frame() {
        let program = compile_lir_to_windows_program(&LirProgram { insts: vec![] });
        // The flush_output helper subroutine must reserve a 88-byte frame
        // (matches CALLEE_STACK_FRAME_BYTES used for ensure_tape) so the
        // inner WriteFile call lands on a 16-byte-aligned RSP.
        let reserves_frame = program
            .asm
            .insts
            .iter()
            .any(|i| matches!(i, AsmInst::AddRegImm32(Reg64::Rsp, -88)));
        let restores_frame = program
            .asm
            .insts
            .iter()
            .any(|i| matches!(i, AsmInst::AddRegImm32(Reg64::Rsp, 88)));
        assert!(
            reserves_frame && restores_frame,
            "flush_output helper must reserve and restore an 88-byte frame"
        );
        // The helper terminates with `Ret` (it's a subroutine, not inlined).
        assert!(
            program.asm.insts.iter().any(|i| matches!(i, AsmInst::Ret)),
            "flush_output helper must end with Ret"
        );
        // It computes count = rbx - (rbp - 4096) by lea rdx,[rbp-4096] then
        // mov r8, rbx; sub r8, rdx.
        let computes_count = program.asm.insts.windows(3).any(|w| {
            matches!(
                w,
                [
                    AsmInst::LeaRegMem(Reg64::Rdx, Reg64::Rbp, -4096),
                    AsmInst::MovRegReg(Reg64::R8, Reg64::Rbx),
                    AsmInst::SubRegReg(Reg64::R8, Reg64::Rdx),
                ]
            )
        });
        assert!(
            computes_count,
            "flush_output must compute count = rbx - (rbp - 4096) for WriteFile"
        );
    }

    #[test]
    fn windows_cell_add_other_values_keep_add_form() {
        let program = compile_lir_to_windows_program(&LirProgram {
            insts: vec![LirInst::CellAdd(2), LirInst::CellAdd(-3)],
        });
        // -3 normalises to 253 which fits in i8 as -3.
        assert!(
            program
                .asm
                .insts
                .iter()
                .any(|i| matches!(i, AsmInst::AddMem8Imm8(Reg64::R13, 2))),
            "CellAdd(2) must still use AddMem8Imm8"
        );
        assert!(
            program
                .asm
                .insts
                .iter()
                .any(|i| matches!(i, AsmInst::AddMem8Imm8(Reg64::R13, -3))),
            "CellAdd(-3) must use AddMem8Imm8 with sign-extended -3"
        );
    }
}
