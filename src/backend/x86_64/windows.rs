use crate::backend::asm::{AsmInst, AsmLabel, AsmProgram, Reg64};
use crate::ir::lir::{LabelId, LirInst, LirProgram};

const INITIAL_TAPE_SIZE: usize = 4096;
const MEM_COMMIT_RESERVE: i64 = 0x3000;
const MEM_RELEASE: i64 = 0x8000;
const PAGE_READWRITE: i64 = 0x04;
const STD_INPUT_HANDLE: i64 = -10;
const STD_OUTPUT_HANDLE: i64 = -11;
const SHADOW_SPACE_BYTES: i32 = 40;
const IO_COUNT_SLOT_DISP: i32 = 24;
const OVERLAPPED_SLOT_DISP: i32 = 32;
const STACK_SAVED_NEW_BASE: i32 = 0;
const STACK_SAVED_COPY_START: i32 = 8;
const STACK_SAVED_NEW_LEN: i32 = 16;
const STACK_SAVED_DESIRED_OFFSET: i32 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kernel32Import {
    ExitProcess,
    GetStdHandle,
    ReadFile,
    WriteFile,
    VirtualAlloc,
    VirtualFree,
}

impl Kernel32Import {
    fn name(self) -> &'static str {
        match self {
            Self::ExitProcess => "ExitProcess",
            Self::GetStdHandle => "GetStdHandle",
            Self::ReadFile => "ReadFile",
            Self::WriteFile => "WriteFile",
            Self::VirtualAlloc => "VirtualAlloc",
            Self::VirtualFree => "VirtualFree",
        }
    }
}

#[derive(Debug, Clone)]
pub struct WindowsImport {
    pub name: &'static str,
    pub hint_name_label: AsmLabel,
    pub ilt_entry_label: AsmLabel,
    pub iat_entry_label: AsmLabel,
}

#[derive(Debug, Clone)]
pub struct WindowsProgram {
    pub asm: AsmProgram,
    pub entry_label: AsmLabel,
    pub import_desc_label: AsmLabel,
    pub import_dir_size: u32,
    pub iat_label: AsmLabel,
    pub iat_size: u32,
    pub dll_name_label: AsmLabel,
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

struct LabelAllocator {
    next: u32,
}

impl LabelAllocator {
    fn new() -> Self {
        Self { next: 0xFFFE_FFFF }
    }

