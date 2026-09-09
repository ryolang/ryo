use super::*;
use cranelift::codegen::ir::Value as ClifValue;

#[test]
fn value_repr_scalar_roundtrip() {
    let v = ClifValue::from_u32(1);
    let repr = ValueRepr::Scalar(v);
    assert_eq!(repr.expect_scalar(), v);
}

#[test]
fn value_repr_str_fields() {
    let repr = ValueRepr::Str {
        ptr: ClifValue::from_u32(1),
        len: ClifValue::from_u32(2),
        cap: ClifValue::from_u32(3),
    };
    match repr {
        ValueRepr::Str { ptr, len, cap } => {
            assert_ne!(ptr, len);
            assert_ne!(len, cap);
        }
        _ => panic!("expected Str"),
    }
}

#[test]
#[should_panic(expected = "expected Scalar, got Str")]
fn value_repr_expect_scalar_panics_on_str() {
    let repr = ValueRepr::Str {
        ptr: ClifValue::from_u32(1),
        len: ClifValue::from_u32(2),
        cap: ClifValue::from_u32(3),
    };
    repr.expect_scalar();
}

/// The three targets CI and the toolchain support: Linux x86-64,
/// Windows x86-64 (MSVC ABI), macOS aarch64.
const SUPPORTED_TRIPLES: [&str; 3] = [
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
];

/// Build a minimal function returning an i128 (the packed {ptr, len}
/// shape the string runtime ABI uses) and compile it with the given
/// flags. Returns the emitted machine code byte count.
fn compile_i128_return(flags: settings::Flags, triple: &str) -> Result<usize, String> {
    let triple: Triple = triple
        .parse()
        .map_err(|e| format!("bad triple {triple}: {e}"))?;
    let isa = isa::lookup(triple)
        .map_err(|e| format!("isa lookup: {e}"))?
        .finish(flags)
        .map_err(|e| format!("isa build: {e}"))?;

    let mut sig = Signature::new(isa.default_call_conv());
    sig.returns.push(AbiParam::new(types::I128));
    let mut func = cranelift::codegen::ir::Function::with_name_signature(
        cranelift::codegen::ir::UserFuncName::user(0, 0),
        sig,
    );
    {
        let mut fb_ctx = FunctionBuilderContext::new();
        let frontend_config = isa.frontend_config();
        let mut fb = FunctionBuilder::new(&mut func, &mut fb_ctx);
        let block = fb.create_block();
        fb.switch_to_block(block);
        // iconst only supports i8-i64; build the i128 via uextend.
        let lo = fb.ins().iconst(types::I64, 42);
        let pair = fb.ins().uextend(types::I128, lo);
        fb.ins().return_(&[pair]);
        fb.seal_all_blocks();
        fb.finalize(frontend_config);
    }

    let mut ctx = cranelift::codegen::Context::for_function(func);
    ctx.compile(
        &*isa,
        &mut cranelift::codegen::control::ControlPlane::default(),
    )
    .map_err(|e| format!("compile: {e:?}"))?;
    let code = ctx
        .compiled_code()
        .ok_or_else(|| "no compiled code".to_string())?;
    Ok(code.code_buffer().len())
}

#[test]
fn aot_i128_return_compiles_on_all_supported_targets() {
    // The packed-u128 string ABI puts an i128 in every producing
    // function's signature; the x64 ABI must accept it (LLVM ABI
    // extensions) on Linux AND Windows, and aarch64 must keep working.
    for triple in SUPPORTED_TRIPLES {
        let flags = aot_shared_flags().expect("shared flags");
        let len = compile_i128_return(flags, triple)
            .unwrap_or_else(|e| panic!("i128 return must compile for {triple}: {e}"));
        assert!(len > 0, "empty machine code for {triple}");
    }
}

#[test]
fn x64_i128_return_panics_without_llvm_abi_extensions() {
    // Pins the failure mode this fix addresses: without the flag, the
    // x64 ABI rejects i128 in signatures. If this test starts failing
    // (no panic), Cranelift changed its gating — re-audit the flag.
    let mut b = settings::builder();
    b.set("opt_level", "speed").expect("opt_level");
    let flags = settings::Flags::new(b);
    for triple in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = compile_i128_return(flags.clone(), triple);
        }));
        let err = result.expect_err("i128 return must panic without llvm abi extensions");
        let msg = err
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(
            msg.contains("i128 args/return values not supported"),
            "unexpected panic for {triple}: {msg}"
        );
    }
}
