//! Shared commands and process-bound Win32 telemetry with authenticated read-only consumers.

#![allow(clippy::missing_safety_doc)]

use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};

pub const TELEMETRY_MAP_NAME: &str = "Local\\WinMmAudioEndpointSharedV1\0";
pub const TELEMETRY_MAGIC: u32 = 0x574D_4D41; // "WMMA"
pub const TELEMETRY_VERSION: u32 = 962;
pub const SET_TOGGLE_BASE: i32 = 100;
pub const TOGGLE_COMMANDS: [i32; 8] = [32, 68, 69, 71, 72, 73, 74, 70];
/// Number of Faunas-port features exposed as individual toggles.
pub const FAUNAS_FEATURE_COUNT: usize = 24;
/// Explicit ON/OFF band for the Faunas-port features.
///
/// The band sits above the core command range so the existing command ids,
/// the operator phase marker and the historical telemetry layout keep their
/// meaning. An even command turns a feature off, an odd one turns it on.
pub const FAUNAS_TOGGLE_BASE: i32 = 200;
pub const FAUNAS_TOGGLE_MAX: i32 = FAUNAS_TOGGLE_BASE + (FAUNAS_FEATURE_COUNT as i32) * 2 - 1;
/// Highest command id accepted anywhere in the protocol.
pub const MAX_COMMAND: i32 = 356;
pub const CRIT_DAMAGE_BONUS_POINTS: i32 = 1_000;
/// Encodes native critical damage plus 1,000 percentage points in the existing IPC field.
pub const CRIT_DAMAGE_ADD_1000: i32 = -CRIT_DAMAGE_BONUS_POINTS;
pub const CRIT_DAMAGE_ADD_100: i32 = -100;
pub const CRIT_DAMAGE_ADD_200: i32 = -200;
pub const CRIT_DAMAGE_ADD_300: i32 = -300;
/// Ceiling on the critical damage multiplier, as a percentage ratio against
/// the native critical damage value.
///
/// The 2026-10-20 comparison accepted an absolute 200% at a native value of
/// 150 and rolled back 250% and above, including native +100, which is also
/// 250% at that native value. The bound is a ratio rather than an absolute
/// number because the native value varies by character and build: an absolute
/// cap would push a high-native character below its own value.
pub const CRIT_DAMAGE_MAX_RATIO: i32 = 135;
/// Skill-path critical damage cap, as a ratio percentage against native.
/// 100 is native; 135 is the measured acceptance ceiling.
pub const SKILL_CRIT_CAP_POINTS: i32 = 135;
/// MP recovery bonus ladder, in AMPR points over the current native value.
/// Live context 2026-10-19: native grants ~+13 server MP per accepted normal hit;
/// the old flat 2000 (cmd 74) reconciled every time and is retired.
pub const MP_AMPR_BONUS_10: i32 = 10;
pub const MP_AMPR_BONUS_25: i32 = 25;
pub const MP_AMPR_BONUS_50: i32 = 50;

// Active toggle bitmask flags
pub const TOGGLE_INVINCIBILITY: u32 = 1 << 0;
pub const TOGGLE_SPEED_BOOST: u32 = 1 << 1;
pub const TOGGLE_NO_COLLISION: u32 = 1 << 2;
pub const TOGGLE_RANGE_30M: u32 = 1 << 3;
pub const TOGGLE_INSTANT_ATK: u32 = 1 << 4;
pub const TOGGLE_ALWAYS_CRIT: u32 = 1 << 5;
pub const TOGGLE_UNLIMITED_MP: u32 = 1 << 6;
pub const TOGGLE_ANTI_RIGIDITY: u32 = 1 << 7;

