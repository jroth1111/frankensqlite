use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};
use fsqlite_pager::{
    MvccPager, SimplePager, SimpleTransaction, TransactionHandle, TransactionMode,
};
use fsqlite_types::opcode::{Opcode, P4};
use fsqlite_types::record::{parse_record, serialize_record};
use fsqlite_types::value::SqliteValue;
use fsqlite_types::{Cx, PageNumber, PageSize};
use fsqlite_vdbe::engine::{MemDatabase, VdbeEngine, set_vdbe_jit_enabled};
use fsqlite_vdbe::{
    ProgramBuilder, VdbeProgram, profile_vdbe_commit_stage, profile_vdbe_decode_stage,
};
use fsqlite_vfs::MemoryVfs;
use std::path::Path;

const EXECUTE_STAGE_OP_REPEATS: [usize; 3] = [64, 256, 1024];
const COMMIT_STAGE_DIRTY_PAGES: [usize; 3] = [2, 8, 32];

fn decode_stage_row(column_count: usize) -> Vec<SqliteValue> {
    (0..column_count)
        .map(|idx| match idx % 3 {
            0 => SqliteValue::Integer(i64::try_from(idx * 97 + 11).unwrap()),
            1 => SqliteValue::Text(format!("decode-stage-{idx:03}").into()),
            _ => SqliteValue::Blob(vec![u8::try_from((idx % 251) + 1).unwrap(); 24].into()),
        })
        .collect()
}

fn build_execute_stage_program(op_repeats: usize) -> VdbeProgram {
    let mut builder = ProgramBuilder::new();
    let end = builder.emit_label();
    builder.emit_jump_to_label(Opcode::Init, 0, 0, end, P4::None, 0);
    let accumulator = builder.alloc_reg();
    builder.emit_op(Opcode::Integer, 0, accumulator, 0, P4::None, 0);
    for _ in 0..op_repeats {
        builder.emit_op(Opcode::AddImm, accumulator, 1, 0, P4::None, 0);
    }
    builder.emit_op(Opcode::Halt, 0, 0, 0, P4::None, 0);
    builder.resolve_label(end);
    builder
        .finish()
        .expect("pipeline execute benchmark program should build")
}

/// Build a dispatch-dominated program whose inner loop is a stream of
/// single-register `Copy` ops. The source register holds an `Integer`, so
/// the body reduces to `clone + set_reg_fast` per dispatch — the work is
/// small enough that the hot-path pre-filter vs main-match routing is the
/// dominant cost, which is exactly the effect we want to measure.
fn build_execute_stage_copy_program(op_repeats: usize) -> VdbeProgram {
    let mut builder = ProgramBuilder::new();
    let end = builder.emit_label();
    builder.emit_jump_to_label(Opcode::Init, 0, 0, end, P4::None, 0);
    let src = builder.alloc_reg();
    let dst = builder.alloc_reg();
    builder.emit_op(Opcode::Integer, 42, src, 0, P4::None, 0);
    for _ in 0..op_repeats {
        // p1=src, p2=dst, p3=0 (copy a single register)
        builder.emit_op(Opcode::Copy, src, dst, 0, P4::None, 0);
    }
    builder.emit_op(Opcode::Halt, 0, 0, 0, P4::None, 0);
    builder.resolve_label(end);
    builder
        .finish()
        .expect("pipeline execute copy benchmark program should build")
}

/// Build a dispatch-dominated program whose inner loop is a stream of
/// single-register `SCopy` (shallow-copy) ops. Like the Copy variant, the
/// source holds an `Integer`, so the body is `clone + set_reg_fast` per
/// dispatch — isolating the hot-path pre-filter vs main-match routing
/// cost for the SCopy arm specifically.
fn build_execute_stage_scopy_program(op_repeats: usize) -> VdbeProgram {
    let mut builder = ProgramBuilder::new();
    let end = builder.emit_label();
    builder.emit_jump_to_label(Opcode::Init, 0, 0, end, P4::None, 0);
    let src = builder.alloc_reg();
    let dst = builder.alloc_reg();
    builder.emit_op(Opcode::Integer, 42, src, 0, P4::None, 0);
    for _ in 0..op_repeats {
        builder.emit_op(Opcode::SCopy, src, dst, 0, P4::None, 0);
    }
    builder.emit_op(Opcode::Halt, 0, 0, 0, P4::None, 0);
    builder.resolve_label(end);
    builder
        .finish()
        .expect("pipeline execute scopy benchmark program should build")
}

