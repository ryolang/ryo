use ryo_backend::codegen::Codegen;
use ryo_core::compilation::CompilationContext;
use ryo_core::diag::{DiagSink, Severity};
use ryo_core::flow::cleanup::{CleanupPlan, ReleaseStorage};
use ryo_core::flow::{DropPosition, Flow};
use ryo_core::ownership::OwnershipSidecar;
use ryo_core::tir::{Tir, TirRef, TirTag};
use ryo_core::types::InternPool;
use tempfile::TempDir;

const LOOP_INOUT_BINDING_CLEANUP: &str = "fn bump(inout value: int):\n\tvalue += 1\n\nfn main():\n\tmut i = 40\n\tmut counter = 0\n\tvalue = \"outer\"\n\tfor i in range(0, 2):\n\t\tmut value = \"inner_012345678901234567890123456789\" + int_to_str(i)\n\t\tmut j = 0\n\t\twhile j < 1:\n\t\t\tprint(value)\n\t\t\tprint(\"\\n\")\n\t\t\tj += 1\n\t\tbump(&counter)\n\tbump(&i)\n\tprint(i)\n\tprint(\"\\n\")\n\tprint(counter)\n\tprint(\"\\n\")\n\tprint(value)\n\tprint(\"\\n\")\n";

type BindingArtifacts = (
    Vec<Tir>,
    Vec<Flow>,
    InternPool,
    OwnershipSidecar,
    CompilationContext,
);

fn binding_execution_artifacts(source: &str, optimized: bool) -> BindingArtifacts {
    let mut context = CompilationContext::new();
    let source_id = context.register_source(std::path::PathBuf::from("binding.ryo"), source.into());
    let registered = context.sources().get(source_id).expect("source");
    let mut pool = InternPool::new();
    let (program, parse_diags) =
        ryo_frontend::parse::parse_source(registered, &mut pool).expect("parse");
    assert!(
        parse_diags
            .iter()
            .all(|diag| diag.severity != Severity::Error),
        "parse diagnostics: {parse_diags:?}"
    );
    let uir = ryo_frontend::astgen::generate(&program, &mut pool, &mut DiagSink::new());
    let mut sema_sink = DiagSink::new();
    let tirs = ryo_frontend::sema::analyze(
        &uir,
        &mut pool,
        &mut sema_sink,
        source,
        std::path::Path::new("binding.ryo"),
        &mut context,
    );
    assert!(!sema_sink.has_errors(), "sema: {sema_sink:?}");
    let flows: Vec<_> = tirs.iter().map(|tir| Flow::build(tir, &pool)).collect();
    let cleanup = if optimized {
        ryo_frontend::ownership::check(&tirs, &flows, &pool, &mut DiagSink::new())
    } else {
        ryo_frontend::ownership::check_without_optimizations(
            &tirs,
            &flows,
            &pool,
            &mut DiagSink::new(),
        )
        .expect("unoptimized cleanup")
    };
    (tirs, flows, pool, cleanup, context)
}