/// Strongly-typed command IDs supported by `wcore`.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandId {
    StopWorker = 0,
    Probe = 1,
    FullHeal = 2,
    InvincibilityLegacy = 3,
    ProbeExtended = 21,
    Invincibility = 32,
    BlockedPrimaryStats = 44,
    SpeedBoost = 68,
    NoCollision = 69,
    ClearDelay = 70,
    AttackRange30m = 71,
    InstantAttack = 72,
    AlwaysCrit = 73,
    UnlimitedMp = 74,
    CritMult175 = 75,
    CritMult200 = 76,
    CritMult250 = 77,
    CritMult300 = 78,
    CritMultReset = 79,
    ObserveCombat = 80,
    CritDamageAdd1000 = 81,
    ObserveAttackResponses = 82,
    CritDamageAdd100 = 83,
    CritDamageAdd200 = 84,
    CritDamageAdd300 = 85,
    MpAmprPlus10 = 86,
    MpAmprPlus25 = 87,
    MpAmprPlus50 = 88,
    EnableCombatFeedback = 89,
    DisableCombatFeedback = 90,
    MpAmprNative = 91,
    ResetCombatMetrics = 92,
    StopPassiveObservation = 93,
    EnableAdaptiveLadder = 94,
    DisableAdaptiveLadder = 95,
    EnableAutoCadence = 96,
    DisableAutoCadence = 97,
    EnableAutoTargetLock = 98,
    DisableAutoTargetLock = 99,
    LastFeatureState = 115,
    /// Operator phase marker for the observation stream.
    ///
    /// Increments the phase id stamped onto every later observation sample, so
    /// a capture can be split into operator-named phases ("near attack",
    /// "far attack") without correlating by hand afterwards.
    SetObservationPhase = 116,
}

impl CommandId {
    /// Highest command id accepted by the loader and the worker.
    pub const MAX: i32 = Self::SetObservationPhase as i32;
    /// Highest command id inside the explicit toggle band.
    const TOGGLE_BAND_MAX: i32 = SET_TOGGLE_BASE + (TOGGLE_COMMANDS.len() as i32) * 2 - 1;

    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    pub const fn toggle_bit(self) -> Option<u32> {
        match self {
            Self::Invincibility | Self::InvincibilityLegacy => Some(TOGGLE_INVINCIBILITY),
            Self::SpeedBoost => Some(TOGGLE_SPEED_BOOST),
            Self::NoCollision => Some(TOGGLE_NO_COLLISION),
            Self::AttackRange30m => Some(TOGGLE_RANGE_30M),
            Self::InstantAttack => Some(TOGGLE_INSTANT_ATK),
            Self::AlwaysCrit => Some(TOGGLE_ALWAYS_CRIT),
            Self::UnlimitedMp => Some(TOGGLE_UNLIMITED_MP),
            Self::ClearDelay => Some(TOGGLE_ANTI_RIGIDITY),
            _ => None,
        }
    }

    pub const fn is_valid_range(cmd: i32) -> bool {
        matches!(cmd, 0..=3 | 21 | 32 | 44 | 68..=Self::MAX)
            || (cmd >= FAUNAS_TOGGLE_BASE && cmd <= FAUNAS_TOGGLE_MAX)
            || matches!(cmd, 300..=306 | 310..=320 | 330..=340 | 350..=356)
    }

    /// Decodes a Faunas-port command into its feature index and desired state.
    pub const fn faunas_feature_index(cmd: i32) -> Option<(usize, bool)> {
        if cmd < FAUNAS_TOGGLE_BASE || cmd > FAUNAS_TOGGLE_MAX {
            return None;
        }
        let index = ((cmd - FAUNAS_TOGGLE_BASE) / 2) as usize;
        Some((index, (cmd - FAUNAS_TOGGLE_BASE) & 1 != 0))
    }

    /// The explicit command that turns one Faunas-port feature on or off.
    pub const fn faunas_toggle_command(index: usize, enabled: bool) -> Option<i32> {
        if index >= FAUNAS_FEATURE_COUNT {
            return None;
        }
        Some(FAUNAS_TOGGLE_BASE + index as i32 * 2 + enabled as i32)
    }