/// Build a dispatch-dominated program whose inner loop is a stream of
/// `DecrJumpZero` ops (the canonical LIMIT counter opcode). The counter
/// is seeded with `op_repeats + 1` so every dispatched opcode hits the
/// decrement-and-fall-through path — none jump to the halt target,
/// giving a stable per-op cost that isolates the hot-path pre-filter
/// routing from the mostly-taken branch predictor.
fn build_execute_stage_decrjumpzero_program(op_repeats: usize) -> VdbeProgram {
    let mut builder = ProgramBuilder::new();
    let end = builder.emit_label();
    let halt = builder.emit_label();
    builder.emit_jump_to_label(Opcode::Init, 0, 0, end, P4::None, 0);
    let counter = builder.alloc_reg();
    // Seed so op_repeats decrements leave the counter at 1 (never zero,
    // so the jump is never taken).
    let seed = i32::try_from(op_repeats + 1).unwrap_or(i32::MAX);
    builder.emit_op(Opcode::Integer, seed, counter, 0, P4::None, 0);
    for _ in 0..op_repeats {
        builder.emit_jump_to_label(Opcode::DecrJumpZero, counter, 0, halt, P4::None, 0);
    }
    builder.resolve_label(halt);
    builder.emit_op(Opcode::Halt, 0, 0, 0, P4::None, 0);
    builder.resolve_label(end);
    builder
        .finish()
        .expect("pipeline execute decrjumpzero benchmark program should build")
}

/// Build a dispatch-dominated program whose inner loop is a stream of
/// `IfPos` ops (the canonical OFFSET counter opcode). Each op's p2 jump
/// target is the instruction immediately after it, so a "jump" is
/// semantically equivalent to a fall-through for execution sequencing
/// but still exercises the opcode's taken-branch body (register read,
/// subtract, write-back, pc reassignment). p3=1 makes each op decrement
/// the counter by one; counter is seeded with `op_repeats + 1` so the
/// val>0 branch is taken every iteration.
fn build_execute_stage_ifpos_program(op_repeats: usize) -> VdbeProgram {
    let mut builder = ProgramBuilder::new();
    let end = builder.emit_label();
    builder.emit_jump_to_label(Opcode::Init, 0, 0, end, P4::None, 0);
    let counter = builder.alloc_reg();
    let seed = i32::try_from(op_repeats + 1).unwrap_or(i32::MAX);
    builder.emit_op(Opcode::Integer, seed, counter, 0, P4::None, 0);
    for _ in 0..op_repeats {
        let next = builder.emit_label();
        // p1=counter, p3=1 (decrement by one), p2=next (the very next
        // instruction — so this is an always-taken, fall-through-style jump).
        builder.emit_jump_to_label(Opcode::IfPos, counter, 1, next, P4::None, 0);
        builder.resolve_label(next);
    }
    builder.emit_op(Opcode::Halt, 0, 0, 0, P4::None, 0);
    builder.resolve_label(end);
    builder
        .finish()
        .expect("pipeline execute ifpos benchmark program should build")
}

