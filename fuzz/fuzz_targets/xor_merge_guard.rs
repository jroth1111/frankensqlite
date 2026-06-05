#![no_main]

//! Fuzz the XOR merge safety guard (bd-pwyf0).
//!
//! Ensures `attempt_raw_xor_merge` and `enforce_raw_xor_merge_policy` never
//! allow raw XOR on SQLite-structured pages, regardless of policy or data.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use fsqlite_mvcc::xor_delta::{
    MergeSafetyError, WriteMergePolicy, attempt_raw_xor_merge, enforce_raw_xor_merge_policy,
};
use fsqlite_types::MergePageKind;

#[derive(Debug, Arbitrary)]
struct FuzzInput {
    page_kind_idx: u8,
    policy_idx: u8,
    base: Vec<u8>,
    delta_a: Vec<u8>,
    delta_b: Vec<u8>,
}

const ALL_KINDS: [MergePageKind; 8] = [
    MergePageKind::BtreeInteriorTable,
    MergePageKind::BtreeLeafTable,
    MergePageKind::BtreeInteriorIndex,
    MergePageKind::BtreeLeafIndex,
    MergePageKind::Overflow,
    MergePageKind::Freelist,
    MergePageKind::PointerMap,
    MergePageKind::Opaque,
];

const ALL_POLICIES: [WriteMergePolicy; 3] = [
    WriteMergePolicy::Off,
    WriteMergePolicy::Safe,
    WriteMergePolicy::LabUnsafe,
];

const STRUCTURED_KINDS: [MergePageKind; 7] = [
    MergePageKind::BtreeInteriorTable,
    MergePageKind::BtreeLeafTable,
    MergePageKind::BtreeInteriorIndex,
    MergePageKind::BtreeLeafIndex,
    MergePageKind::Overflow,
    MergePageKind::Freelist,
    MergePageKind::PointerMap,
];

fuzz_target!(|input: FuzzInput| {
    let page_kind = ALL_KINDS[input.page_kind_idx as usize % ALL_KINDS.len()];
    let policy = ALL_POLICIES[input.policy_idx as usize % ALL_POLICIES.len()];

    // --- enforce_raw_xor_merge_policy invariant ---
    // Structured page kinds must ALWAYS be rejected, regardless of policy.
    let enforce_result = enforce_raw_xor_merge_policy(page_kind, policy);
    if page_kind.is_sqlite_structured() {
        let err = enforce_result
            .as_ref()
            .expect_err("structured page must be rejected");
        match err {
            MergeSafetyError::RawXorForbidden { .. }
            | MergeSafetyError::LabUnsafeRejectedInRelease => {}
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    // Off and Safe policies must ALWAYS reject, even for Opaque.
    if policy == WriteMergePolicy::Off || policy == WriteMergePolicy::Safe {
        assert!(
            enforce_result.is_err(),
            "policy {policy:?} must reject even for {page_kind:?}"
        );
    }

    // --- attempt_raw_xor_merge invariant ---
    // Cap page sizes to avoid OOM in the fuzzer.
    if input.base.len() > 65536 || input.delta_a.len() > 65536 || input.delta_b.len() > 65536 {
        return;
    }

    let merge_result = attempt_raw_xor_merge(
        &input.base,
        &input.delta_a,
        &input.delta_b,
        page_kind,
        policy,
    );

    // For all structured page kinds: merge must fail with RawXorForbidden or
    // LabUnsafeRejectedInRelease — never succeed.
    for &kind in &STRUCTURED_KINDS {
        let result = attempt_raw_xor_merge(
            &input.base,
            &input.delta_a,
            &input.delta_b,
            kind,
            policy,
        );
        assert!(
            result.is_err(),
            "raw XOR merge must fail for structured page kind {kind:?} policy {policy:?}"
        );
    }

    // If Opaque + LabUnsafe succeeds in debug builds, verify XOR identity.
    if page_kind == MergePageKind::Opaque && policy == WriteMergePolicy::LabUnsafe {
        if let Ok(merged) = &merge_result {
            assert_eq!(merged.len(), input.base.len());
            for (i, (&b, (&da, &db))) in input
                .base
                .iter()
                .zip(input.delta_a.iter().zip(input.delta_b.iter()))
                .enumerate()
            {
                assert_eq!(
                    merged[i],
                    b ^ da ^ db,
                    "XOR identity violated at offset {i}"
                );
            }
        }
    }
});