    pub const fn unavailable_reason(cmd: i32) -> Option<&'static str> {
        match cmd {
            96 | 97 => Some("Automatic attack pacing is not implemented."),
            98 | 99 => Some("Automatic target acquisition is not implemented."),
            _ => None,
        }
    }

    /// Explicit OFF/ON commands are repeatable; unlike legacy toggles they never invert twice.
    pub const fn set_toggle_command(bit: u32, enabled: bool) -> Option<i32> {
        if bit == 0 || bit.count_ones() != 1 || bit > TOGGLE_ANTI_RIGIDITY {
            return None;
        }
        Some(SET_TOGGLE_BASE + bit.trailing_zeros() as i32 * 2 + enabled as i32)
    }

    pub const fn decode_toggle_command(cmd: i32) -> Option<(i32, u32, bool)> {
        if cmd < SET_TOGGLE_BASE || cmd > Self::TOGGLE_BAND_MAX {
            return None;
        }
        let index = ((cmd - SET_TOGGLE_BASE) / 2) as usize;
        Some((
            TOGGLE_COMMANDS[index],
            1 << index,
            (cmd - SET_TOGGLE_BASE) & 1 != 0,
        ))
    }

    pub const fn is_blocked(cmd: i32) -> bool {
        cmd == 44
    }
}

/// Atomic telemetry fields, exposed to the producer only inside a write transaction.
#[repr(C, align(64))]
#[derive(Default)]
pub struct TelemetryFields {
    pub magic: AtomicU32,
    pub version: AtomicU32,
    pub pid: AtomicU32,
    pub worker_state: AtomicI32,
    pub active_toggles: AtomicU32,
    /// Requested critical damage: 0=native, positive=absolute percent, negative=native+N points.
    pub crit_multiplier_pct: AtomicI32,
    /// Requested AMPR bonus in points over native; 0 = native recovery.
    pub mp_ampr_bonus: AtomicI32,
    pub invincibility_grant_id: AtomicI32,
    pub last_cmd: AtomicI32,
    pub last_status: AtomicI32,
    pub cmd_seq: AtomicU32,
    pub hook_hits: AtomicU64,
    pub last_damage: AtomicI32,
    pub now_hp: AtomicI32,
    pub max_hp: AtomicI32,
    pub now_mp: AtomicI32,
    pub max_mp: AtomicI32,
    pub stats: [AtomicI32; 6],
    pub resource_tick_ms: AtomicU64,
    pub combat_monitor: AtomicU32,
    pub responses_accepted: AtomicU64,
    pub responses_rejected: AtomicU64,
    pub responses_unknown: AtomicU64,
    pub last_received_hp: AtomicI32,
    pub last_violation: AtomicI32,
    pub skill_cap: AtomicI32,
    pub mp_hit_calls: AtomicU64,
    pub mp_getter_calls: AtomicU64,
    /// Callback activity since the last activation, not a gameplay acceptance claim.
    pub observed_toggles: AtomicU32,
    pub partial_toggles: AtomicU32,
    pub adaptive_enabled: AtomicU32,
    pub received_mp: AtomicI32,
    pub mp_sample_tick_ms: AtomicU64,
}

/// All fields participate in the same multi-writer sequence lock.
/// Wire version 962 adds activation evidence, partial coverage and received MP.
#[repr(C, align(64))]
#[derive(Default)]
pub struct SharedTelemetry {
    creator_pid: AtomicU32,
    creator_handle: AtomicU64,
    created_at: AtomicU64,
    sequence: AtomicU64,
    data: TelemetryFields,
}

const _: () = assert!(std::mem::align_of::<SharedTelemetry>() == 64);
const _: () = assert!(std::mem::size_of::<SharedTelemetry>().is_multiple_of(64));
/// Plain snapshot of `SharedTelemetry` for UI rendering and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TelemetrySnapshot {
    pub valid: bool,
    pub pid: u32,
    pub worker_state: i32,
    pub active_toggles: u32,
    /// Requested setting, not measured damage: 0=native, positive=percent, negative=additive points.
    pub crit_multiplier_pct: i32,
    pub mp_ampr_bonus: i32,
    pub invincibility_grant_id: i32,
    pub last_cmd: i32,
    pub last_status: i32,
    pub cmd_seq: u32,
    pub hook_hits: u64,
    pub last_damage: i32,
    pub now_hp: i32,
    pub max_hp: i32,
    pub now_mp: i32,
    pub max_mp: i32,
    pub stats: [i32; 6],
    pub resource_tick_ms: u64,
    pub combat_monitor: u32,
    pub responses_accepted: u64,
    pub responses_rejected: u64,
    pub responses_unknown: u64,
    pub last_received_hp: i32,
    pub last_violation: i32,
    pub skill_cap: i32,
    pub mp_hit_calls: u64,
    pub mp_getter_calls: u64,
    pub observed_toggles: u32,
    pub partial_toggles: u32,
    pub adaptive_enabled: u32,
    pub received_mp: i32,
    pub mp_sample_tick_ms: u64,
}

