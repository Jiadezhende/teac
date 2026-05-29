//! AAPCS64 stack-frame layout for the aarch64 backend.
//!
//! `FrameLayout` is the sole authority on how a single function's
//! frame is partitioned and how every fp- or sp-relative address is
//! constructed.  The frame regions, top-down:
//!
//! | Region              | Address range                          |
//! | ------------------- | -------------------------------------- |
//! | incoming arg slots  | `[fp, STACK_ARG_FP_BASE + n]`          |
//! | saved fp/lr         | `[fp, 0]` and `[fp, 8]`                |
//! | local frame         | `[fp, slot.offset_from_fp]`            |
//! | outgoing arg slots  | `[sp, off]`  (live only across a `bl`) |
//!
//! The local frame mixes `alloca`-backed stack objects and
//! spill-coloured vreg slots; both grow downward from `fp` and are
//! allocated by the target-agnostic [`StackFrame`].  `FrameLayout`
//! adds the AAPCS64-specific layer: the saved fp/lr region, the
//! incoming/outgoing argument offsets, the spill-slot size policy,
//! and the named address constructors that every other module in the
//! backend funnels through.

use super::aapcs::{ArgumentLocation, OUTGOING_ARG_SLOT_BYTES};
use super::types::{Addr, RegisterSize, Register, REG_FP};
use crate::asm::common::{align_up, StackFrame, StackSlot, StructLayouts};
use crate::asm::error::Error;
use crate::ir;

/// Width of the saved fp/lr pair that the AAPCS64 prologue places at
/// `[fp, 0]` / `[fp, 8]`.  Anchors the boundary between the local
/// frame (below) and the incoming-arg overflow (above).
pub const SAVED_REGS_BYTES: i64 = 16;

/// Byte offset, relative to `fp`, of the first stack-passed argument
/// slot in the callee's frame.  Stack-passed args (AAPCS64 §6.4.2
/// NGRN- / NSRN-saturated overflow) sit directly above the saved
/// fp/lr pair.
pub const STACK_ARG_FP_BASE: i64 = SAVED_REGS_BYTES;

/// `sp` alignment required at every `bl` boundary.  The outgoing-arg
/// area is rounded up to this multiple so `sp` stays 16-byte aligned
/// across a call.
pub const SP_ALIGNMENT: i64 = 16;

/// Size of the transient stack slot used by the printer when it has
/// to borrow `sp` to stash a register across a multi-step lowering.
/// AArch64 requires `sp` to stay 16-byte aligned at every instruction
/// boundary, so the slot is sized to that alignment even though only
/// 8 bytes are actually used.
pub const SCRATCH_SPILL_SLOT: i64 = 16;

/// AAPCS64 frame layout for one function.  Wraps the target-agnostic
/// [`StackFrame`] and owns every fp- or sp-relative address
/// construction in the aarch64 backend.
#[derive(Debug, Default)]
pub struct FrameLayout {
    inner: StackFrame,
}

impl FrameLayout {
    /// Builds the layout from the function's IR blocks, allocating
    /// one local-frame slot per `alloca`.  Spill slots are added
    /// later via [`Self::alloc_spill`] once register allocation has
    /// chosen which vregs spill.
    pub fn from_blocks(
        blocks: &[ir::BasicBlock],
        layouts: &StructLayouts,
    ) -> Result<Self, Error> {
        Ok(Self {
            inner: StackFrame::from_blocks(blocks, layouts)?,
        })
    }

    /// Reserves a spill slot sized according to the spilled vreg's
    /// [`RegisterSize`] and returns it.  32-bit values (W32 / S32) get
    /// a 4-byte slot; 64-bit values (X64) get an 8-byte slot.  Per-slot
    /// alignment matches the slot's size.  The slot is owned by the
    /// register allocation that requested it; the frame only tracks the
    /// cumulative size.
    pub fn alloc_spill(&mut self, size: RegisterSize) -> StackSlot {
        let (bytes, align) = spill_slot_layout(size);
        self.inner.alloc_slot(align, bytes)
    }

    /// Final local-frame size in bytes, rounded up to AAPCS64's
    /// 16-byte stack alignment.  Emitted as the prologue's
    /// `sub sp, sp, #frame_size` operand.
    pub fn frame_size(&self) -> i64 {
        self.inner.frame_size_aligned()
    }

    pub fn has_alloca(&self, vreg: usize) -> bool {
        self.inner.has_alloca(vreg)
    }

    pub fn alloca_slot(&self, vreg: usize) -> Option<StackSlot> {
        self.inner.alloca_slot(vreg)
    }

    /// fp-relative address of a local-frame slot: `[fp, slot.offset_from_fp]`.
    pub fn local_addr(slot: StackSlot) -> Addr {
        Addr::BaseOff {
            base: Register::Physical(REG_FP),
            offset: slot.offset_from_fp,
        }
    }

    /// fp-relative address of `extra` bytes past the start of `slot`.
    /// Used when the caller needs to reach into the middle of an
    /// alloca-backed aggregate (e.g. a struct field at a non-zero
    /// member offset).
    pub fn local_addr_with_offset(slot: StackSlot, extra: i64) -> Addr {
        Addr::BaseOff {
            base: Register::Physical(REG_FP),
            offset: slot.offset_from_fp + extra,
        }
    }

    /// fp-relative address of an incoming stack-passed argument at
    /// AAPCS64 offset `offset` in the caller's outgoing-arg area.
    /// Skips the saved fp/lr pair that the prologue placed between
    /// `fp` and the incoming-arg region.
    pub fn incoming_arg_addr(offset: i64) -> Addr {
        Addr::BaseOff {
            base: Register::Physical(REG_FP),
            offset: STACK_ARG_FP_BASE + offset,
        }
    }
}

/// sp-relative address of an outgoing stack-passed argument the
/// caller is currently settling for an imminent `bl`.  Valid only
/// while the matching `SubSp` / `AddSp` bracket is open.
pub fn outgoing_arg_addr(offset: i64) -> Addr {
    Addr::BaseOff {
        base: Register::StackPointer,
        offset,
    }
}

/// Bytes the caller must reserve below `sp` for the stack-passed
/// arguments in `locs`, rounded up to AAPCS64's 16-byte sp alignment.
/// Returns `0` when `locs` contains no [`ArgumentLocation::Stack`]
/// entries.
pub fn outgoing_stack_bytes(locs: &[ArgumentLocation]) -> i64 {
    let raw = locs
        .iter()
        .filter_map(|loc| match loc {
            ArgumentLocation::Stack { offset } => Some(*offset + OUTGOING_ARG_SLOT_BYTES),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    align_up(raw, SP_ALIGNMENT)
}

/// Spill-slot `(size, align)` policy.  32-bit values (W32 / S32)
/// occupy a 4-byte slot; 64-bit values (X64) occupy an 8-byte slot.
/// Per-slot alignment equals the slot size; the local frame as a
/// whole is rounded up to 16 bytes through [`FrameLayout::frame_size`].
fn spill_slot_layout(size: RegisterSize) -> (i64, i64) {
    match size {
        RegisterSize::W32 | RegisterSize::S32 => (4, 4),
        RegisterSize::X64 => (8, 8),
    }
}