/// Build a dispatch-dominated program whose inner loop is a stream of
/// `IsNull` ops (the canonical NULL-test / NOT NULL-constraint opcode,
/// 87 codegen sites — highest-frequency unpromoted opcode at the time
/// this bench was added). The source register is seeded with `Null`,
/// so each op exercises the taken-branch path: `is_null` returns true
/// → `pc = op.p2`. Each op's p2 jump target is the instruction
/// immediately after it, so the always-taken jump is semantically
/// equivalent to a fall-through for execution sequencing but still
/// runs the real branch body. This isolates the hot-path pre-filter
/// vs main-match routing cost for the IsNull arm specifically.
fn build_execute_stage_isnull_program(op_repeats: usize) -> VdbeProgram {
    let mut builder = ProgramBuilder::new();
    let end = builder.emit_label();
    builder.emit_jump_to_label(Opcode::Init, 0, 0, end, P4::None, 0);
    let probe = builder.alloc_reg();
    builder.emit_op(Opcode::Null, 0, probe, 0, P4::None, 0);
    for _ in 0..op_repeats {
        let next = builder.emit_label();
        builder.emit_jump_to_label(Opcode::IsNull, probe, 0, next, P4::None, 0);
        builder.resolve_label(next);
    }
    builder.emit_op(Opcode::Halt, 0, 0, 0, P4::None, 0);
    builder.resolve_label(end);
    builder
        .finish()
        .expect("pipeline execute isnull benchmark program should build")
}

/// Build a dispatch-dominated program whose inner loop is a stream of
/// `Rowid` ops against a single positioned storage cursor.  The cursor
/// is opened on a one-row table and Rewound to the only row, so each
/// dispatched `Rowid` op runs the realistic body shape (one
/// `storage_cursors` HashMap probe + one `cursor.rowid` call + one
/// `set_reg_fast`) without any cursor motion in between.  This isolates
/// the hot-path pre-filter vs main-match routing cost for the `Rowid`
/// arm specifically — same shape pattern as the `IsNull`/`IfPos`
/// /`IfNot` benches, where the body is uniform across ops and dispatch
/// routing is the variable being measured.
fn build_execute_stage_rowid_program(root_page: i32, op_repeats: usize) -> VdbeProgram {
    let mut builder = ProgramBuilder::new();
    // The bytecode verifier requires Rewind p2 to be strictly `<
    // op_count`, so the Rewind EOF target points at a real instruction
    // — the program-end Halt — via a label resolved *at* that Halt.
    // The single-row table guarantees Rewind never takes the EOF branch
    // in the bench loop, but the verifier still demands a valid
    // in-bounds target.  Init is omitted: the engine starts at pc=0
    // unconditionally.
    let halt = builder.emit_label();
    builder.emit_op(Opcode::OpenWrite, 0, root_page, 0, P4::Int(1), 0);
    builder.emit_jump_to_label(Opcode::Rewind, 0, 0, halt, P4::None, 0);
    let r_out = builder.alloc_reg();
    for _ in 0..op_repeats {
        builder.emit_op(Opcode::Rowid, 0, r_out, 0, P4::None, 0);
    }
    builder.resolve_label(halt);
    builder.emit_op(Opcode::Halt, 0, 0, 0, P4::None, 0);
    builder
        .finish()
        .expect("pipeline execute rowid benchmark program should build")
}

/// Mirrors `build_execute_stage_rowid_program` but emits `IdxRowid`
/// in the body loop.  Both opcodes route through `cursor_rowid`, so
/// this isolates dispatch routing cost for the `IdxRowid` arm specifically
/// — same shape pattern as the Rowid bench.
fn build_execute_stage_idx_rowid_program(root_page: i32, op_repeats: usize) -> VdbeProgram {
    let mut builder = ProgramBuilder::new();
    let halt = builder.emit_label();
    builder.emit_op(Opcode::OpenWrite, 0, root_page, 0, P4::Int(1), 0);
    builder.emit_jump_to_label(Opcode::Rewind, 0, 0, halt, P4::None, 0);
    let r_out = builder.alloc_reg();
    for _ in 0..op_repeats {
        builder.emit_op(Opcode::IdxRowid, 0, r_out, 0, P4::None, 0);
    }
    builder.resolve_label(halt);
    builder.emit_op(Opcode::Halt, 0, 0, 0, P4::None, 0);
    builder
        .finish()
        .expect("pipeline execute idx_rowid benchmark program should build")
}

