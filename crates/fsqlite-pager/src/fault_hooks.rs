//! Targeted fault hooks for group-commit publish verification.

use std::mem;
use std::sync::{LazyLock, Mutex};

use fsqlite_error::{FrankenError, Result};
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultHookArm {
    pub run_id: String,
    pub scenario_id: String,
    pub invariant_family: String,
}

impl FaultHookArm {
    #[must_use]
    pub fn new(
        run_id: impl Into<String>,
        scenario_id: impl Into<String>,
        invariant_family: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            scenario_id: scenario_id.into(),
            invariant_family: invariant_family.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultInjectionRecord {
    pub trigger_seq: u64,
    pub point: &'static str,
    pub run_id: String,
    pub scenario_id: String,
    pub invariant_family: String,
    pub detail: String,
}

#[derive(Debug, Default)]
struct PagerFaultHookState {
    next_trigger_seq: u64,
    after_flush_before_publish: Option<FaultHookArm>,
    during_phase_c: Option<FaultHookArm>,
    drop_condvar_notify: Option<FaultHookArm>,
    records: Vec<FaultInjectionRecord>,
}

static PAGER_FAULT_HOOK_STATE: LazyLock<Mutex<PagerFaultHookState>> =
    LazyLock::new(|| Mutex::new(PagerFaultHookState::default()));

pub fn clear() {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *state = PagerFaultHookState::default();
}

#[must_use]
pub fn take_records() -> Vec<FaultInjectionRecord> {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    mem::take(&mut state.records)
}

pub fn arm_after_flush_before_publish(arm: FaultHookArm) {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.after_flush_before_publish = Some(arm);
}

pub(crate) fn maybe_inject_after_flush_before_publish(
    flush_epoch: u64,
    batch_count: usize,
    frame_count: usize,
) -> Result<()> {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(arm) = state.after_flush_before_publish.take() else {
        return Ok(());
    };

    let detail =
        format!("flush_epoch={flush_epoch} batch_count={batch_count} frame_count={frame_count}");
    record_trigger(&mut state, &arm, "after_flush_before_publish", detail);
    Err(FrankenError::Io(std::io::Error::other(format!(
        "fault_inject:after_flush_before_publish run_id={} scenario_id={} invariant_family={}",
        arm.run_id, arm.scenario_id, arm.invariant_family
    ))))
}

/// Arm the Phase-C publication fault hook (F4 / H4).
///
/// When armed, `maybe_inject_during_phase_c()` fires once inside
/// `SimpleTransaction::commit()` after commit_seq is updated but before
/// snapshot publish completes. Simulates crash during metadata publication.
pub fn arm_during_phase_c(arm: FaultHookArm) {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.during_phase_c = Some(arm);
}

/// Check and fire the Phase-C publication fault hook.
pub(crate) fn maybe_inject_during_phase_c(commit_seq: u64, db_size: u32) -> Result<()> {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(arm) = state.during_phase_c.take() else {
        return Ok(());
    };

    let detail = format!("commit_seq={commit_seq} db_size={db_size}");
    record_trigger(&mut state, &arm, "during_phase_c", detail);
    Err(FrankenError::Io(std::io::Error::other(format!(
        "fault_inject:during_phase_c run_id={} scenario_id={} invariant_family={}",
        arm.run_id, arm.scenario_id, arm.invariant_family
    ))))
}

/// Arm the dropped-condvar-notify fault hook (F11 / H11).
///
/// When armed, `maybe_inject_drop_condvar_notify()` returns true,
/// signaling the caller to suppress the `Condvar::notify_all()` that normally
/// follows a successful `publish_completed_epoch()` store.  The completed epoch
/// is still published; only the wakeup is dropped so waiters must recover via
/// timeout-based rechecks.
pub fn arm_drop_condvar_notify(arm: FaultHookArm) {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.drop_condvar_notify = Some(arm);
}

/// Check and fire the dropped-condvar-notify hook.
///
/// Returns `true` if the hook fires (caller should suppress the notify).
pub(crate) fn maybe_inject_drop_condvar_notify(completed_epoch: u64) -> bool {
    let mut state = PAGER_FAULT_HOOK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(arm) = state.drop_condvar_notify.take() else {
        return false;
    };

    let detail = format!("completed_epoch={completed_epoch}");
    record_trigger(&mut state, &arm, "drop_condvar_notify", detail);
    true
}

fn record_trigger(
    state: &mut PagerFaultHookState,
    arm: &FaultHookArm,
    point: &'static str,
    detail: String,
) {
    state.next_trigger_seq = state.next_trigger_seq.saturating_add(1);
    let record = FaultInjectionRecord {
        trigger_seq: state.next_trigger_seq,
        point,
        run_id: arm.run_id.clone(),
        scenario_id: arm.scenario_id.clone(),
        invariant_family: arm.invariant_family.clone(),
        detail,
    };
    warn!(
        target: "fsqlite_pager::fault_injection",
        trigger_seq = record.trigger_seq,
        point = record.point,
        run_id = %record.run_id,
        scenario_id = %record.scenario_id,
        invariant_family = %record.invariant_family,
        detail = %record.detail,
        "fault hook fired"
    );
    state.records.push(record);
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_GUARD: Mutex<()> = Mutex::new(());

    fn arm(point: &str) -> FaultHookArm {
        FaultHookArm::new(format!("test-{point}"), format!("scenario-{point}"), "unit")
    }

    #[test]
    fn test_clear_resets_all_armed_hooks_and_records() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        arm_after_flush_before_publish(arm("flush"));
        arm_during_phase_c(arm("phase_c"));
        arm_drop_condvar_notify(arm("condvar"));
        let _ = maybe_inject_after_flush_before_publish(1, 1, 1);
        assert_eq!(take_records().len(), 1);

        clear();
        assert!(
            maybe_inject_after_flush_before_publish(1, 1, 1).is_ok(),
            "cleared flush hook must not fire"
        );
        assert!(
            maybe_inject_during_phase_c(1, 1).is_ok(),
            "cleared phase_c hook must not fire"
        );
        assert!(
            !maybe_inject_drop_condvar_notify(1),
            "cleared condvar hook must not fire"
        );
        assert!(take_records().is_empty(), "clear must reset records");
    }

    #[test]
    fn test_armed_hook_fires_once_then_disarms() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        arm_during_phase_c(arm("phase_c"));
        let err = maybe_inject_during_phase_c(42, 10);
        assert!(err.is_err(), "armed hook should return Err on first call");
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("fault_inject:during_phase_c"),
        );