    fn fresh(&mut self) -> AsmLabel {
        let label = AsmLabel(self.next);
        self.next -= 1;
        label
    }
}

pub fn compile_lir_to_windows_program(lir: &LirProgram) -> WindowsProgram {
    let mut labels = LabelAllocator::new();
    let entry_label = labels.fresh();
    let ensure_tape_label = labels.fresh();
    let exit_one_label = labels.fresh();
    let grow_loop = labels.fresh();
    let imports = Kernel32Imports::new(
        &mut labels,
        &[
            Kernel32Import::ExitProcess,
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

    for inst in &lir.insts {
        match inst {
            LirInst::PtrAdd(0) => {}
            LirInst::PtrAdd(n) => {
                emit_ptr_add_out(&mut out, &mut labels, *n, ensure_tape_label);
            }
            LirInst::LinearMul(factors) => {
                if factors.is_empty() {
                    out.push(AsmInst::MovMem8Imm8(Reg64::R13, 0));
                    continue;
                }
                out.push(AsmInst::MovzxEbxFromMemR13);
                out.push(AsmInst::MovMem8Imm8(Reg64::R13, 0));
                for (off, factor) in factors {
                    emit_ptr_add_out(&mut out, &mut labels, *off, ensure_tape_label);
                    out.push(AsmInst::MovEaxEbx);
                    out.push(AsmInst::ImulEaxEbxImm32(*factor));
                    out.push(AsmInst::AddMemR13Al);
                    emit_ptr_add_out(&mut out, &mut labels, -*off, ensure_tape_label);
                }
            }
            LirInst::Scan(dir) => {
                let loop_top = labels.fresh();
                let loop_done = labels.fresh();
                out.push(AsmInst::Label(loop_top));
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0));
                out.push(AsmInst::Jz(loop_done));
                emit_ptr_add_out(&mut out, &mut labels, *dir, ensure_tape_label);
                out.push(AsmInst::Jmp(loop_top));
                out.push(AsmInst::Label(loop_done));
            }
            LirInst::CellAdd(0) => {}
            LirInst::CellAdd(n) => {
                let imm = ((*n % 256) + 256) % 256;
                if imm != 0 {
                    out.push(AsmInst::AddMem8Imm8(Reg64::R13, imm as u8 as i8));
                }
            }
            LirInst::CellSet(v) => out.push(AsmInst::MovMem8Imm8(Reg64::R13, *v)),
            LirInst::PutByte => emit_put_byte(
                &mut out,
                imports.iat_label(Kernel32Import::WriteFile),
                exit_one_label,
            ),
            LirInst::GetByte => emit_get_byte(
                &mut out,
                &mut labels,
                imports.iat_label(Kernel32Import::ReadFile),
                exit_one_label,
            ),
            LirInst::Label(id) => out.push(AsmInst::Label(map_label(*id))),
            LirInst::JumpIfZero(id) => {
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0));
                out.push(AsmInst::Jz(map_label(*id)));
            }
            LirInst::JumpIfNonZero(id) => {
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0));
                out.push(AsmInst::Jnz(map_label(*id)));
            }
            #[allow(unreachable_patterns)]
            _ => panic!("unsupported LIR instruction in windows backend"),
        }
    }

    emit_exit_process(
        &mut out,
        imports.iat_label(Kernel32Import::ExitProcess),
        0,
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
    emit_exit_process(
        &mut out,
        imports.iat_label(Kernel32Import::ExitProcess),
        1,
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

pub fn compile_trivial_exit_program(exit_code: u32) -> WindowsProgram {
    let mut labels = LabelAllocator::new();
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

pub fn compile_precomputed_stdout_program(data: &[u8]) -> WindowsProgram {
    if data.is_empty() {
        return compile_trivial_exit_program(0);
    }

    let mut labels = LabelAllocator::new();
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
    out.push(AsmInst::LeaRegMem(Reg64::R9, Reg64::Rsp, IO_COUNT_SLOT_DISP));
    out.push(AsmInst::CallMemLabel(
        imports.iat_label(Kernel32Import::WriteFile),
    ));
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jz(exit_one_label));
    emit_exit_process(
        &mut out,
        imports.iat_label(Kernel32Import::ExitProcess),
        0,
    );
    out.push(AsmInst::Label(exit_one_label));
    emit_exit_process(
        &mut out,
        imports.iat_label(Kernel32Import::ExitProcess),
        1,
    );
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
    out.push(AsmInst::AddRegImm32(Reg64::Rsp, -SHADOW_SPACE_BYTES));
}

fn emit_ptr_add_out(
    out: &mut Vec<AsmInst>,
    labels: &mut LabelAllocator,
    n: isize,
    ensure_tape_label: AsmLabel,
) {
    if n == 0 {
        return;
    }

    let slow_path = labels.fresh();
    let done = labels.fresh();

    out.push(AsmInst::MovRegReg(Reg64::R15, Reg64::R13));
    emit_add_reg_isize(out, Reg64::R15, n);
    out.push(AsmInst::CmpRegReg(Reg64::R15, Reg64::R12));
    out.push(AsmInst::Jb(slow_path));
    out.push(AsmInst::CmpRegReg(Reg64::R15, Reg64::R14));
    out.push(AsmInst::Jae(slow_path));
    out.push(AsmInst::MovRegReg(Reg64::R13, Reg64::R15));
    out.push(AsmInst::Jmp(done));
    out.push(AsmInst::Label(slow_path));
    out.push(AsmInst::Call(ensure_tape_label));
    out.push(AsmInst::Label(done));
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

fn emit_put_byte(out: &mut Vec<AsmInst>, write_file_iat: AsmLabel, exit_one_label: AsmLabel) {
    emit_zero_stack_qword(out, IO_COUNT_SLOT_DISP);
    out.push(AsmInst::MovRegReg(Reg64::Rcx, Reg64::Rdi));
    out.push(AsmInst::MovRegReg(Reg64::Rdx, Reg64::R13));
    out.push(AsmInst::MovRegImm64(Reg64::R8, 1));
    out.push(AsmInst::LeaRegMem(Reg64::R9, Reg64::Rsp, IO_COUNT_SLOT_DISP));
    out.push(AsmInst::CallMemLabel(write_file_iat));
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jz(exit_one_label));
}

fn emit_get_byte(
    out: &mut Vec<AsmInst>,
    labels: &mut LabelAllocator,
    read_file_iat: AsmLabel,
    exit_one_label: AsmLabel,
) {
    let done = labels.fresh();
    emit_zero_stack_qword(out, IO_COUNT_SLOT_DISP);
    out.push(AsmInst::MovRegReg(Reg64::Rcx, Reg64::Rsi));
    out.push(AsmInst::MovRegReg(Reg64::Rdx, Reg64::R13));
    out.push(AsmInst::MovRegImm64(Reg64::R8, 1));
    out.push(AsmInst::LeaRegMem(Reg64::R9, Reg64::Rsp, IO_COUNT_SLOT_DISP));
    out.push(AsmInst::CallMemLabel(read_file_iat));
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jz(exit_one_label));
    out.push(AsmInst::MovRegMem64(Reg64::Rax, Reg64::Rsp, IO_COUNT_SLOT_DISP));
    out.push(AsmInst::CmpRegImm32(Reg64::Rax, 0));
    out.push(AsmInst::Jnz(done));
    out.push(AsmInst::MovMem8Imm8(Reg64::R13, 255));
    out.push(AsmInst::Label(done));
}

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
    out.push(AsmInst::AddRegImm32(Reg64::Rsp, -SHADOW_SPACE_BYTES));
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
    out.push(AsmInst::MovMemReg64(Reg64::Rsp, STACK_SAVED_NEW_LEN, Reg64::R11));
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
    out.push(AsmInst::MovRegMem64(Reg64::R11, Reg64::Rsp, STACK_SAVED_NEW_LEN));
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
    out.push(AsmInst::MovMemReg64(Reg64::Rsp, STACK_SAVED_NEW_BASE, Reg64::Rdx));
    out.push(AsmInst::MovMemReg64(
        Reg64::Rsp,
        STACK_SAVED_COPY_START,
        Reg64::R8,
    ));
    out.push(AsmInst::MovMemReg64(Reg64::Rsp, STACK_SAVED_NEW_LEN, Reg64::R11));
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
    out.push(AsmInst::AddRegImm32(Reg64::Rsp, SHADOW_SPACE_BYTES));
    out.push(AsmInst::Ret);
}

fn emit_add_reg_isize(out: &mut Vec<AsmInst>, reg: Reg64, value: isize) {
    let mut remaining = i64::try_from(value).expect("pointer delta did not fit in i64");
    while remaining != 0 {
        let chunk = if remaining > i64::from(i32::MAX) {
            i32::MAX
        } else if remaining < i64::from(i32::MIN) {
            i32::MIN
        } else {
            remaining as i32
        };
        out.push(AsmInst::AddRegImm32(reg, chunk));
        remaining -= i64::from(chunk);
    }
}

fn map_label(id: LabelId) -> AsmLabel {
    AsmLabel(id.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::asm::AsmInst;
    use crate::ir::lir::LirProgram;

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
                "GetStdHandle",
                "ReadFile",
                "WriteFile",
                "VirtualAlloc",
                "VirtualFree",
            ]
        );
    }
}