fn build_execute_stage_ifnot_program(op_repeats: usize) -> VdbeProgram {
    // Mirrors the IsNull builder's always-taken-jump shape: each
    // IfNot's p2 jump target is the immediately-next instruction, so
    // the body runs the real branch (falsy → take jump) but execution
    // sequencing stays linear.  The probe register is seeded to 0
    // (falsy) so the branch is always taken — same shape pattern as
    // ifpos/isnull, exercising the dispatch + body without polluting
    // the timing with side-effects.
    let mut builder = ProgramBuilder::new();
    let end = builder.emit_label();
    builder.emit_jump_to_label(Opcode::Init, 0, 0, end, P4::None, 0);
    let probe = builder.alloc_reg();
    builder.emit_op(Opcode::Integer, 0, probe, 0, P4::None, 0);
    for _ in 0..op_repeats {
        let next = builder.emit_label();
        builder.emit_jump_to_label(Opcode::IfNot, probe, 0, next, P4::None, 0);
        builder.resolve_label(next);
    }
    builder.emit_op(Opcode::Halt, 0, 0, 0, P4::None, 0);
    builder.resolve_label(end);
    builder
        .finish()
        .expect("pipeline execute ifnot benchmark program should build")
}

fn prepare_commit_stage_fixture(dirty_pages: usize) -> (Cx, SimpleTransaction<MemoryVfs>) {
    let cx = Cx::new();
    let pager = SimplePager::open_with_cx(
        &cx,
        MemoryVfs::new(),
        Path::new("/:memory:"),
        PageSize::DEFAULT,
    )
    .expect("pipeline commit benchmark should open pager");
    let mut txn = pager
        .begin(&cx, TransactionMode::Immediate)
        .expect("pipeline commit benchmark should begin transaction");
    let page_bytes = PageSize::DEFAULT.as_usize();
    txn.write_page(&cx, PageNumber::ONE, &vec![0xA5; page_bytes])
        .expect("pipeline commit benchmark should dirty page one");
    for page_idx in 1..dirty_pages {
        let page_no = txn
            .allocate_page(&cx)
            .expect("pipeline commit benchmark should allocate page");
        let fill = u8::try_from((page_idx % 251) + 1).unwrap();
        txn.write_page(&cx, page_no, &vec![fill; page_bytes])
            .expect("pipeline commit benchmark should dirty page");
    }
    (cx, txn)
}

