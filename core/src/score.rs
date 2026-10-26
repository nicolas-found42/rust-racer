//! Telemetry-driven drift scoring. All state advances per fixed tick; angle is
//! computed in radians internally and converted explicitly for thresholds/UI.
//!
//! Contract (spec-exact):
//! - Qualifying drift: speed >= 20 km/h, |drift angle| in [10 deg, 65 deg],
//!   grounded tire contact.
//! - Base points per tick: 100 * speed_factor * angle_factor * fixed_dt,
//!   speed_factor = clamp(speed_kmh / 60, 0, 2),
//!   angle_factor = clamp((|angle_deg| - 10) / 35, 0, 1).
//! - Combo multiplier +1 after every 2 s of qualifying drift, capped at 5x.
//!   Each qualifying increment is scored with the current multiplier.
//! - A direction transition may interrupt qualification for up to 0.75 s
//!   without banking the combo (no points in the gap).
//! - A longer gap banks pending into session total.
//! - Hard collision, reset, or spin beyond 90 deg while moving above minimum
//!   speed discards pending and resets combo.
//! - Clipping-zone proximity factor capped at 1.25x, only while qualifying.

use crate::FIXED_DT;

pub const MIN_SPEED_KMH: f32 = 20.0;
pub const MIN_ANGLE_DEG: f32 = 10.0;
pub const MAX_ANGLE_DEG: f32 = 65.0;
pub const SPIN_ANGLE_DEG: f32 = 90.0;
pub const MULTIPLIER_STEP_SECONDS: f32 = 2.0;
/// MULTIPLIER_STEP_SECONDS expressed in exact fixed ticks at 120 Hz.
pub const MULTIPLIER_STEP_TICKS: u32 = (MULTIPLIER_STEP_SECONDS / FIXED_DT).round() as u32;
pub const MAX_MULTIPLIER: u32 = 5;
pub const DIRECTION_GRACE_SECONDS: f32 = 0.75;
pub const PROXIMITY_MAX_FACTOR: f32 = 1.25;

/// Timed-run duration in seconds.
pub const RUN_DURATION_SECONDS: f32 = 120.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftSide {
    Left,
    Right,
    None,
}

/// A single scored event accumulated during one tick.
#[derive(Debug, Clone, Copy, Default)]
pub struct TickScore {
    /// Points added to the pending combo this tick (already multiplied).
    pub pending_added: f32,
}

#[derive(Debug, Clone)]
pub struct ScoreState {
    /// Banked points this session (survived combos).
    pub banked: f32,
    /// Points accumulated in the current (unbanked) combo, multiplier applied.
    pub pending: f32,
    pub multiplier: u32,
    /// Qualifying-drift ticks accumulated at the current multiplier step.
    /// Tick counting (not float seconds) so the 2 s step is exact at 120 Hz.
    pub multiplier_ticks: u32,
    /// Consecutive qualifying time (for HUD drift chain time).
    pub drift_time: f32,
    /// Uninterrupted qualifying streak length this combo (seconds).
    pub gap_timer: f32,
    pub side: DriftSide,
    /// True while the current tick qualifies.
    pub qualifying: bool,
    /// Total accumulated session points (banked only; pending banks in later).
    pub best_banked: f32,
}

impl Default for ScoreState {
    fn default() -> Self {
        ScoreState {
            banked: 0.0,
            pending: 0.0,
            multiplier: 1,
            multiplier_ticks: 0,
            drift_time: 0.0,
            gap_timer: 0.0,
            side: DriftSide::None,
            qualifying: false,
            best_banked: 0.0,
        }
    }
}

impl ScoreState {
    pub fn total_display(&self) -> f32 {
        self.banked + self.pending
    }