fn require_inner_loop_exit_cleanup(
    tirs: &[Tir],
    flows: &[Flow],
    pool: &mut InternPool,
    sidecar: &OwnershipSidecar,
) -> Result<(), String> {
    let main = tirs
        .iter()
        .position(|tir| pool.str(tir.name) == "main")
        .ok_or_else(|| "main body missing".to_string())?;
    let tir = &tirs[main];
    let flow = &flows[main];
    let cleanup = &sidecar.functions[main];
    let plan = CleanupPlan::resolve(tir, flow, cleanup, pool)?;
    let value_name = pool.intern_str("value");
    let declarations: Vec<_> = tir
        .instructions
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, inst)| inst.tag == TirTag::VarDecl)
        .map(|(index, _)| {
            let declaration = TirRef::from_raw(index as u32);
            let view = tir.var_decl_view(declaration);
            (declaration, view.id, view.initializer)
        })
        .filter(|(_, id, _)| tir.binding_name(*id) == value_name)
        .collect();
    if declarations.len() != 2 {
        return Err(format!(
            "expected outer and inner value declarations, got {}",
            declarations.len()
        ));
    }
    let (_, outer_id, outer_initializer) = declarations[0];
    let (_, inner_id, inner_initializer) = declarations[1];
    if outer_id == inner_id {
        return Err("same-spelled declarations share a BindingId".into());
    }
    let outer_place = flow
        .resolve(outer_id)
        .ok_or_else(|| "outer binding has no Flow place".to_string())?;
    let inner_place = flow
        .resolve(inner_id)
        .ok_or_else(|| "inner binding has no Flow place".to_string())?;
    if outer_place == inner_place {
        return Err("same-spelled bindings share a Flow place".into());
    }
    let loop_inst = tir
        .instructions
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, inst)| inst.tag == TirTag::WhileLoop)
        .map(|(index, _)| TirRef::from_raw(index as u32))
        .ok_or_else(|| "nested loop missing".to_string())?;
    let inner_drop = cleanup
        .drops
        .iter()
        .position(|drop| {
            drop.target == inner_initializer
                && matches!(
                    drop.position,
                    DropPosition::AfterStep { inst } if inst == loop_inst
                )
        })
        .ok_or_else(|| {
            format!(
                "inner binding has no cleanup at loop exit: init={inner_initializer:?} loop={loop_inst:?} drops={:?}",
                cleanup.drops
            )
        })?;
    if plan.releases.get(inner_drop) != Some(&ReleaseStorage::Binding(inner_place)) {
        return Err("inner loop-exit release resolves to the wrong binding".into());
    }
    let outer_drop = cleanup
        .drops
        .iter()
        .position(|drop| {
            drop.target == outer_initializer && drop.position.anchor_inst() != Some(loop_inst)
        })
        .ok_or_else(|| "outer binding has no later cleanup".to_string())?;
    if outer_drop == inner_drop {
        return Err("inner and outer cleanup entries are conflated".into());
    }
    if plan.releases.get(outer_drop) != Some(&ReleaseStorage::Binding(outer_place)) {
        return Err("outer later release resolves to the wrong binding".into());
    }
    Ok(())
}

#[test]
fn loop_exit_cleanup_releases_inner_binding_at_loop_exit() {
    for optimized in [true, false] {
        let (tirs, flows, mut pool, cleanup, _context) =
            binding_execution_artifacts(LOOP_INOUT_BINDING_CLEANUP, optimized);
        require_inner_loop_exit_cleanup(&tirs, &flows, &mut pool, &cleanup)
            .unwrap_or_else(|error| panic!("optimized={optimized}: {error}"));
    }
}

const SHADOWED_LITERAL_CLEANUP: &str = "fn main():\n\tvalue = \"outer\"\n\tif true:\n\t\tmut value = \"inner\"\n\t\tprint(value)\n\t\tprint(\"\\n\")\n\tprint(value)\n\tprint(\"\\n\")\n";
const SHADOWED_HEAP_CLEANUP: &str = "fn main():\n\tvalue = \"outer_012345678901234567890123456789\" + \"!\"\n\tfor i in range(0, 1):\n\t\tmut value = \"inner_012345678901234567890123456789\" + \"?\"\n\t\tprint(value)\n\t\tprint(\"\\n\")\n\tprint(value)\n\tprint(\"\\n\")\n";

