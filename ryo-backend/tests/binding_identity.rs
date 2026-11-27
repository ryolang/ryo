use ryo_backend::codegen::Codegen;
use ryo_core::compilation::CompilationContext;
use ryo_core::diag::{DiagSink, Severity};
use ryo_core::flow::cleanup::{CleanupPlan, ReleaseStorage};
use ryo_core::flow::fire_points::{FirePoints, FireVerdict};
use ryo_core::flow::{DropPosition, Flow};
use ryo_core::ownership::OwnershipSidecar;
use ryo_core::tir::{Tir, TirRef, TirTag};
use ryo_core::types::InternPool;
use tempfile::TempDir;

const LOOP_INOUT_BINDING_CLEANUP: &str = "fn bump(inout value: int):\n\tvalue += 1\n\nfn main():\n\tmut i = 40\n\tmut counter = 0\n\tvalue = \"outer_012345678901234567890123456789\" + \"!\"\n\tfor i in range(0, 2):\n\t\tmut value = \"inner_012345678901234567890123456789\" + int_to_str(i)\n\t\tmut j = 0\n\t\twhile j < 1:\n\t\t\tprint(value)\n\t\t\tprint(\"\\n\")\n\t\t\tj += 1\n\t\tbump(&counter)\n\tbump(&i)\n\tprint(i)\n\tprint(\"\\n\")\n\tprint(counter)\n\tprint(\"\\n\")\n\tprint(value)\n\tprint(\"\\n\")\n";

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
    let final_read = tir
        .instructions
        .iter()
        .enumerate()
        .skip(1)
        .rev()
        .find_map(|(index, inst)| match inst.data {
            ryo_core::tir::TirData::Var(id) if id == outer_id => {
                Some(TirRef::from_raw(index as u32))
            }
            _ => None,
        })
        .ok_or_else(|| "outer binding has no final read".to_string())?;
    let read_block = flow
        .step_block_of(final_read)
        .ok_or_else(|| "final outer read has no Flow step".to_string())?;
    let read_step = flow
        .step_index_of(final_read)
        .ok_or_else(|| "final outer read has no Flow ordinal".to_string())?;
    let read_ordinal = read_step - flow.blocks[read_block.0 as usize].steps_start;
    let read_rank = flow
        .forward_order()
        .position(|block| block == read_block)
        .ok_or_else(|| "final outer read block is not in forward order".to_string())?;
    let release_points = match FirePoints::new(flow).resolve(cleanup.drops[outer_drop].position) {
        FireVerdict::Resolved(point) => vec![point],
        FireVerdict::ResolvedPerArm(points) if !points.is_empty() => points,
        verdict => {
            return Err(format!(
                "outer release has no exact resolved fire point after final outer read: {verdict:?}"
            ));
        }
    };
    for point in release_points {
        let follows_read = if point.block == read_block {
            point.ordinal > read_ordinal
        } else {
            flow.forward_order()
                .position(|block| block == point.block)
                .is_some_and(|release_rank| release_rank > read_rank)
        };
        if !follows_read {
            return Err(format!(
                "outer release must resolve after final outer read {final_read:?}; read=({read_block:?}, {read_ordinal}), release=({:?}, {})",
                point.block, point.ordinal
            ));
        }
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

#[test]
fn outer_cleanup_assertion_rejects_release_before_final_read() {
    for optimized in [true, false] {
        let (tirs, flows, mut pool, mut cleanup, _context) =
            binding_execution_artifacts(LOOP_INOUT_BINDING_CLEANUP, optimized);
        let main = tirs
            .iter()
            .position(|tir| pool.str(tir.name) == "main")
            .expect("main body");
        let tir = &tirs[main];
        let value_name = pool.intern_str("value");
        let (outer_id, outer_initializer) = tir
            .instructions
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, inst)| inst.tag == TirTag::VarDecl)
            .map(|(index, _)| tir.var_decl_view(TirRef::from_raw(index as u32)))
            .filter(|view| tir.binding_name(view.id) == value_name)
            .map(|view| (view.id, view.initializer))
            .next()
            .expect("outer declaration");
        let final_read = tir
            .instructions
            .iter()
            .enumerate()
            .skip(1)
            .rev()
            .find_map(|(index, inst)| match inst.data {
                ryo_core::tir::TirData::Var(id) if id == outer_id => {
                    Some(TirRef::from_raw(index as u32))
                }
                _ => None,
            })
            .expect("final outer read");
        cleanup.functions[main]
            .drops
            .iter_mut()
            .find(|drop| drop.target == outer_initializer)
            .expect("outer release")
            .position = DropPosition::BeforeStep { inst: final_read };
        let error = require_inner_loop_exit_cleanup(&tirs, &flows, &mut pool, &cleanup)
            .expect_err("predicate accepted a release before the final outer read");
        assert!(error.contains("after final outer read"), "{error}");
    }
}

