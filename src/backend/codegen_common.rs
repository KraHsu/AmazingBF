//! Shared codegen utilities used by both the Linux and Windows x86_64 backends.
//!
//! Contains ABI-neutral helpers: label allocation, displacement-form memory
//! access, wide immediate addition, and the `PtrAdd`/`PtrAddChecked` sequences
//! that are identical across platforms.

use crate::backend::asm::{AsmInst, AsmLabel, Reg64};
use crate::ir::lir::{LabelId, LirInst, LirProgram};
use std::collections::HashSet;

/// Downward-counting allocator for transient internal labels.
///
/// Both backends use the same scheme: user labels count up from 0 (mapped
/// directly from `LabelId`), while internal labels count down from a
/// platform-specific ceiling. This struct encapsulates the decrement and
/// optional collision guard.
pub(crate) struct LabelAllocator {
    next: u32,
    /// Inclusive lower bound — `fresh()` panics if it would cross this.
    /// Set to 0 when no reserved range exists (Windows path).
    floor: u32,
}

impl LabelAllocator {
    /// Create an allocator that starts at `base` and decrements toward `floor`.
    pub(crate) fn new(base: u32, floor: u32) -> Self {
        Self { next: base, floor }
    }

    /// Allocate the next internal label.
    pub(crate) fn fresh(&mut self) -> AsmLabel {
        debug_assert!(
            self.next > self.floor,
            "internal label space exhausted (next=0x{:08x}, floor=0x{:08x})",
            self.next,
            self.floor,
        );
        let label = AsmLabel(self.next);
        self.next -= 1;
        label
    }
}

/// Map a LIR [`LabelId`] onto an [`AsmLabel`].
///
/// The raw `u32` is reused as-is: user labels count up from 0 and internal
/// labels count down from `u32::MAX`, so the two namespaces never collide.
pub(crate) fn map_label(id: LabelId) -> AsmLabel {
    AsmLabel(id.0)
}

/// Pick between `AddMem8ImmDisp8` and `AddMem8ImmDisp32` based on `off`'s
/// width. `off == 0` is handled by the caller (it canonicalises to `CellAdd`
/// and never reaches this helper).
pub(crate) fn mem8_add_at_r13(off: isize, imm: i8) -> AsmInst {
    if let Ok(disp8) = i8::try_from(off) {
        AsmInst::AddMem8ImmDisp8(Reg64::R13, disp8, imm)
    } else {
        let disp32 =
            i32::try_from(off).expect("CellAddAt offset must fit in i32 (lir_postpone DISP32 cap)");
        AsmInst::AddMem8ImmDisp32(Reg64::R13, disp32, imm)
    }
}

/// Pick between `MovMem8ImmDisp8` and `MovMem8ImmDisp32` based on `off`'s
/// width. `off == 0` is handled by the caller (canonicalised to `CellSet`).
pub(crate) fn mem8_set_at_r13(off: isize, val: u8) -> AsmInst {
    if let Ok(disp8) = i8::try_from(off) {
        AsmInst::MovMem8ImmDisp8(Reg64::R13, disp8, val)
    } else {
        let disp32 =
            i32::try_from(off).expect("CellSetAt offset must fit in i32 (lir_postpone DISP32 cap)");
        AsmInst::MovMem8ImmDisp32(Reg64::R13, disp32, val)
    }
}