fn execute_binding_mode(source: &str, mode: &str) {
    let optimized = mode.ends_with("opt") && !mode.ends_with("unopt");
    let (tirs, flows, pool, cleanup, _context) = binding_execution_artifacts(source, optimized);
    if mode.starts_with("jit") {
        let mut jit = Codegen::new_jit_with_optimizations(optimized).expect("JIT initialization");
        let (main, _) = jit
            .compile(&tirs, &flows, &pool, &cleanup, false)
            .expect("JIT emission");
        assert_eq!(jit.execute(main, &[]).expect("JIT execution"), 0);
        return;
    }

    let temp = TempDir::new().expect("AOT tempdir");
    let mut aot = Codegen::new_aot_with_optimizations(target_lexicon::Triple::host(), optimized)
        .expect("AOT initialization");
    aot.compile(&tirs, &flows, &pool, &cleanup, false)
        .expect("AOT emission");
    let object = temp.path().join(if cfg!(windows) {
        "binding.obj"
    } else {
        "binding.o"
    });
    std::fs::write(&object, aot.finish().expect("object emission")).expect("write object");
    let executable = temp
        .path()
        .join(format!("binding{}", std::env::consts::EXE_SUFFIX));
    let (runtime, runtime_lock) =
        ryo_backend::runtime_lib::extract_runtime_to_cache().expect("runtime archive");
    ryo_backend::linker::link_executable(
        &object,
        &executable,
        &runtime,
        ryo_backend::linker::LinkMode::Musl,
    )
    .expect("AOT link");
    drop(runtime_lock);
    let run = std::process::Command::new(executable)
        .output()
        .expect("run AOT binary");
    assert!(
        run.status.success(),
        "AOT execution failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    use std::io::Write as _;
    std::io::stdout()
        .write_all(&run.stdout)
        .expect("forward stdout");
}

fn run_binding_execution_child(case: usize) -> bool {
    let Ok(selected) = std::env::var("RYO_BINDING_EXECUTION_CASE") else {
        return false;
    };
    if selected != case.to_string() {
        return false;
    }
    let mode = std::env::var("RYO_BINDING_EXECUTION_MODE").expect("execution mode");
    let source = match case {
        0 => SHADOWED_LITERAL_CLEANUP,
        1 => SHADOWED_HEAP_CLEANUP,
        2 => LOOP_INOUT_BINDING_CLEANUP,
        _ => return false,
    };
    execute_binding_mode(source, &mode);
    true
}

fn assert_binding_execution_matrix(test_name: &str, case: usize, expected: &str) {
    for mode in ["jit-opt", "jit-unopt", "aot-opt", "aot-unopt"] {
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([test_name, "--exact", "--nocapture", "--test-threads=2"])
            .env("RYO_BINDING_EXECUTION_CASE", case.to_string())
            .env("RYO_BINDING_EXECUTION_MODE", mode)
            .output()
            .expect("spawn binding execution child");
        assert!(
            output.status.success(),
            "{mode} case {case} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let captured = String::from_utf8_lossy(&output.stdout);
        let success_marker = format!("test {test_name} ... ok");
        let program_output = captured
            .trim_start_matches(['\r', '\n'])
            .strip_prefix("running 1 test\n")
            .and_then(|output| output.split(&success_marker).next())
            .expect("successful libtest wrapper around program output");
        assert_eq!(program_output, expected, "{mode} case {case} stdout");
    }
}

#[test]
fn shadowed_owned_value_cleanup_matches_optimized_and_unoptimized() {
    if run_binding_execution_child(0) || run_binding_execution_child(1) {
        return;
    }
    assert_binding_execution_matrix(
        "shadowed_owned_value_cleanup_matches_optimized_and_unoptimized",
        0,
        "inner\nouter\n",
    );
    assert_binding_execution_matrix(
        "shadowed_owned_value_cleanup_matches_optimized_and_unoptimized",
        1,
        "inner_012345678901234567890123456789?\nouter_012345678901234567890123456789!\n",
    );
}

#[test]
fn loop_exit_and_inout_writeback_preserve_binding_identity() {
    if run_binding_execution_child(2) {
        return;
    }
    assert_binding_execution_matrix(
        "loop_exit_and_inout_writeback_preserve_binding_identity",
        2,
        "inner_0123456789012345678901234567890\ninner_0123456789012345678901234567891\n41\n2\nouter\n",
    );
}