    /// Advance scoring by one fixed tick.
    ///
    /// * `speed_kmh` — planar speed from the authoritative sim state.
    /// * `angle_deg` — signed drift angle in degrees (|x| in 10..=65 qualifies).
    /// * `grounded` — at least one tire grounded.
    /// * `zone_factor` — precomputed proximity factor (1.0 = none, max 1.25);
    ///   applied only during qualifying ticks.
    pub fn tick(
        &mut self,
        speed_kmh: f32,
        angle_deg: f32,
        grounded: bool,
        zone_factor: f32,
    ) -> TickScore {
        let abs_angle = angle_deg.abs();
        let qualifies = grounded
            && speed_kmh >= MIN_SPEED_KMH
            && abs_angle >= MIN_ANGLE_DEG
            && abs_angle <= MAX_ANGLE_DEG;

        self.qualifying = qualifies;
        let mut added = 0.0;

        if qualifies {
            let side = if angle_deg > 0.0 {
                DriftSide::Left
            } else {
                DriftSide::Right
            };
            // Direction transition: opposite side vs. the combo's current side.
            // The combo keeps its original side; a transition is tolerated in
            // the gap timer (counted, not scored).
            if self.side == DriftSide::None {
                self.side = side;
            } else if side != self.side {
                // Direction flip: no points this tick; combo not banked yet.
                self.gap_timer += FIXED_DT;
                self.drift_time += FIXED_DT;
                return TickScore { pending_added: 0.0 };
            }

            self.gap_timer = 0.0;
            self.drift_time += FIXED_DT;
            self.multiplier_ticks += 1;
            while self.multiplier_ticks >= MULTIPLIER_STEP_TICKS && self.multiplier < MAX_MULTIPLIER
            {
                self.multiplier_ticks -= MULTIPLIER_STEP_TICKS;
                self.multiplier += 1;
            }

            let speed_factor = (speed_kmh / 60.0).clamp(0.0, 2.0);
            let angle_factor = ((abs_angle - MIN_ANGLE_DEG) / 35.0).clamp(0.0, 1.0);
            let prox = zone_factor.clamp(1.0, PROXIMITY_MAX_FACTOR);
            let base = 100.0 * speed_factor * angle_factor * FIXED_DT;
            added = base * self.multiplier as f32 * prox;
            self.pending += added;
        } else {
            self.gap_timer += FIXED_DT;
            if self.gap_timer > DIRECTION_GRACE_SECONDS {
                // Long enough gap: bank the pending combo and reset.
                self.bank_pending();
            }
        }

        TickScore {
            pending_added: added,
        }
    }

    fn bank_pending(&mut self) {
        if self.pending > 0.0 {
            self.banked += self.pending;
            self.pending = 0.0;
        }
        self.reset_combo();
    }

    /// End-of-drift cleanup: combo banked or voided; timers reset.
    pub fn reset_combo(&mut self) {
        self.multiplier = 1;
        self.multiplier_ticks = 0;
        self.drift_time = 0.0;
        self.gap_timer = 0.0;
        self.side = DriftSide::None;
    }

    /// Hard failure: collision beyond threshold, reset, or a spin past 90 deg
    /// while above minimum speed. Pending points are discarded, combo lost.
    pub fn invalidate_combo(&mut self) {
        self.pending = 0.0;
        self.reset_combo();
    }

    /// Bank immediately (e.g. timed run expiry banks surviving pending).
    pub fn force_bank(&mut self) {
        self.bank_pending();
    }

    /// Session bookkeeping after a bank (updates best-so-far for results).
    pub fn note_best(&mut self) {
        self.best_banked = self.best_banked.max(self.banked);
    }

    /// Full session reset (retry): clears everything including banked score.
    pub fn reset_session(&mut self) {
        *self = ScoreState::default();
    }