/// Emit one or more `AddRegImm32` instructions to add an `isize` value to
/// `reg`. Values exceeding `i32` range are split into multiple chunks.
pub(crate) fn emit_add_reg_isize(out: &mut Vec<AsmInst>, reg: Reg64, value: isize) {
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

/// Emit `PtrAdd`: move `r13` by `n`, calling `ensure_tape` on out-of-range.
pub(crate) fn emit_ptr_add_out(
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

/// ABI-specific hooks that differ between Linux (SysV) and Windows (Win64).
pub(crate) trait PlatformEmitter {
    fn emit_put_byte(
        &self,
        out: &mut Vec<AsmInst>,
        labels: &mut LabelAllocator,
        flush_output_label: AsmLabel,
    );

    fn emit_get_byte(
        &self,
        out: &mut Vec<AsmInst>,
        labels: &mut LabelAllocator,
        exit_one_label: AsmLabel,
        flush_output_label: AsmLabel,
    );

    /// Win64 requires 16-byte RSP alignment before `call`; SysV does not.
    fn needs_rsp_alignment(&self) -> bool;
}

/// Translate the LIR instruction stream into AsmInst, delegating only
/// PutByte/GetByte and LinearMul RSP alignment to the platform emitter.
pub(crate) fn emit_lir_body(
    out: &mut Vec<AsmInst>,
    labels: &mut LabelAllocator,
    lir: &LirProgram,
    ensure_tape_label: AsmLabel,
    flush_output_label: AsmLabel,
    exit_one_label: AsmLabel,
    platform: &dyn PlatformEmitter,
) {
    let loop_heads: HashSet<_> = lir
        .insts
        .iter()
        .filter_map(|inst| match inst {
            LirInst::JumpIfNonZero(id) => Some(*id),
            _ => None,
        })
        .collect();

    let mut verified_window: Option<(isize, isize)> = None;
    let needs_align = platform.needs_rsp_alignment();

    for inst in &lir.insts {
        match inst {
            LirInst::PtrAdd(0) => {}
            LirInst::PtrAdd(n) => {
                let n = *n;
                if let Some((wlo, whi)) = verified_window
                    && wlo <= n
                    && n <= whi
                {
                    emit_add_reg_isize(out, Reg64::R13, n);
                    verified_window = Some((wlo - n, whi - n));
                } else {
                    emit_ptr_add_out(out, labels, n, ensure_tape_label);
                    verified_window = None;
                }
            }
            LirInst::PtrAddChecked {
                delta,
                lo_extent,
                hi_extent,
            } => {
                let delta = *delta;
                let lo_extent = *lo_extent;
                let hi_extent = *hi_extent;
                let covered = matches!(
                    verified_window,
                    Some((wlo, whi)) if wlo <= lo_extent && hi_extent <= whi
                );
                if covered {
                    if delta != 0 {
                        emit_add_reg_isize(out, Reg64::R13, delta);
                    }
                } else {
                    emit_ptr_add_checked_out(
                        out,
                        labels,
                        delta,
                        lo_extent,
                        hi_extent,
                        ensure_tape_label,
                    );
                }
                let (nlo, nhi) = match verified_window {
                    Some((wlo, whi)) => (wlo.min(lo_extent), whi.max(hi_extent)),
                    None => (lo_extent, hi_extent),
                };
                verified_window = Some((nlo - delta, nhi - delta));
            }
            LirInst::LinearMul(factors) => {
                if factors.is_empty() {
                    out.push(AsmInst::MovMem8Imm8(Reg64::R13, 0));
                    continue;
                }
                let all_disp8 = factors.iter().all(|(off, _)| i8::try_from(*off).is_ok());
                out.push(AsmInst::Push(Reg64::Rbx));
                if needs_align {
                    out.push(AsmInst::AddRegImm32(Reg64::Rsp, -8));
                }
                out.push(AsmInst::MovzxEbxFromMemR13);
                out.push(AsmInst::MovMem8Imm8(Reg64::R13, 0));
                if all_disp8 {
                    let lo = factors.iter().map(|(o, _)| *o).min().unwrap_or(0).min(0);
                    let hi = factors.iter().map(|(o, _)| *o).max().unwrap_or(0).max(0);
                    emit_ptr_add_checked_out(out, labels, 0, lo, hi, ensure_tape_label);
                    for (off, f) in factors {
                        let d = *off as i8;
                        let f_mod = ((f % 256) + 256) % 256;
                        match f_mod {
                            0 => {}
                            1 => out.push(AsmInst::AddMemR13BlDisp8(d)),
                            255 => out.push(AsmInst::SubMemR13BlDisp8(d)),
                            _ => {
                                out.push(AsmInst::ImulEaxEbxImm32(*f));
                                out.push(AsmInst::AddMemR13AlDisp8(d));
                            }
                        }
                    }
                    verified_window = Some((lo, hi));
                } else {
                    for (off, f) in factors {
                        emit_ptr_add_out(out, labels, *off, ensure_tape_label);
                        let f_mod = ((f % 256) + 256) % 256;
                        match f_mod {
                            0 => {}
                            1 => out.push(AsmInst::AddMemR13Bl),
                            255 => out.push(AsmInst::SubMemR13Bl),
                            _ => {
                                out.push(AsmInst::ImulEaxEbxImm32(*f));
                                out.push(AsmInst::AddMemR13Al);
                            }
                        }
                        emit_ptr_add_out(out, labels, -*off, ensure_tape_label);
                    }
                    verified_window = None;
                }
                if needs_align {
                    out.push(AsmInst::AddRegImm32(Reg64::Rsp, 8));
                }
                out.push(AsmInst::Pop(Reg64::Rbx));
            }
            LirInst::LinearMulWithSets { factors, sets } => {
                let done_label = labels.fresh();
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0));
                out.push(AsmInst::Jz(done_label));
                let all_offsets: Vec<isize> = factors
                    .iter()
                    .map(|(o, _)| *o)
                    .chain(sets.iter().copied())
                    .collect();
                let all_disp8 = all_offsets.iter().all(|o| i8::try_from(*o).is_ok());
                out.push(AsmInst::Push(Reg64::Rbx));
                if needs_align {
                    out.push(AsmInst::AddRegImm32(Reg64::Rsp, -8));
                }
                out.push(AsmInst::MovzxEbxFromMemR13);
                out.push(AsmInst::MovMem8Imm8(Reg64::R13, 0));
                if all_disp8 {
                    let lo = all_offsets.iter().copied().min().unwrap_or(0).min(0);
                    let hi = all_offsets.iter().copied().max().unwrap_or(0).max(0);
                    emit_ptr_add_checked_out(out, labels, 0, lo, hi, ensure_tape_label);
                    for (off, f) in factors {
                        let d = *off as i8;
                        let f_mod = ((f % 256) + 256) % 256;
                        match f_mod {
                            0 => {}
                            1 => out.push(AsmInst::AddMemR13BlDisp8(d)),
                            255 => out.push(AsmInst::SubMemR13BlDisp8(d)),
                            _ => {
                                out.push(AsmInst::ImulEaxEbxImm32(*f));
                                out.push(AsmInst::AddMemR13AlDisp8(d));
                            }
                        }
                    }
                    for off in sets {
                        out.push(AsmInst::MovMem8ImmDisp8(Reg64::R13, *off as i8, 0));
                    }
                    verified_window = Some((lo, hi));
                } else {
                    for (off, f) in factors {
                        emit_ptr_add_out(out, labels, *off, ensure_tape_label);
                        let f_mod = ((f % 256) + 256) % 256;
                        match f_mod {
                            0 => {}
                            1 => out.push(AsmInst::AddMemR13Bl),
                            255 => out.push(AsmInst::SubMemR13Bl),
                            _ => {
                                out.push(AsmInst::ImulEaxEbxImm32(*f));
                                out.push(AsmInst::AddMemR13Al);
                            }
                        }
                        emit_ptr_add_out(out, labels, -*off, ensure_tape_label);
                    }
                    for off in sets {
                        emit_ptr_add_out(out, labels, *off, ensure_tape_label);
                        out.push(AsmInst::MovMem8Imm8(Reg64::R13, 0));
                        emit_ptr_add_out(out, labels, -*off, ensure_tape_label);
                    }
                    verified_window = None;
                }
                if needs_align {
                    out.push(AsmInst::AddRegImm32(Reg64::Rsp, 8));
                }
                out.push(AsmInst::Pop(Reg64::Rbx));
                out.push(AsmInst::Label(done_label));
            }
            LirInst::Scan(dir) => {
                let step = *dir;
                debug_assert!(step == 1 || step == -1, "Scan step must be ±1");
                let loop_top = labels.fresh();
                let loop_done = labels.fresh();
                out.push(AsmInst::Label(loop_top));
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0));
                out.push(AsmInst::Jz(loop_done));
                emit_ptr_add_out(out, labels, step, ensure_tape_label);
                out.push(AsmInst::Jmp(loop_top));
                out.push(AsmInst::Label(loop_done));
                verified_window = None;
            }
            LirInst::ScanWithHint { dir, hint_bytes } => {
                let step = *dir;
                debug_assert!(step == 1 || step == -1, "ScanWithHint step must be ±1");
                let slow_top = labels.fresh();
                let done = labels.fresh();

                if *hint_bytes > 0 {
                    out.push(AsmInst::MovRegImm64(Reg64::Rax, 0));
                    out.push(AsmInst::MovRegReg(Reg64::Rdi, Reg64::R13));
                    out.push(AsmInst::MovRegImm64(Reg64::Rcx, i64::from(*hint_bytes)));
                    if step == -1 {
                        out.push(AsmInst::Std);
                    } else {
                        out.push(AsmInst::Cld);
                    }
                    out.push(AsmInst::RepneScasb);
                    if step == -1 {
                        out.push(AsmInst::Cld);
                    }
                    out.push(AsmInst::MovRegReg(Reg64::R13, Reg64::Rdi));
                    out.push(AsmInst::AddRegImm32(Reg64::R13, -(step as i32)));
                }

                out.push(AsmInst::Label(slow_top));
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0));
                out.push(AsmInst::Jz(done));
                emit_ptr_add_out(out, labels, step, ensure_tape_label);
                out.push(AsmInst::Jmp(slow_top));
                out.push(AsmInst::Label(done));
                verified_window = None;
            }
            LirInst::CellAdd(0) => {}
            LirInst::CellAdd(n) => {
                let imm = ((*n % 256) + 256) % 256;
                match imm {
                    0 => {}
                    1 => out.push(AsmInst::IncMem8(Reg64::R13)),
                    255 => out.push(AsmInst::DecMem8(Reg64::R13)),
                    other => {
                        out.push(AsmInst::AddMem8Imm8(Reg64::R13, other as u8 as i8));
                    }
                }
            }
            LirInst::CellSet(v) => {
                out.push(AsmInst::MovMem8Imm8(Reg64::R13, *v));
            }
            LirInst::CellAddAt { off, delta } => {
                debug_assert!(
                    *off != 0,
                    "CellAddAt(off=0) should be canonicalised to CellAdd"
                );
                let imm = ((*delta % 256) + 256) % 256;
                if imm != 0 {
                    out.push(mem8_add_at_r13(*off, imm as u8 as i8));
                }
            }
            LirInst::CellSetAt { off, val } => {
                debug_assert!(
                    *off != 0,
                    "CellSetAt(off=0) should be canonicalised to CellSet"
                );
                out.push(mem8_set_at_r13(*off, *val));
            }
            LirInst::ZeroRun { start, count } => {
                debug_assert!(*count >= 2, "ZeroRun should hold at least two bytes");
                if *count >= 16 {
                    out.push(AsmInst::XorEaxEax);
                    out.push(AsmInst::LeaRegMem(Reg64::Rdi, Reg64::R13, *start));
                    out.push(AsmInst::MovEcxImm32(*count as i32));
                    out.push(AsmInst::Cld);
                    out.push(AsmInst::RepStosb);
                } else {
                    for i in 0..*count {
                        let off = isize::try_from(i64::from(*start) + i64::from(i))
                            .expect("ZeroRun offset must fit in isize");
                        if off == 0 {
                            out.push(AsmInst::MovMem8Imm8(Reg64::R13, 0));
                        } else {
                            out.push(mem8_set_at_r13(off, 0));
                        }
                    }
                }
            }
            LirInst::PutByte => {
                platform.emit_put_byte(out, labels, flush_output_label);
                verified_window = None;
            }
            LirInst::GetByte => {
                platform.emit_get_byte(out, labels, exit_one_label, flush_output_label);
                verified_window = None;
            }
            LirInst::Label(id) => {
                if loop_heads.contains(id) {
                    out.push(AsmInst::Align16);
                }
                out.push(AsmInst::Label(map_label(*id)));
                verified_window = None;
            }
            LirInst::JumpIfZero(id) => {
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0));
                out.push(AsmInst::Jz(map_label(*id)));
                verified_window = None;
            }
            LirInst::JumpIfNonZero(id) => {
                out.push(AsmInst::CmpMem8Imm8(Reg64::R13, 0));
                out.push(AsmInst::Jnz(map_label(*id)));
                verified_window = None;
            }
        }
    }
}
/// then advance `r13` by `delta` with no further bounds check.
pub(crate) fn emit_ptr_add_checked_out(
    out: &mut Vec<AsmInst>,
    labels: &mut LabelAllocator,
    delta: isize,
    lo_extent: isize,
    hi_extent: isize,
    ensure_tape_label: AsmLabel,
) {
    debug_assert!(
        lo_extent <= 0 && 0 <= hi_extent,
        "PtrAddChecked window must contain the origin"
    );
    debug_assert!(
        lo_extent <= delta && delta <= hi_extent,
        "PtrAddChecked delta must lie inside the verified window"
    );

    if lo_extent == 0 && hi_extent == 0 {
        debug_assert_eq!(delta, 0, "degenerate PtrAddChecked must have delta = 0");
        return;
    }

    let retry = labels.fresh();
    let slow_lo = labels.fresh();
    let slow_hi = labels.fresh();
    let done = labels.fresh();

    out.push(AsmInst::Label(retry));

    if lo_extent < 0 {
        out.push(AsmInst::MovRegReg(Reg64::R15, Reg64::R13));
        emit_add_reg_isize(out, Reg64::R15, lo_extent);
        out.push(AsmInst::CmpRegReg(Reg64::R15, Reg64::R12));
        out.push(AsmInst::Jb(slow_lo));
    }
    if hi_extent > 0 {
        out.push(AsmInst::MovRegReg(Reg64::R15, Reg64::R13));
        emit_add_reg_isize(out, Reg64::R15, hi_extent);
        out.push(AsmInst::CmpRegReg(Reg64::R15, Reg64::R14));
        out.push(AsmInst::Jae(slow_hi));
    }

    if delta != 0 {
        emit_add_reg_isize(out, Reg64::R13, delta);
    }
    out.push(AsmInst::Jmp(done));

    if lo_extent < 0 {
        out.push(AsmInst::Label(slow_lo));
        out.push(AsmInst::Call(ensure_tape_label));
        emit_add_reg_isize(out, Reg64::R13, -lo_extent);
        out.push(AsmInst::Jmp(retry));
    }
    if hi_extent > 0 {
        out.push(AsmInst::Label(slow_hi));
        out.push(AsmInst::Call(ensure_tape_label));
        emit_add_reg_isize(out, Reg64::R13, -hi_extent);
        out.push(AsmInst::Jmp(retry));
    }

    out.push(AsmInst::Label(done));
}
