use ryo_backend::linker;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmitKind {
    /// Pretty-printed AST (parser output).
    Ast,
    /// Untyped IR (astgen output, Zig-style ZIR analogue).
    Uir,
    /// Typed IR (sema output, Zig-style AIR analogue).
    Tir,
    /// Flow execution and resolved cleanup.
    Flow,
    /// Cranelift IR (codegen output).
    Clif,
}

/// Libc to link a produced Linux binary against (the `ryo build
/// --link` flag). Accepted on every host; on non-Linux hosts it has no
/// effect because zig cc already links natively there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkMode {
    /// Static musl: the binary runs on any Linux regardless of host
    /// glibc version (the default).
    #[default]
    Musl,
    /// Link natively against the host glibc. Binaries are only as
    /// portable as the build host's glibc, but they get the host's
    /// full NSS/getaddrinfo stack.
    Glibc,
}

impl From<LinkMode> for linker::LinkMode {
    fn from(mode: LinkMode) -> Self {
        match mode {
            LinkMode::Musl => linker::LinkMode::Musl,
            LinkMode::Glibc => linker::LinkMode::Glibc,
        }
    }
}

/// Resolve `--emit` flag values into a normalized set. Membership
/// is what governs printing; the source order on the command line
/// is intentionally discarded. An empty list selects nothing;
/// `ir_command` selects all five sections in that case, while
/// `run`/`build` default to silence.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct EmitSet {
    pub ast: bool,
    pub uir: bool,
    pub tir: bool,
    pub flow: bool,
    pub clif: bool,
}

impl EmitSet {
    pub fn from_args(emit: &[EmitKind]) -> Self {
        let mut s = EmitSet::default();
        for k in emit {
            match k {
                EmitKind::Ast => s.ast = true,
                EmitKind::Uir => s.uir = true,
                EmitKind::Tir => s.tir = true,
                EmitKind::Flow => s.flow = true,
                EmitKind::Clif => s.clif = true,
            }
        }
        s
    }
}
