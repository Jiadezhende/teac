//! IR-level value space: the types that appear as instruction operands and
//! the types that record module-level definitions.
//!
//! Two concerns are kept strictly apart:
//!
//! * **Definitions** own the static data of a value — `GlobalDef` carries a
//!   global's initializer list; for SSA locals the producing instruction
//!   itself plays this role.  Definitions live in the module and never
//!   appear inside an instruction operand.
//! * **References** are the typed handles that an instruction operand
//!   carries: [`Local`] = `(LocalId, Dtype)`, [`GlobalRef`] = `(Rc<str>,
//!   Dtype)`, [`IntConst`] = `(Dtype, i64)`.  All three are cheap to clone,
//!   so propagating operands through passes does not drag any per-value
//!   heap data along.

use super::types::Dtype;
use std::fmt::{Display, Formatter};
use std::rc::Rc;

/// Unique identifier of an SSA local (virtual register) within one function.
///
/// `LocalId`s are allocated by [`FunctionGenerator::fresh_local`] and by a
/// small number of IR passes that grow the register space (mem2reg's phi
/// insertion, the assembly backend's parallel-copy materializer).  Values are
/// `Copy` so they can be used freely as map keys and compared without
/// intermediate clones.
///
/// [`FunctionGenerator::fresh_local`]: super::function::FunctionGenerator::fresh_local
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LocalId(pub usize);

/// A typed handle to a local SSA value.
///
/// Used for three purposes, all with the same shape:
/// * an instruction operand (`Operand::Local`),
/// * a symbol-table entry during IR lowering, and
/// * a function argument slot.
///
/// In SSA form every local is defined exactly once by the instruction that
/// produces it, so `(LocalId, Dtype)` is everything a local ever needs.
#[derive(Clone)]
pub struct Local {
    pub id: LocalId,
    pub dtype: Dtype,
}

impl Local {
    pub fn new(dtype: Dtype, id: LocalId) -> Self {
        Self { id, dtype }
    }
}

impl Display for Local {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "%r{}", self.id.0)
    }
}

/// A module-level global **definition**.  Lives in `Module::global_list`
/// keyed by the global's fully-qualified name (the key is held as `Rc<str>`
/// so instruction operands that refer to this global can share the same
/// string without re-allocating).
///
/// Never appears inside an instruction operand; use [`GlobalRef`] for that.
pub struct GlobalDef {
    pub dtype: Dtype,
    pub initializers: Option<Vec<i32>>,
}

/// A lightweight reference to a module-level global, suitable for embedding
/// in instruction operands.  Cloning is cheap: the name is shared through
/// `Rc<str>` with the owning module's global list.
#[derive(Clone)]
pub struct GlobalRef {
    pub name: Rc<str>,
    pub dtype: Dtype,
}

impl Display for GlobalRef {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{}", self.name)
    }
}

/// A typed integer constant operand.
///
/// The value is stored as `i64` to leave room for constants wider than i32
/// (e.g. pointer-sized indices) once the IR supports them.
#[derive(Clone)]
pub struct IntConst {
    pub dtype: Dtype,
    pub val: i64,
}

impl Display for IntConst {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.val)
    }
}

/// A typed floating-point constant operand.
///
/// The value is stored as `f64` because that is the precision LLVM IR uses
/// for the textual hex form of *every* floating constant — even `float`
/// (single-precision) operands are written as a 64-bit IEEE 754 hex literal.
#[derive(Clone)]
pub struct FloatConst {
    pub dtype: Dtype,
    pub val: f64,
}

impl Display for FloatConst {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        // LLVM IR writes floating constants as a 64-bit IEEE 754 hex literal.
        // For `float` (single-precision) operands the bit pattern must be
        // *exactly representable* in f32, otherwise LLVM rejects the module.
        // Round-trip through f32 so the emitted hex is the f32-exact value
        // (e.g. 3.14 -> 0x40091EB860000000, not the raw f64 0x40091EB851EB851F).
        let bits = match &self.dtype {
            Dtype::F32 => f64::from(self.val as f32).to_bits(),
            _ => self.val.to_bits(),
        };
        write!(f, "0x{bits:016X}")
    }
}

/// Instruction operand.
///
/// Exactly one of:
/// * a typed integer constant,
/// * a reference to a local SSA value inside the same function, or
/// * a reference to a module-level global.
#[derive(Clone)]
pub enum Operand {
    Const(IntConst),
    FloatConst(FloatConst),
    Local(Local),
    Global(GlobalRef),
}

impl Operand {
    /// The operand's data type.
    pub fn dtype(&self) -> &Dtype {
        match self {
            Operand::Const(c) => &c.dtype,
            Operand::FloatConst(c) => &c.dtype,
            Operand::Local(l) => &l.dtype,
            Operand::Global(g) => &g.dtype,
        }
    }

    /// If the operand is a local, its [`LocalId`]; otherwise `None`.
    pub fn local_id(&self) -> Option<LocalId> {
        match self {
            Operand::Local(l) => Some(l.id),
            _ => None,
        }
    }

    /// True for any operand other than an integer constant — i.e. anything
    /// that denotes a named vreg or a global symbol and could therefore
    /// hold an address.  The front-end pairs this predicate with a
    /// separate `Dtype::Pointer` check to decide whether to insert an
    /// implicit load at a value-use site.
    pub fn is_addressable(&self) -> bool {
        matches!(self, Operand::Local(_) | Operand::Global(_))
    }
}

impl Display for Operand {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Operand::Const(c) => Display::fmt(c, f),
            Operand::FloatConst(c) => Display::fmt(c, f),
            Operand::Local(l) => Display::fmt(l, f),
            Operand::Global(g) => Display::fmt(g, f),
        }
    }
}

impl From<Local> for Operand {
    fn from(l: Local) -> Self {
        Operand::Local(l)
    }
}

impl From<&Local> for Operand {
    fn from(l: &Local) -> Self {
        Operand::Local(l.clone())
    }
}

impl From<GlobalRef> for Operand {
    fn from(g: GlobalRef) -> Self {
        Operand::Global(g)
    }
}

impl From<i32> for Operand {
    fn from(v: i32) -> Self {
        Operand::Const(IntConst {
            dtype: Dtype::I32,
            val: i64::from(v),
        })
    }
}

impl From<f32> for Operand {
    fn from(v: f32) -> Self {
        Operand::FloatConst(FloatConst {
            dtype: Dtype::F32,
            val: f64::from(v),
        })
    }
}