const SHADOWED_LITERAL_CLEANUP: &str = "fn main():\n\tvalue = \"outer\"\n\tif true:\n\t\tmut value = \"inner\"\n\t\tprint(value)\n\t\tprint(\"\\n\")\n\tprint(value)\n\tprint(\"\\n\")\n";
const SHADOWED_HEAP_CLEANUP: &str = "fn main():\n\tvalue = \"outer_012345678901234567890123456789\" + \"!\"\n\tfor i in range(0, 1):\n\t\tmut value = \"inner_012345678901234567890123456789\" + \"?\"\n\t\tprint(value)\n\t\tprint(\"\\n\")\n\tprint(value)\n\tprint(\"\\n\")\n";
const DUPLICATE_MOVE_PARAMS: &str = "fn dup(move x: str, x: str):\n\treturn\n\nfn main():\n\tdup(\"first_012345678901234567890123456789\" + \"!\", \"second_012345678901234567890123456789\" + \"?\")\n\tprint(\"ok\\n\")\n";
const DUPLICATE_MOVE_INOUT_PARAMS: &str = "fn dup(move x: str, inout x: str):\n\treturn\n\nfn main():\n\tmut second = \"second_012345678901234567890123456789\" + \"?\"\n\tdup(\"first_012345678901234567890123456789\" + \"!\", &second)\n\tprint(second)\n\tprint(\"\\n\")\n";

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
        3 => DUPLICATE_MOVE_PARAMS,
        4 => DUPLICATE_MOVE_INOUT_PARAMS,
        _ => return false,
    };
    execute_binding_mode(source, &mode);
    true
}

fn libtest_success_trailer_len(trailer: &str) -> Option<usize> {
    let rest =
        trailer.strip_prefix("\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; ")?;
    let (result, remainder) = rest.split_once("s\n\n")?;
    let (filtered, seconds) = result.split_once(" filtered out; finished in ")?;
    if filtered.parse::<usize>().is_err() || seconds.parse::<f64>().is_err() {
        return None;
    }
    Some(trailer.len() - remainder.len())
}

fn binding_program_output<'a>(captured: &'a str, test_name: &str) -> Result<&'a str, String> {
    let output = captured
        .trim_start_matches(['\r', '\n'])
        .strip_prefix("running 1 test\n")
        .ok_or_else(|| "missing single-test libtest header".to_string())?;
    let success_marker = format!("test {test_name} ... ok");
    let mut wrappers = output.match_indices(&success_marker).filter(|(index, _)| {
        libtest_success_trailer_len(&output[index + success_marker.len()..]).is_some()
    });
    let (wrapper, _) = wrappers
        .next()
        .ok_or_else(|| "missing valid libtest success wrapper".to_string())?;
    if wrappers.next().is_some() {
        return Err("multiple valid libtest success wrappers".into());
    }
    let trailer = &output[wrapper + success_marker.len()..];
    if libtest_success_trailer_len(trailer) != Some(trailer.len()) {
        return Err("output follows the libtest success trailer".into());
    }
    Ok(&output[..wrapper])
}

#[test]
fn stdout_parser_preserves_marker_like_program_output() {
    let marker = "test sample ... ok";
    let trailer = "\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.00s\n\n";
    let program = format!("{marker}\nactual\n");
    let captured = format!("running 1 test\n{program}{marker}{trailer}");
    assert_eq!(
        binding_program_output(&captured, "sample"),
        Ok(program.as_str())
    );
}

#[test]
fn stdout_parser_rejects_missing_duplicate_and_invalid_wrappers() {
    let marker = "test sample ... ok";
    let trailer = "\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.00s\n\n";
    assert!(binding_program_output("running 1 test\nactual\n", "sample").is_err());
    let duplicate = format!("running 1 test\n{marker}{trailer}actual\n{marker}{trailer}");
    assert!(binding_program_output(&duplicate, "sample").is_err());
    let invalid = format!("running 1 test\nactual\n{marker}\n\nnot a result trailer\n");
    assert!(binding_program_output(&invalid, "sample").is_err());
    let trailing = format!("running 1 test\nactual\n{marker}{trailer}extra\n");
    assert!(binding_program_output(&trailing, "sample").is_err());
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
        let program_output = binding_program_output(&captured, test_name)
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
        "inner_0123456789012345678901234567890\ninner_0123456789012345678901234567891\n41\n2\nouter_012345678901234567890123456789!\n",
    );
}

#[test]
fn duplicate_parameter_modes_preserve_cleanup_identity() {
    if run_binding_execution_child(3) || run_binding_execution_child(4) {
        return;
    }
    assert_binding_execution_matrix(
        "duplicate_parameter_modes_preserve_cleanup_identity",
        3,
        "ok\n",
    );
    assert_binding_execution_matrix(
        "duplicate_parameter_modes_preserve_cleanup_identity",
        4,
        "second_012345678901234567890123456789?\n",
    );
}
