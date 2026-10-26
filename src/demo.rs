//! Env-gated demo harness (`RACER_DEMO=1`).
//!
//! Skips the first-launch controls overlay, auto-selects a car, enters free
//! practice, and scripts driving inputs. This gives a repeatable smoke run
//! for verification and CI without anyone at the keyboard; it is inert
//! unless the environment variable is set.

use crate::game_state::*;
use avian3d::prelude::*;
use bevy::prelude::*;
use core::sim::DriverInput;

/// True when the demo harness should drive the game.
pub fn enabled() -> bool {
    std::env::var("RACER_DEMO").as_deref() == Ok("1")
}

#[derive(Resource, Default)]
struct DemoState {
    last_cam_flip: f64,
}

/// Cycle camera modes while driving so automated screenshots cover the car
/// from multiple angles (chase, slow orbit, hood).
fn demo_camera_cycle(
    time: Res<Time>,
    mut state: ResMut<DemoState>,
    mode: Res<State<AppState>>,
    mut camera: ResMut<CameraState>,
) {
    if *mode != AppState::Driving {
        return;
    }
    let now = time.elapsed_secs_f64();
    if now - state.last_cam_flip < 6.0 {
        return;
    }
    state.last_cam_flip = now;
    camera.mode = match camera.mode {
        CameraMode::Chase => CameraMode::Orbit,
        CameraMode::Orbit => CameraMode::Hood,
        CameraMode::Hood => CameraMode::Chase,
    };
    camera.snap_requested = true;
}

pub struct DemoPlugin;

impl Plugin for DemoPlugin {
    fn build(&self, app: &mut App) {
        if !enabled() {
            return;
        }
        app.init_resource::<DemoState>()
            .add_systems(Startup, demo_setup)
            .add_systems(Update, (demo_enter_drive, demo_telemetry, demo_auto_reset, demo_camera_cycle));
        info!("demo harness enabled (RACER_DEMO=1)");
    }
}

/// One line per second: car position, speed, grounded wheels, so headless
/// smoke runs expose the simulation state without a human watching. Also
/// auto-enables the under-the-hood overlay so screenshots carry contact and
/// force gizmos.
fn demo_telemetry(
    mut next_log: Local<f64>,
    time: Res<Time>,
    runtime: Res<VehicleRuntime>,
    car: Query<&Position, With<PlayerCar>>,
    mut debug: ResMut<DebugView>,
) {
    if !debug.enabled {
        debug.enabled = true;
    }
    let now = time.elapsed_secs_f64();
    if now < *next_log {
        return;
    }
    *next_log = now + 1.0;
    let pos = car.single().map(|p| p.0).unwrap_or_default();
    let speed_kmh = runtime.telemetry.speed * 3.6;
    info!(
        "demo t={:.1}s pos=({:.1},{:.1},{:.1}) speed={:.1}km/h wheels_g={} rpm={:.0} angle={:.1}deg",
        now,
        pos.x,
        pos.y,
        pos.z,
        speed_kmh,
        runtime.telemetry.grounded_wheels,
        runtime.rpm,
        runtime.telemetry.drift_angle.to_degrees()
    );
}

/// Recover a beached demo car: if the car crawls for several seconds while
/// the script demands throttle, request a checkpoint reset.
fn demo_auto_reset(
    mut stuck_since: Local<Option<f64>>,
    time: Res<Time>,
    runtime: Res<VehicleRuntime>,
    mode: Res<State<AppState>>,
    car_q: Query<&Position, With<PlayerCar>>,
    mut reset: ResMut<crate::vehicle::ResetRequest>,
) {
    if *mode != AppState::Driving {
        *stuck_since = None;
        return;
    }
    let wants_motion = runtime.telemetry.throttle > 0.5;
    let stuck = wants_motion && runtime.telemetry.speed < 0.3;
    let now = time.elapsed_secs_f64();
    // Keep observation windows on the skidpad: reset when the car leaves the
    // disc (demo only — the park itself stays open).
    let car = car_q.single().map(|p| p.0).unwrap_or_default();
    if car.x * car.x + car.z * car.z > 42.0 * 42.0 {
        reset.seq += 1;
        reset.full = false;
        *stuck_since = None;
        info!("demo: left skidpad radius, auto-reset");
        return;
    }
    match (*stuck_since, stuck) {
        (None, true) => *stuck_since = Some(now),
        (Some(_), false) => *stuck_since = None,
        (Some(s), true) => {
            if now - s > 3.0 {
                reset.seq += 1;
                reset.full = false;
                *stuck_since = None;
                info!("demo: car beached, auto-reset");
            }
        }
        (None, false) => {}
    }
}

/// Skip the first-launch controls overlay; keep the change unsaved so the
/// demo never alters persisted settings.
fn demo_setup(mut settings: Option<ResMut<GameSettings>>) {
    if let Some(settings) = settings.as_mut() {
        settings.controls_shown = true;
    }
}

/// Select the balanced car and enter practice (or a timed run with
/// `RACER_DEMO=timed`).
fn demo_enter_drive(
    mut selected: ResMut<SelectedCar>,
    mut mode: ResMut<GameMode>,
    mut next: ResMut<NextState<AppState>>,
) {
    selected.car = Some(core::tuning::CarId::FairladyZ);
    mode.timed = std::env::var("RACER_DEMO").as_deref() == Ok("timed");
    next.set(AppState::Driving);
}

/// Scripted driving input: launch, ease into a slide, swing through S-curves
/// — all within the skidpad radius. Loops so long observation windows keep
/// seeing action.
pub fn scripted_input(elapsed: f64) -> DriverInput {
    let t = elapsed as f32 % 14.0;
    let mut input = DriverInput::default();
    // Launch straight.
    if (0.5..3.0).contains(&t) {
        input.throttle = 1.0;
    }
    // Ease into a right-hand slide: part throttle, moderate steer, short
    // handbrake kiss to unsettle the rear.
    if (3.0..3.35).contains(&t) {
        input.throttle = 0.8;
        input.steer = 0.5;
        input.handbrake = true;
    }
    if (3.35..6.0).contains(&t) {
        input.throttle = 0.9;
        input.steer = 0.5;
    }
    // Swing the other way.
    if (6.0..9.0).contains(&t) {
        input.throttle = 0.9;
        input.steer = -0.5;
    }
    // Circle back toward the pad center.
    if (9.0..12.5).contains(&t) {
        input.throttle = 0.85;
        input.steer = 0.6;
    }
    // Settle before the next cycle.
    if t >= 12.5 {
        input.brake = 0.5;
    }
    input
}