    /// Spin detection per spec: >90 deg while moving above minimum speed.
    pub fn check_spin(&self, angle_deg: f32, speed_kmh: f32) -> bool {
        angle_deg.abs() > SPIN_ANGLE_DEG && speed_kmh > MIN_SPEED_KMH
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FIXED_DT;
    const EPS: f32 = 1e-4;

    fn qualify_ticks(s: &mut ScoreState, n: u32, speed_kmh: f32, angle_deg: f32) -> f32 {
        let mut total = 0.0;
        for _ in 0..n {
            let t = s.tick(speed_kmh, angle_deg, true, 1.0);
            total += t.pending_added;
        }
        total
    }

    #[test]
    fn below_min_speed_scores_nothing() {
        let mut s = ScoreState::default();
        let added = qualify_ticks(&mut s, 240, 19.9, 30.0);
        assert!(added.abs() < EPS && s.pending.abs() < EPS);
    }

    #[test]
    fn below_min_angle_scores_nothing() {
        let mut s = ScoreState::default();
        let added = qualify_ticks(&mut s, 240, 60.0, 9.9);
        assert!(s.pending.abs() < EPS);
        assert!(added.abs() < EPS);
    }

    #[test]
    fn above_max_angle_scores_nothing() {
        let mut s = ScoreState::default();
        qualify_ticks(&mut s, 240, 60.0, 65.1);
        assert!(s.pending.abs() < EPS);
    }

    #[test]
    fn base_points_match_formula() {
        // speed 60 km/h -> speed_factor 1.0; 45 deg -> angle_factor (45-10)/35 = 1.0
        let mut s = ScoreState::default();
        let t = s.tick(60.0, 45.0, true, 1.0);
        let expected = 100.0 * 1.0 * 1.0 * FIXED_DT * 1.0;
        assert!(
            (t.pending_added - expected).abs() < EPS,
            "{} vs {}",
            t.pending_added,
            expected
        );
    }

    #[test]
    fn speed_factor_clamped_at_2() {
        let mut s = ScoreState::default();
        let t = s.tick(300.0, 45.0, true, 1.0);
        let expected = 100.0 * 2.0 * 1.0 * FIXED_DT;
        assert!((t.pending_added - expected).abs() < EPS);
    }

    #[test]
    fn multiplier_increases_every_two_seconds_caps_at_five() {
        let mut s = ScoreState::default();
        // 6 seconds of qualifying at 120 Hz = 720 ticks.
        qualify_ticks(&mut s, 240, 60.0, 30.0);
        assert_eq!(s.multiplier, 2);
        qualify_ticks(&mut s, 240, 60.0, 30.0);
        assert_eq!(s.multiplier, 3);
        qualify_ticks(&mut s, 2400, 60.0, 30.0); // +20 s
        assert_eq!(s.multiplier, MAX_MULTIPLIER);
    }

    #[test]
    fn short_gap_keeps_pending() {
        let mut s = ScoreState::default();
        qualify_ticks(&mut s, 240, 60.0, 30.0);
        let pending_before = s.pending;
        // 0.5 s of non-qualifying gap (< 0.75 s grace).
        qualify_ticks(&mut s, 60, 10.0, 30.0);
        assert_eq!(s.pending, pending_before);
        // Resumes scoring on the same combo.
        let t = qualify_ticks(&mut s, 1, 60.0, 30.0);
        assert!(t > 0.0);
    }

    #[test]
    fn long_gap_banks_pending() {
        let mut s = ScoreState::default();
        qualify_ticks(&mut s, 240, 60.0, 30.0);
        let pending = s.pending;
        assert!(pending > 0.0);
        qualify_ticks(&mut s, 120, 10.0, 30.0); // 1 s gap > grace
        assert_eq!(s.banked, pending);
        assert_eq!(s.pending, 0.0);
        assert_eq!(s.multiplier, 1);
    }

    #[test]
    fn gap_in_direction_transition_earns_nothing_but_keeps_combo() {
        let mut s = ScoreState::default();
        qualify_ticks(&mut s, 240, 60.0, 30.0); // left drift
        let before = s.pending;
        let before_mult = s.multiplier;
        // Flip direction: scored as gap ticks, no points.
        let t = qualify_ticks(&mut s, 24, 60.0, -30.0);
        assert!(t.abs() < EPS);
        assert_eq!(s.pending, before);
        assert_eq!(s.multiplier, before_mult);
    }

    #[test]
    fn spin_beyond_90_invalidates() {
        let mut s = ScoreState::default();
        qualify_ticks(&mut s, 240, 60.0, 30.0);
        assert!(s.pending > 0.0);
        assert!(s.check_spin(95.0, 40.0));
        s.invalidate_combo();
        assert_eq!(s.pending, 0.0);
        assert_eq!(s.multiplier, 1);
    }

    #[test]
    fn slow_spin_is_not_invalidating() {
        let mut s = ScoreState::default();
        assert!(!s.check_spin(95.0, 15.0));
    }

    #[test]
    fn zone_factor_applies_only_when_qualifying_and_capped() {
        let mut s = ScoreState::default();
        let t = s.tick(60.0, 45.0, true, 1.25);
        let expected = 100.0 * 1.0 * 1.0 * FIXED_DT * 1.25;
        assert!((t.pending_added - expected).abs() < EPS);
        // Factor when not qualifying has no effect.
        let t2 = s.tick(10.0, 45.0, true, 1.25);
        assert_eq!(t2.pending_added, 0.0);
        // Over-cap factor is clamped.
        let mut s2 = ScoreState::default();
        let t3 = s2.tick(60.0, 45.0, true, 3.0);
        let expected3 = 100.0 * FIXED_DT * PROXIMITY_MAX_FACTOR;
        assert!((t3.pending_added - expected3).abs() < EPS);
    }

    #[test]
    fn force_bank_survives_expiry() {
        let mut s = ScoreState::default();
        qualify_ticks(&mut s, 240, 60.0, 30.0);
        let pending = s.pending;
        s.force_bank();
        assert_eq!(s.banked, pending);
        assert_eq!(s.pending, 0.0);
    }

    #[test]
    fn grounded_required() {
        let mut s = ScoreState::default();
        let added = qualify_ticks_with_ground(&mut s, 120, 60.0, 30.0, false);
        assert!(added.abs() < EPS);
        assert!(!s.qualifying);
    }

    fn qualify_ticks_with_ground(
        s: &mut ScoreState,
        n: u32,
        speed: f32,
        angle: f32,
        grounded: bool,
    ) -> f32 {
        let mut total = 0.0;
        for _ in 0..n {
            total += s.tick(speed, angle, grounded, 1.0).pending_added;
        }
        total
    }

    #[test]
    fn angle_wraparound_via_signed_input() {
        // The sim supplies signed angle in (-180,180]; wraparound is handled by
        // normalization upstream. Scoring sees e.g. 179 deg as non-qualifying.
        let mut s = ScoreState::default();
        s.tick(60.0, 179.0, true, 1.0);
        assert!(!s.qualifying);
        // A normalized -170 deg also fails (|angle| > 65).
        s.tick(60.0, -170.0, true, 1.0);
        assert!(!s.qualifying);
    }
}