impl SharedTelemetry {
    /// Only hold this guard around atomic stores; never invoke game callbacks inside it.
    fn update(&self, write: impl FnOnce(&TelemetryFields)) {
        let sequence = loop {
            let seq = self.sequence.load(Ordering::SeqCst);
            if seq & 1 == 0
                && self
                    .sequence
                    .compare_exchange_weak(
                        seq,
                        seq.wrapping_add(1),
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_ok()
            {
                break seq;
            }
            core::hint::spin_loop();
        };
        struct Unlock<'a> {
            sequence: &'a AtomicU64,
            next: u64,
        }
        impl Drop for Unlock<'_> {
            fn drop(&mut self) {
                self.sequence.store(self.next, Ordering::SeqCst);
            }
        }
        let _unlock = Unlock {
            sequence: &self.sequence,
            next: sequence.wrapping_add(2),
        };
        write(&self.data);
    }

    pub fn init(&self, pid: u32) {
        self.update(|data| {
            if data.pid.load(Ordering::SeqCst) != pid
                || data.version.load(Ordering::SeqCst) != TELEMETRY_VERSION
            {
                data.resource_tick_ms.store(0, Ordering::SeqCst);
                data.combat_monitor.store(0, Ordering::SeqCst);
                data.responses_accepted.store(0, Ordering::SeqCst);
                data.responses_rejected.store(0, Ordering::SeqCst);
                data.responses_unknown.store(0, Ordering::SeqCst);
                data.last_received_hp.store(-1, Ordering::SeqCst);
                data.last_violation.store(0, Ordering::SeqCst);
                data.skill_cap
                    .store(SKILL_CRIT_CAP_POINTS, Ordering::SeqCst);
                data.mp_hit_calls.store(0, Ordering::SeqCst);
                data.mp_getter_calls.store(0, Ordering::SeqCst);
                data.observed_toggles.store(0, Ordering::SeqCst);
                data.partial_toggles.store(0, Ordering::SeqCst);
                data.adaptive_enabled.store(1, Ordering::SeqCst);
                data.received_mp.store(-1, Ordering::SeqCst);
                data.mp_sample_tick_ms.store(0, Ordering::SeqCst);
                data.magic.store(0, Ordering::SeqCst);
                data.active_toggles.store(0, Ordering::SeqCst);
                data.crit_multiplier_pct.store(0, Ordering::SeqCst);
                data.mp_ampr_bonus.store(0, Ordering::SeqCst);
                data.invincibility_grant_id.store(0, Ordering::SeqCst);
                data.hook_hits.store(0, Ordering::SeqCst);
                data.last_damage.store(0, Ordering::SeqCst);
                data.last_cmd.store(0, Ordering::SeqCst);
                data.last_status.store(0, Ordering::SeqCst);
                data.cmd_seq.store(0, Ordering::SeqCst);
                for value in [&data.now_hp, &data.max_hp, &data.now_mp, &data.max_mp] {
                    value.store(0, Ordering::SeqCst);
                }
                for value in &data.stats {
                    value.store(0, Ordering::SeqCst);
                }
            }
            data.pid.store(pid, Ordering::SeqCst);
            data.version.store(TELEMETRY_VERSION, Ordering::SeqCst);
            data.magic.store(TELEMETRY_MAGIC, Ordering::SeqCst);
        });
    }

    pub fn set_toggle_bit(&self, bit: u32, enabled: bool) {
        self.update(|data| {
            data.observed_toggles.fetch_and(!bit, Ordering::SeqCst);
            if enabled {
                data.active_toggles.fetch_or(bit, Ordering::SeqCst);
            } else {
                data.active_toggles.fetch_and(!bit, Ordering::SeqCst);
                data.partial_toggles.fetch_and(!bit, Ordering::SeqCst);
            }
        });
    }

    pub fn record_cmd_result(&self, pid: u32, cmd: i32, status: i32) {
        self.update(|data| {
            data.pid.store(pid, Ordering::SeqCst);
            data.last_cmd.store(cmd, Ordering::SeqCst);
            data.last_status.store(status, Ordering::SeqCst);
            data.cmd_seq.fetch_add(1, Ordering::SeqCst);
        });
    }

    pub fn snapshot(&self) -> TelemetrySnapshot {
        // A stalled producer must not hang the UI.
        for _ in 0..64 {
            let before = self.sequence.load(Ordering::SeqCst);
            if before & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let snap = self.load_snapshot();
            if self.sequence.load(Ordering::SeqCst) == before {
                return snap;
            }
        }
        TelemetrySnapshot::default()
    }

    fn load_snapshot(&self) -> TelemetrySnapshot {
        if self.data.magic.load(Ordering::SeqCst) != TELEMETRY_MAGIC
            || self.data.version.load(Ordering::SeqCst) != TELEMETRY_VERSION
        {
            return TelemetrySnapshot::default();
        }
        TelemetrySnapshot {
            valid: true,
            resource_tick_ms: self.data.resource_tick_ms.load(Ordering::SeqCst),
            combat_monitor: self.data.combat_monitor.load(Ordering::SeqCst),
            responses_accepted: self.data.responses_accepted.load(Ordering::SeqCst),
            responses_rejected: self.data.responses_rejected.load(Ordering::SeqCst),
            responses_unknown: self.data.responses_unknown.load(Ordering::SeqCst),
            last_received_hp: self.data.last_received_hp.load(Ordering::SeqCst),
            last_violation: self.data.last_violation.load(Ordering::SeqCst),
            skill_cap: self.data.skill_cap.load(Ordering::SeqCst),
            mp_hit_calls: self.data.mp_hit_calls.load(Ordering::SeqCst),
            mp_getter_calls: self.data.mp_getter_calls.load(Ordering::SeqCst),
            observed_toggles: self.data.observed_toggles.load(Ordering::SeqCst),
            partial_toggles: self.data.partial_toggles.load(Ordering::SeqCst),
            adaptive_enabled: self.data.adaptive_enabled.load(Ordering::SeqCst),
            received_mp: self.data.received_mp.load(Ordering::SeqCst),
            mp_sample_tick_ms: self.data.mp_sample_tick_ms.load(Ordering::SeqCst),
            pid: self.data.pid.load(Ordering::SeqCst),
            worker_state: self.data.worker_state.load(Ordering::SeqCst),
            active_toggles: self.data.active_toggles.load(Ordering::SeqCst),
            crit_multiplier_pct: self.data.crit_multiplier_pct.load(Ordering::SeqCst),
            mp_ampr_bonus: self.data.mp_ampr_bonus.load(Ordering::SeqCst),
            invincibility_grant_id: self.data.invincibility_grant_id.load(Ordering::SeqCst),
            last_cmd: self.data.last_cmd.load(Ordering::SeqCst),
            last_status: self.data.last_status.load(Ordering::SeqCst),
            cmd_seq: self.data.cmd_seq.load(Ordering::SeqCst),
            hook_hits: self.data.hook_hits.load(Ordering::SeqCst),
            last_damage: self.data.last_damage.load(Ordering::SeqCst),
            now_hp: self.data.now_hp.load(Ordering::SeqCst),
            max_hp: self.data.max_hp.load(Ordering::SeqCst),
            now_mp: self.data.now_mp.load(Ordering::SeqCst),
            max_mp: self.data.max_mp.load(Ordering::SeqCst),
            stats: [
                self.data.stats[0].load(Ordering::SeqCst),
                self.data.stats[1].load(Ordering::SeqCst),
                self.data.stats[2].load(Ordering::SeqCst),
                self.data.stats[3].load(Ordering::SeqCst),
                self.data.stats[4].load(Ordering::SeqCst),
                self.data.stats[5].load(Ordering::SeqCst),
            ],
        }
    }
}

