//! Shared game state, resources, and cross-module contracts.
//!
//! Every gameplay module (vehicle, scoring, camera, UI, FX, audio) reads and
//! writes these types only, keeping systems decoupled.

use bevy::prelude::*;
use core::persistence::Settings;
use core::score::ScoreState;
use core::sim::{Telemetry, WheelState};

/// Top-level game states.
#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AppState {
    #[default]
    Loading,
    Garage,
    Driving,
    Paused,
    Results,
}

/// Camera modes, cycled with `C`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CameraMode {
    #[default]
    Chase,
    Hood,
    Orbit,
}

#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct CameraState {
    pub mode: CameraMode,
    /// Force the camera to snap to target on next update (respawn/mode switch).
    pub snap_requested: bool,
}

/// Asset loading progress, written by the loading/asset module and read by the
/// loading screen. `total` may be zero while the asset list is still being
/// assembled; the UI treats that as "no measurable progress yet".
#[derive(Resource, Debug, Clone, Default)]
pub struct LoadingProgress {
    pub total: u32,
    pub loaded: u32,
    pub message: String,
    pub failed: Option<String>,
}

/// Game mode for the current session.
#[derive(Resource, Debug, Clone, Copy)]
pub struct GameMode {
    /// True for a 120 s scored run; false for free practice.
    pub timed: bool,
}

impl Default for GameMode {
    fn default() -> Self {
        GameMode { timed: false }
    }
}

/// Live vehicle state mirrored from the fixed-tick simulation for rendering,
/// camera, HUD, FX, and audio. Written by the vehicle plugin each physics
/// step; read everywhere else.
#[derive(Resource, Debug, Clone, Default)]
pub struct VehicleRuntime {
    pub telemetry: Telemetry,
    pub wheels: [WheelState; 4],
    pub rpm: f32,
    pub gear: i8,
    /// Smoothed steering angle (rad) for wheel visuals.
    pub steer_visual: f32,
    pub speed_kmh: f32,
    pub drift_angle_deg: f32,
    pub yaw_rate: f32,
    /// World-space chassis forward (planar, normalized) for camera work.
    pub forward: Vec3,
    /// World-space planar velocity.
    pub velocity: Vec3,
    /// Suspension length per wheel for visual offset.
    pub suspension: [f32; 4],
    pub car_entity: Option<Entity>,
}

impl VehicleRuntime {
    pub fn grounded_wheels(&self) -> u8 {
        self.telemetry.grounded_wheels
    }
}

/// Per-tick scoring state and timed-run progress.
#[derive(Resource, Debug, Clone, Default)]
pub struct ScoreRuntime {
    pub score: ScoreState,
    /// Clipping-zone proximity factor applied this tick (1.0 = none).
    pub zone_factor: f32,
    /// Seconds remaining in a timed run (valid when `run_active`).
    pub run_time_left: f32,
    pub run_active: bool,
    /// Set when a timed run finishes: final banked score.
    pub run_result: Option<RunResult>,
    /// True when this session is ineligible for personal bests (debug step or
    /// altered tuning).
    pub is_practice: bool,
    /// Set true briefly when a new personal best was recorded.
    pub new_best: bool,
}

#[derive(Debug, Clone, Default)]
pub struct RunResult {
    pub banked: f32,
    pub pending: f32,
    pub is_practice: bool,
    pub new_best: bool,
    pub practice_reason: Option<String>,
}

/// Pause/step control. Physics runs when Driving, or when stepping from pause.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct StepControl {
    /// Number of fixed ticks to advance while paused (debug single-step).
    pub pending_ticks: u32,
    /// True once debug stepping was used this session (marks run as practice).
    pub stepped_this_session: bool,
}

impl StepControl {
    pub fn request_step(&mut self) {
        self.pending_ticks = self.pending_ticks.saturating_add(1);
        self.stepped_this_session = true;
    }

    pub fn consume_step(&mut self) {
        self.pending_ticks = self.pending_ticks.saturating_sub(1);
    }
}

/// Settings + dirty flag; persistence plugin saves when dirty.
#[derive(Resource, Debug, Clone)]
pub struct GameSettings {
    pub settings: Settings,
    pub dirty: bool,
    /// Controls overlay shown at least once (persisted).
    pub controls_shown: bool,
    /// Per-car tuning modification flags (session-scoped, not persisted).
    pub tuning_modified: crate::persistence::TuningModified,
    /// Live tuning for the current car (bounded; 1.0 = preset defaults).
    pub tuning: core::tuning::LiveTuning,
}

/// One-shot control requests consumed by their owning systems.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct ControlRequests {
    /// Monotonic counters; consumers compare against their Local seq.
    pub reset: u64,
    pub camera_next: u64,
    pub toggle_telemetry: u64,
    pub toggle_forces: u64,
    pub toggle_contacts: u64,
}

/// One-shot impact signal for audio/camera shake. Consumers poll `seq`.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct ImpactSignal {
    pub strength: f32,
    pub seq: u64,
}

/// One-shot UI sound signal. Consumers poll `seq`.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct UiSoundSignal {
    pub seq: u64,
}

/// Toggle flags for the under-the-hood/debug overlay.
#[derive(Resource, Debug, Clone, Copy)]
pub struct DebugView {
    pub enabled: bool,
    pub show_forces: bool,
    pub show_contacts: bool,
}

impl Default for DebugView {
    fn default() -> Self {
        DebugView {
            enabled: false,
            show_forces: true,
            show_contacts: true,
        }
    }
}

/// Marker for the spawned player car root entity (rigid body).
#[derive(Component, Debug, Clone, Copy)]
pub struct PlayerCar;

/// Marker for the visual child root under the car (meshes + wheel pivots).
#[derive(Component, Debug, Clone, Copy)]
pub struct CarVisual;


/// Checkpoint poses for safe resets: position + yaw.
#[derive(Resource, Debug, Clone, Default)]
pub struct Checkpoints {
    pub poses: Vec<(Vec3, f32)>,
}

impl Checkpoints {
    /// Nearest checkpoint to a world position.
    pub fn nearest(&self, pos: Vec3) -> (Vec3, f32) {
        self.poses
            .iter()
            .copied()
            .min_by(|a, b| {
                a.0.distance_squared(pos)
                    .partial_cmp(&b.0.distance_squared(pos))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or((Vec3::new(0.0, 1.0, 0.0), 0.0))
    }
}

/// Reference to the currently loaded car visual info (for garage preview).
#[derive(Resource, Debug, Clone, Default)]
pub struct SelectedCar {
    pub car: Option<core::tuning::CarId>,
}

/// Course minimap texture + live car position for the HUD.
#[derive(Resource, Debug, Clone, Default)]
pub struct MinimapTexture {
    pub image: Option<bevy::asset::Handle<bevy::image::Image>>,
    pub car_uv: Vec2,
    pub car_forward: Vec2,
}

/// Physics timing statistics for the debug overlay.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct PhysicsTiming {
    /// Measured controller tick cost in ms (max over the last second).
    pub controller_ms: f32,
    /// Fixed ticks executed in the last render frame.
    pub ticks_last_frame: u32,
    /// Fixed ticks accumulated since the UI last sampled.
    pub ticks_pending: u32,
}