fn bench_vdbe_decode_stage(c: &mut Criterion) {
    let mut group = c.benchmark_group("vdbe_pipeline_decode");

    for column_count in [8_usize, 32, 128] {
        let record = serialize_record(&decode_stage_row(column_count));
        group.throughput(Throughput::Bytes(
            u64::try_from(record.len()).unwrap_or(u64::MAX),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(column_count),
            &record,
            |b, record| {
                b.iter(|| {
                    let decoded = profile_vdbe_decode_stage(|| {
                        parse_record(black_box(record.as_slice()))
                            .expect("pipeline decode benchmark should parse record")
                    });
                    black_box(decoded);
                });
            },
        );
    }

    group.finish();
}

fn bench_vdbe_execute_stage(c: &mut Criterion) {
    set_vdbe_jit_enabled(false);
    let mut group = c.benchmark_group("vdbe_pipeline_execute");

    for op_repeats in EXECUTE_STAGE_OP_REPEATS {
        let program = build_execute_stage_program(op_repeats);
        group.throughput(Throughput::Elements(
            u64::try_from(op_repeats).unwrap_or(u64::MAX),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(op_repeats),
            &program,
            |b, program| {
                let execution_cx = Cx::new();
                let mut engine = VdbeEngine::new_with_execution_cx(
                    program.register_count(),
                    &execution_cx,
                    PageSize::DEFAULT,
                );
                engine.set_collect_result_rows(false);
                b.iter(|| {
                    let outcome = engine
                        .execute(program)
                        .expect("pipeline execute benchmark should execute");
                    black_box(outcome);
                });
            },
        );
    }

    group.finish();
}

fn bench_vdbe_execute_copy_stage(c: &mut Criterion) {
    set_vdbe_jit_enabled(false);
    let mut group = c.benchmark_group("vdbe_pipeline_execute_copy");

    for op_repeats in EXECUTE_STAGE_OP_REPEATS {
        let program = build_execute_stage_copy_program(op_repeats);
        group.throughput(Throughput::Elements(
            u64::try_from(op_repeats).unwrap_or(u64::MAX),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(op_repeats),
            &program,
            |b, program| {
                let execution_cx = Cx::new();
                let mut engine = VdbeEngine::new_with_execution_cx(
                    program.register_count(),
                    &execution_cx,
                    PageSize::DEFAULT,
                );
                engine.set_collect_result_rows(false);
                b.iter(|| {
                    let outcome = engine
                        .execute(program)
                        .expect("pipeline execute copy benchmark should execute");
                    black_box(outcome);
                });
            },
        );
    }

    group.finish();
}

fn bench_vdbe_execute_scopy_stage(c: &mut Criterion) {
    set_vdbe_jit_enabled(false);
    let mut group = c.benchmark_group("vdbe_pipeline_execute_scopy");

    for op_repeats in EXECUTE_STAGE_OP_REPEATS {
        let program = build_execute_stage_scopy_program(op_repeats);
        group.throughput(Throughput::Elements(
            u64::try_from(op_repeats).unwrap_or(u64::MAX),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(op_repeats),
            &program,
            |b, program| {
                let execution_cx = Cx::new();
                let mut engine = VdbeEngine::new_with_execution_cx(
                    program.register_count(),
                    &execution_cx,
                    PageSize::DEFAULT,
                );
                engine.set_collect_result_rows(false);
                b.iter(|| {
                    let outcome = engine
                        .execute(program)
                        .expect("pipeline execute scopy benchmark should execute");
                    black_box(outcome);
                });
            },
        );
    }

    group.finish();
}

fn bench_vdbe_execute_decrjumpzero_stage(c: &mut Criterion) {
    set_vdbe_jit_enabled(false);
    let mut group = c.benchmark_group("vdbe_pipeline_execute_decrjumpzero");

    for op_repeats in EXECUTE_STAGE_OP_REPEATS {
        let program = build_execute_stage_decrjumpzero_program(op_repeats);
        group.throughput(Throughput::Elements(
            u64::try_from(op_repeats).unwrap_or(u64::MAX),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(op_repeats),
            &program,
            |b, program| {
                let execution_cx = Cx::new();
                let mut engine = VdbeEngine::new_with_execution_cx(
                    program.register_count(),
                    &execution_cx,
                    PageSize::DEFAULT,
                );
                engine.set_collect_result_rows(false);
                b.iter(|| {
                    let outcome = engine
                        .execute(program)
                        .expect("pipeline execute decrjumpzero benchmark should execute");
                    black_box(outcome);
                });
            },
        );
    }

    group.finish();
}

fn bench_vdbe_execute_ifpos_stage(c: &mut Criterion) {
    set_vdbe_jit_enabled(false);
    let mut group = c.benchmark_group("vdbe_pipeline_execute_ifpos");

    for op_repeats in EXECUTE_STAGE_OP_REPEATS {
        let program = build_execute_stage_ifpos_program(op_repeats);
        group.throughput(Throughput::Elements(
            u64::try_from(op_repeats).unwrap_or(u64::MAX),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(op_repeats),
            &program,
            |b, program| {
                let execution_cx = Cx::new();
                let mut engine = VdbeEngine::new_with_execution_cx(
                    program.register_count(),
                    &execution_cx,
                    PageSize::DEFAULT,
                );
                engine.set_collect_result_rows(false);
                b.iter(|| {
                    let outcome = engine
                        .execute(program)
                        .expect("pipeline execute ifpos benchmark should execute");
                    black_box(outcome);
                });
            },
        );
    }

    group.finish();
}

fn bench_vdbe_execute_isnull_stage(c: &mut Criterion) {
    set_vdbe_jit_enabled(false);
    let mut group = c.benchmark_group("vdbe_pipeline_execute_isnull");

    for op_repeats in EXECUTE_STAGE_OP_REPEATS {
        let program = build_execute_stage_isnull_program(op_repeats);
        group.throughput(Throughput::Elements(
            u64::try_from(op_repeats).unwrap_or(u64::MAX),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(op_repeats),
            &program,
            |b, program| {
                let execution_cx = Cx::new();
                let mut engine = VdbeEngine::new_with_execution_cx(
                    program.register_count(),
                    &execution_cx,
                    PageSize::DEFAULT,
                );
                engine.set_collect_result_rows(false);
                b.iter(|| {
                    let outcome = engine
                        .execute(program)
                        .expect("pipeline execute isnull benchmark should execute");
                    black_box(outcome);
                });
            },
        );
    }

    group.finish();
}

fn bench_vdbe_execute_rowid_stage(c: &mut Criterion) {
    set_vdbe_jit_enabled(false);
    let mut group = c.benchmark_group("vdbe_pipeline_execute_rowid");

    for op_repeats in EXECUTE_STAGE_OP_REPEATS {
        // Build a fresh single-row MemDatabase per param so each bench
        // run gets an independent root-page id (engine takes ownership
        // of the database).  Rowid bodies hit `storage_cursors` after
        // OpenWrite/Rewind position the cursor on the only row.
        let mut db = MemDatabase::new();
        let root = db.create_table(1);
        db.get_table_mut(root)
            .expect("table should exist")
            .insert_row(1, vec![SqliteValue::Integer(42)]);
        let program = build_execute_stage_rowid_program(root, op_repeats);

        group.throughput(Throughput::Elements(
            u64::try_from(op_repeats).unwrap_or(u64::MAX),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(op_repeats),
            &(program, db),
            |b, (program, db)| {
                let execution_cx = Cx::new();
                let mut engine = VdbeEngine::new_with_execution_cx(
                    program.register_count(),
                    &execution_cx,
                    PageSize::DEFAULT,
                );
                engine.set_collect_result_rows(false);
                engine.enable_storage_cursors(true);
                engine.set_database(db.clone());
                engine.set_reject_mem_fallback(false);
                b.iter(|| {
                    let outcome = engine
                        .execute(program)
                        .expect("pipeline execute rowid benchmark should execute");
                    black_box(outcome);
                });
            },
        );
    }

    group.finish();
}

fn bench_vdbe_execute_idx_rowid_stage(c: &mut Criterion) {
    set_vdbe_jit_enabled(false);
    let mut group = c.benchmark_group("vdbe_pipeline_execute_idx_rowid");

    for op_repeats in EXECUTE_STAGE_OP_REPEATS {
        let mut db = MemDatabase::new();
        let root = db.create_table(1);
        db.get_table_mut(root)
            .expect("table should exist")
            .insert_row(1, vec![SqliteValue::Integer(42)]);
        let program = build_execute_stage_idx_rowid_program(root, op_repeats);

        group.throughput(Throughput::Elements(
            u64::try_from(op_repeats).unwrap_or(u64::MAX),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(op_repeats),
            &(program, db),
            |b, (program, db)| {
                let execution_cx = Cx::new();
                let mut engine = VdbeEngine::new_with_execution_cx(
                    program.register_count(),
                    &execution_cx,
                    PageSize::DEFAULT,
                );
                engine.set_collect_result_rows(false);
                engine.enable_storage_cursors(true);
                engine.set_database(db.clone());
                engine.set_reject_mem_fallback(false);
                b.iter(|| {
                    let outcome = engine
                        .execute(program)
                        .expect("pipeline execute idx_rowid benchmark should execute");
                    black_box(outcome);
                });
            },
        );
    }

    group.finish();
}

fn bench_vdbe_execute_ifnot_stage(c: &mut Criterion) {
    set_vdbe_jit_enabled(false);
    let mut group = c.benchmark_group("vdbe_pipeline_execute_ifnot");

    for op_repeats in EXECUTE_STAGE_OP_REPEATS {
        let program = build_execute_stage_ifnot_program(op_repeats);
        group.throughput(Throughput::Elements(
            u64::try_from(op_repeats).unwrap_or(u64::MAX),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(op_repeats),
            &program,
            |b, program| {
                let execution_cx = Cx::new();
                let mut engine = VdbeEngine::new_with_execution_cx(
                    program.register_count(),
                    &execution_cx,
                    PageSize::DEFAULT,
                );
                engine.set_collect_result_rows(false);
                b.iter(|| {
                    let outcome = engine
                        .execute(program)
                        .expect("pipeline execute ifnot benchmark should execute");
                    black_box(outcome);
                });
            },
        );
    }

    group.finish();
}

fn bench_vdbe_commit_stage(c: &mut Criterion) {
    let mut group = c.benchmark_group("vdbe_pipeline_commit");

    for dirty_pages in COMMIT_STAGE_DIRTY_PAGES {
        group.throughput(Throughput::Elements(
            u64::try_from(dirty_pages).unwrap_or(u64::MAX),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(dirty_pages),
            &dirty_pages,
            |b, &dirty_pages| {
                b.iter_batched(
                    || prepare_commit_stage_fixture(dirty_pages),
                    |(cx, mut txn)| {
                        profile_vdbe_commit_stage(|| {
                            txn.commit(&cx)
                                .expect("pipeline commit benchmark should commit");
                        });
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn build_execute_stage_next_program(root_page: i32) -> VdbeProgram {
    let mut builder = ProgramBuilder::new();
    let halt = builder.emit_label();
    builder.emit_op(Opcode::OpenWrite, 0, root_page, 0, P4::Int(1), 0);
    builder.emit_jump_to_label(Opcode::Rewind, 0, 0, halt, P4::None, 0);
    let body = i32::try_from(builder.current_addr()).expect("body addr fits i32");
    builder.emit_op(Opcode::Next, 0, body, 0, P4::None, 0);
    builder.resolve_label(halt);
    builder.emit_op(Opcode::Halt, 0, 0, 0, P4::None, 0);
    builder
        .finish()
        .expect("pipeline execute next benchmark program should build")
}

fn bench_vdbe_execute_next_stage(c: &mut Criterion) {
    set_vdbe_jit_enabled(false);
    let mut group = c.benchmark_group("vdbe_pipeline_execute_next");

    for op_repeats in EXECUTE_STAGE_OP_REPEATS {
        let mut db = MemDatabase::new();
        let root = db.create_table(1);
        let table = db.get_table_mut(root).expect("table should exist");
        for i in 1..=op_repeats {
            table.insert_row(
                i64::try_from(i).unwrap_or(1),
                vec![SqliteValue::Integer(i64::try_from(i).unwrap_or(0))],
            );
        }
        let program = build_execute_stage_next_program(root);

        group.throughput(Throughput::Elements(
            u64::try_from(op_repeats).unwrap_or(u64::MAX),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(op_repeats),
            &(program, db),
            |b, (program, db)| {
                let execution_cx = Cx::new();
                let mut engine = VdbeEngine::new_with_execution_cx(
                    program.register_count(),
                    &execution_cx,
                    PageSize::DEFAULT,
                );
                engine.set_collect_result_rows(false);
                engine.set_database(db.clone());
                engine.set_reject_mem_fallback(false);
                b.iter(|| {
                    let outcome = engine
                        .execute(program)
                        .expect("pipeline execute next benchmark should execute");
                    black_box(outcome);
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_vdbe_decode_stage,
    bench_vdbe_execute_stage,
    bench_vdbe_execute_copy_stage,
    bench_vdbe_execute_scopy_stage,
    bench_vdbe_execute_decrjumpzero_stage,
    bench_vdbe_execute_ifpos_stage,
    bench_vdbe_execute_isnull_stage,
    bench_vdbe_execute_ifnot_stage,
    bench_vdbe_execute_rowid_stage,
    bench_vdbe_execute_idx_rowid_stage,
    bench_vdbe_execute_next_stage,
    bench_vdbe_commit_stage
);
criterion_main!(benches);