mod ipc;
pub use ipc::{
    diagnose_telemetry, monotonic_ms, read_telemetry, telemetry, TelemetryDiagnosis,
    TelemetryStage, TelemetryWriter,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_evidence_is_cleared_between_runs_and_received_mp_stays_distinct() {
        let shm = SharedTelemetry::default();
        shm.init(1234);
        shm.set_toggle_bit(TOGGLE_UNLIMITED_MP, true);
        shm.update(|d| {
            d.observed_toggles
                .store(TOGGLE_UNLIMITED_MP, Ordering::SeqCst);
            d.partial_toggles
                .store(TOGGLE_UNLIMITED_MP, Ordering::SeqCst);
            d.now_mp.store(2000, Ordering::SeqCst);
            d.received_mp.store(13, Ordering::SeqCst);
            d.mp_sample_tick_ms.store(456, Ordering::SeqCst);
        });
        let s = shm.snapshot();
        assert_eq!(
            (s.now_mp, s.received_mp, s.mp_sample_tick_ms),
            (2000, 13, 456)
        );
        shm.set_toggle_bit(TOGGLE_UNLIMITED_MP, false);
        shm.set_toggle_bit(TOGGLE_UNLIMITED_MP, true);
        assert_eq!(shm.snapshot().observed_toggles, 0);
        assert_eq!(shm.snapshot().partial_toggles, 0);
        shm.init(2345);
        assert_eq!(shm.snapshot().received_mp, -1);
        assert_eq!(shm.snapshot().mp_sample_tick_ms, 0);
    }

    #[test]
    fn snapshots_never_mix_concurrent_resource_or_command_transactions() {
        let shm = std::sync::Arc::new(SharedTelemetry::default());
        shm.init(4242);
        let done = std::sync::Arc::new(AtomicU32::new(0));
        let mut writers = Vec::new();
        for worker in 0..2 {
            let shm = shm.clone();
            let done = done.clone();
            writers.push(std::thread::spawn(move || {
                for value in 1..20_000 {
                    let value = value + worker * 20_000;
                    shm.update(|data| {
                        data.now_hp.store(value, Ordering::SeqCst);
                        data.max_hp.store(value, Ordering::SeqCst);
                        data.now_mp.store(value, Ordering::SeqCst);
                        data.max_mp.store(value, Ordering::SeqCst);
                    });
                    shm.record_cmd_result(4242, value, -value);
                }
                done.fetch_add(1, Ordering::SeqCst);
            }));
        }
        while done.load(Ordering::SeqCst) != 2 {
            let snap = shm.snapshot();
            if snap.valid {
                assert_eq!(snap.now_hp, snap.max_hp);
                assert_eq!(snap.now_hp, snap.now_mp);
                assert_eq!(snap.now_mp, snap.max_mp);
                assert_eq!(snap.last_cmd, -snap.last_status);
            }
        }
        for writer in writers {
            writer.join().unwrap();
        }
        assert_eq!(shm.snapshot().cmd_seq, 39_998);
    }

    #[test]
    fn stalled_writer_returns_invalid_without_hanging_reader() {
        let shm = SharedTelemetry::default();
        shm.init(4242);
        shm.sequence.fetch_add(1, Ordering::SeqCst);
        assert!(!shm.snapshot().valid);
        shm.sequence.fetch_add(1, Ordering::SeqCst);
        assert!(shm.snapshot().valid);
    }

    #[test]
    fn test_shared_telemetry_roundtrip() {
        let shm = SharedTelemetry::default();
        shm.init(4242);
        assert_eq!(shm.snapshot().crit_multiplier_pct, 0);
        assert_eq!(shm.snapshot().crit_multiplier_pct, 0);
        assert_eq!(shm.snapshot().mp_ampr_bonus, 0);
        shm.update(|data| data.mp_ampr_bonus.store(MP_AMPR_BONUS_25, Ordering::SeqCst));
        shm.init(4242);
        assert_eq!(shm.snapshot().mp_ampr_bonus, MP_AMPR_BONUS_25);
        shm.update(|data| data.mp_ampr_bonus.store(0, Ordering::SeqCst));
        shm.set_toggle_bit(TOGGLE_INVINCIBILITY, true);
        shm.set_toggle_bit(TOGGLE_ALWAYS_CRIT, true);
        shm.update(|data| data.crit_multiplier_pct.store(250, Ordering::SeqCst));
        shm.init(4242);
        shm.update(|data| data.hook_hits.store(123, Ordering::SeqCst));
        shm.record_cmd_result(4242, 73, 0);

        let snap = shm.snapshot();
        assert!(snap.valid);
        assert_eq!(snap.pid, 4242);
        assert_ne!(snap.active_toggles & TOGGLE_INVINCIBILITY, 0);
        assert_ne!(snap.active_toggles & TOGGLE_ALWAYS_CRIT, 0);
        assert_eq!(snap.crit_multiplier_pct, 250);
        assert_eq!(snap.hook_hits, 123);
        assert_eq!(snap.last_cmd, 73);
        assert_eq!(snap.last_status, 0);

        shm.update(|data| {
            data.crit_multiplier_pct
                .store(CRIT_DAMAGE_ADD_1000, Ordering::SeqCst)
        });
        shm.init(4242);
        assert_eq!(shm.snapshot().crit_multiplier_pct, CRIT_DAMAGE_ADD_1000);

        // Clean up toggle bits
        shm.set_toggle_bit(TOGGLE_INVINCIBILITY, false);
        shm.set_toggle_bit(TOGGLE_ALWAYS_CRIT, false);
        shm.update(|data| data.crit_multiplier_pct.store(0, Ordering::SeqCst));
        shm.init(4242);
        assert_eq!(shm.snapshot().crit_multiplier_pct, 0);
    }

    #[test]
    fn faunas_command_band_round_trips_and_stays_inside_the_valid_range() {
        for index in 0..FAUNAS_FEATURE_COUNT {
            for enabled in [false, true] {
                let cmd = CommandId::faunas_toggle_command(index, enabled).unwrap();
                assert!(CommandId::is_valid_range(cmd));
                let (decoded_index, decoded_enabled) =
                    CommandId::faunas_feature_index(cmd).unwrap();
                assert_eq!(decoded_index, index);
                assert_eq!(decoded_enabled, enabled);
            }
        }
        assert!(CommandId::faunas_toggle_command(FAUNAS_FEATURE_COUNT, true).is_none());
        assert!(CommandId::faunas_feature_index(199).is_none());
        assert!(CommandId::faunas_feature_index(FAUNAS_TOGGLE_MAX + 1).is_none());
        // The band must not collide with the core command range or the
        // operator phase marker.
        const { assert!(FAUNAS_TOGGLE_BASE > CommandId::MAX) };
        assert!(!CommandId::is_valid_range(CommandId::MAX + 1));
        // The value bands are accepted and stay clear of the feature band.
        for cmd in [300, 306, 310, 319, 320, 330, 339, 340, 350, 356] {
            assert!(CommandId::is_valid_range(cmd));
        }
        assert!(!CommandId::is_valid_range(299));
        assert!(!CommandId::is_valid_range(307));
        assert!(!CommandId::is_valid_range(357));
    }

    #[test]
    fn unresolved_faunas_features_report_a_reason_instead_of_running() {
        // Slots 22/23 (cmd 244..247) now carry sustain farming (mp_cost +
        // skill_delay) with verified PC_v226 targets, so they run.
        assert!(CommandId::unavailable_reason(244).is_none());
        assert!(CommandId::unavailable_reason(245).is_none());
        assert!(CommandId::unavailable_reason(246).is_none());
        assert!(CommandId::unavailable_reason(247).is_none());
        assert!(CommandId::unavailable_reason(201).is_none());
    }

    #[test]
    fn test_command_id_classification() {
        assert!(CommandId::is_valid_range(0));
        assert!(CommandId::is_valid_range(73));
        assert!(!CommandId::is_valid_range(-1));
        assert!(CommandId::is_valid_range(
            CommandId::CritDamageAdd1000.as_i32()
        ));
        assert!(CommandId::is_valid_range(
            CommandId::ObserveAttackResponses.as_i32()
        ));
        assert!(CommandId::is_valid_range(
            CommandId::CritDamageAdd100.as_i32()
        ));
        assert!(CommandId::is_valid_range(
            CommandId::CritDamageAdd200.as_i32()
        ));
        assert!(CommandId::is_valid_range(
            CommandId::CritDamageAdd300.as_i32()
        ));
        assert_eq!(CommandId::MAX, 116);
        assert!(CommandId::is_valid_range(CommandId::MpAmprPlus10.as_i32()));
        assert!(CommandId::is_valid_range(CommandId::MpAmprPlus25.as_i32()));
        assert!(CommandId::is_valid_range(CommandId::MpAmprPlus50.as_i32()));
        assert!(!CommandId::is_valid_range(CommandId::MAX + 1));
        assert!(CommandId::is_blocked(44));
        assert_eq!(CommandId::AlwaysCrit.toggle_bit(), Some(TOGGLE_ALWAYS_CRIT));
        assert_eq!(
            CommandId::ClearDelay.toggle_bit(),
            Some(TOGGLE_ANTI_RIGIDITY)
        );
    }

    #[test]
    fn state_commands_roundtrip_all_feature_bits_and_new_commands_are_reachable() {
        for (index, &expected_cmd) in TOGGLE_COMMANDS.iter().enumerate() {
            for enabled in [false, true] {
                let bit = 1 << index;
                let cmd = CommandId::set_toggle_command(bit, enabled).unwrap();
                assert_eq!(
                    CommandId::decode_toggle_command(cmd),
                    Some((expected_cmd, bit, enabled))
                );
                assert!(CommandId::is_valid_range(cmd));
            }
        }
        assert!(CommandId::set_toggle_command(3, true).is_none());
        for cmd in [
            81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 94, 95, 96, 97, 98, 99,
        ] {
            assert!(CommandId::is_valid_range(cmd));
        }
        assert!(!CommandId::is_valid_range(117));
        assert!(CommandId::is_valid_range(
            CommandId::SetObservationPhase.as_i32()
        ));
        assert_eq!(
            CommandId::decode_toggle_command(CommandId::SetObservationPhase.as_i32()),
            None,
            "the phase marker must not be read as a toggle command"
        );
    }

    #[test]
    fn resource_age_is_not_refreshed_by_settings_or_command_receipts() {
        let shm = SharedTelemetry::default();
        shm.init(42);
        shm.update(|data| data.resource_tick_ms.store(12345, Ordering::SeqCst));
        shm.set_toggle_bit(TOGGLE_ALWAYS_CRIT, true);
        shm.record_cmd_result(42, 83, 0);
        assert_eq!(shm.snapshot().resource_tick_ms, 12345);
    }

    #[test]
    fn stale_build_and_previous_process_telemetry_are_not_reused() {
        let shm = SharedTelemetry::default();
        shm.init(4242);
        shm.set_toggle_bit(TOGGLE_ALWAYS_CRIT, true);
        shm.update(|data| data.crit_multiplier_pct.store(250, Ordering::SeqCst));
        shm.update(|data| data.hook_hits.store(99, Ordering::SeqCst));
        shm.update(|data| data.version.store(TELEMETRY_VERSION - 1, Ordering::SeqCst));
        assert!(!shm.snapshot().valid);
        shm.init(4242);
        assert!(shm.snapshot().valid);
        assert_eq!(shm.snapshot().active_toggles, 0);
        assert_eq!(shm.snapshot().crit_multiplier_pct, 0);
        shm.set_toggle_bit(TOGGLE_ALWAYS_CRIT, true);
        shm.init(4243);
        assert_eq!(shm.snapshot().pid, 4243);
        assert_eq!(shm.snapshot().active_toggles, 0);
        assert_eq!(shm.snapshot().hook_hits, 0);
    }

    #[test]
    fn test_ipc_identifiers_are_neutral() {
        let lower = TELEMETRY_MAP_NAME.to_ascii_lowercase();
        for forbidden in ["toram", "cheat", "hack", "wcore", "companion"] {
            assert!(
                !lower.contains(forbidden),
                "TELEMETRY_MAP_NAME must not contain '{}'",
                forbidden
            );
        }
        assert_ne!(TELEMETRY_MAGIC, 0x544F_524D, "Magic must not be 'TORM'");
    }
}