        assert!(
            maybe_inject_during_phase_c(43, 11).is_ok(),
            "disarmed hook must not fire on second call"
        );
    }

    #[test]
    fn test_drop_condvar_notify_returns_true_once() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        arm_drop_condvar_notify(arm("condvar"));
        assert!(maybe_inject_drop_condvar_notify(1), "first call fires");
        assert!(
            !maybe_inject_drop_condvar_notify(2),
            "second call does not fire"
        );
    }

    #[test]
    fn test_records_capture_trigger_details() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        arm_after_flush_before_publish(FaultHookArm::new("run-1", "scen-1", "inv-1"));
        let _ = maybe_inject_after_flush_before_publish(7, 3, 12);

        arm_during_phase_c(FaultHookArm::new("run-2", "scen-2", "inv-2"));
        let _ = maybe_inject_during_phase_c(99, 50);

        let records = take_records();
        assert_eq!(records.len(), 2);

        assert_eq!(records[0].point, "after_flush_before_publish");
        assert_eq!(records[0].run_id, "run-1");
        assert!(records[0].detail.contains("flush_epoch=7"));
        assert!(records[0].detail.contains("batch_count=3"));
        assert!(records[0].detail.contains("frame_count=12"));

        assert_eq!(records[1].point, "during_phase_c");
        assert_eq!(records[1].run_id, "run-2");
        assert!(records[1].detail.contains("commit_seq=99"));
        assert!(records[1].detail.contains("db_size=50"));

        assert_eq!(records[0].trigger_seq, 1);
        assert_eq!(records[1].trigger_seq, 2);
    }

    #[test]
    fn test_flush_hook_fires_once_then_disarms() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        arm_after_flush_before_publish(arm("flush"));
        let err = maybe_inject_after_flush_before_publish(5, 2, 8);
        assert!(err.is_err());
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("fault_inject:after_flush_before_publish"),
        );

        assert!(
            maybe_inject_after_flush_before_publish(6, 3, 9).is_ok(),
            "disarmed flush hook must not fire on second call"
        );
    }

    #[test]
    fn test_condvar_notify_record_captures_detail() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        arm_drop_condvar_notify(FaultHookArm::new("run-cv", "scen-cv", "inv-cv"));
        assert!(maybe_inject_drop_condvar_notify(77));

        let records = take_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].point, "drop_condvar_notify");
        assert_eq!(records[0].run_id, "run-cv");
        assert_eq!(records[0].scenario_id, "scen-cv");
        assert_eq!(records[0].invariant_family, "inv-cv");
        assert!(records[0].detail.contains("completed_epoch=77"));
    }

    #[test]
    fn test_all_hooks_fire_independently() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        arm_after_flush_before_publish(arm("flush"));
        arm_during_phase_c(arm("phase_c"));
        arm_drop_condvar_notify(arm("condvar"));

        assert!(maybe_inject_after_flush_before_publish(1, 1, 1).is_err());
        assert!(maybe_inject_during_phase_c(2, 2).is_err());
        assert!(maybe_inject_drop_condvar_notify(3));

        let records = take_records();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].point, "after_flush_before_publish");
        assert_eq!(records[1].point, "during_phase_c");
        assert_eq!(records[2].point, "drop_condvar_notify");
        assert_eq!(records[0].trigger_seq, 1);
        assert_eq!(records[1].trigger_seq, 2);
        assert_eq!(records[2].trigger_seq, 3);
    }

    #[test]
    fn test_unarmed_hooks_are_noop() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        assert!(maybe_inject_after_flush_before_publish(1, 1, 1).is_ok());
        assert!(maybe_inject_during_phase_c(1, 1).is_ok());
        assert!(!maybe_inject_drop_condvar_notify(1));
        assert!(take_records().is_empty());
    }

    #[test]
    fn test_fault_hook_arm_equality() {
        let a = FaultHookArm::new("r1", "s1", "inv1");
        let b = FaultHookArm::new("r1", "s1", "inv1");
        let c = FaultHookArm::new("r2", "s1", "inv1");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_take_records_drains_and_is_empty_after() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        arm_drop_condvar_notify(arm("condvar"));
        let _ = maybe_inject_drop_condvar_notify(1);

        let first = take_records();
        assert_eq!(first.len(), 1);
        let second = take_records();
        assert!(second.is_empty(), "take_records must drain");
    }

    #[test]
    fn test_rearming_overwrites_previous_arm() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        arm_during_phase_c(FaultHookArm::new("first", "s1", "inv1"));
        arm_during_phase_c(FaultHookArm::new("second", "s2", "inv2"));

        let _ = maybe_inject_during_phase_c(1, 1);
        let records = take_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "second", "re-arm must overwrite first");
    }

    #[test]
    fn test_trigger_seq_monotonic_across_cycles() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        arm_after_flush_before_publish(arm("flush"));
        let _ = maybe_inject_after_flush_before_publish(1, 1, 1);
        let r1 = take_records();

        arm_during_phase_c(arm("phase_c"));
        let _ = maybe_inject_during_phase_c(2, 2);
        let r2 = take_records();

        assert!(
            r2[0].trigger_seq > r1[0].trigger_seq,
            "trigger_seq must increase across take_records drains"
        );
    }

    #[test]
    fn test_fault_hook_arm_new_maps_fields_correctly() {
        let a = FaultHookArm::new("my-run", "my-scenario", "my-invariant");
        assert_eq!(a.run_id, "my-run");
        assert_eq!(a.scenario_id, "my-scenario");
        assert_eq!(a.invariant_family, "my-invariant");
    }

    #[test]
    fn test_fault_injection_record_fields_from_condvar_hook() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        arm_drop_condvar_notify(FaultHookArm::new("r", "s", "i"));
        assert!(maybe_inject_drop_condvar_notify(999));

        let records = take_records();
        assert_eq!(records.len(), 1);
        let rec = &records[0];
        assert_eq!(rec.point, "drop_condvar_notify");
        assert_eq!(rec.run_id, "r");
        assert_eq!(rec.scenario_id, "s");
        assert_eq!(rec.invariant_family, "i");
        assert!(rec.detail.contains("completed_epoch=999"));
        assert!(rec.trigger_seq > 0);
    }

    #[test]
    fn test_fault_injection_record_clone_and_eq() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        arm_during_phase_c(FaultHookArm::new("rc", "sc", "ic"));
        let _ = maybe_inject_during_phase_c(42, 7);
        let records = take_records();
        let original = &records[0];
        let cloned = original.clone();
        assert_eq!(*original, cloned);
        assert_eq!(cloned.trigger_seq, original.trigger_seq);
        assert_eq!(cloned.point, "during_phase_c");
        assert_eq!(cloned.detail, original.detail);
    }

    #[test]
    fn test_fault_hook_arm_clone_and_debug() {
        let a = FaultHookArm::new("run-dbg", "scen-dbg", "inv-dbg");
        let b = a.clone();
        assert_eq!(a, b);
        let dbg = format!("{a:?}");
        assert!(dbg.contains("run-dbg"));
        assert!(dbg.contains("scen-dbg"));
        assert!(dbg.contains("inv-dbg"));
    }

    #[test]
    fn test_flush_error_message_includes_all_arm_fields() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        arm_after_flush_before_publish(FaultHookArm::new("R1", "S1", "INV1"));
        let err = maybe_inject_after_flush_before_publish(1, 1, 1).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("run_id=R1"), "error must include run_id");
        assert!(msg.contains("scenario_id=S1"), "error must include scenario_id");
        assert!(
            msg.contains("invariant_family=INV1"),
            "error must include invariant_family"
        );
    }

    #[test]
    fn test_flush_hook_boundary_zero_counts() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();

        arm_after_flush_before_publish(arm("zero"));
        let err = maybe_inject_after_flush_before_publish(0, 0, 0);
        assert!(err.is_err());
        let records = take_records();
        assert_eq!(records.len(), 1);
        assert!(records[0].detail.contains("flush_epoch=0"));
        assert!(records[0].detail.contains("batch_count=0"));
        assert!(records[0].detail.contains("frame_count=0"));
    }
}
