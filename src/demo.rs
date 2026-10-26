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

/// True when the game should stay silent and keep out of the way of whoever
/// is using the machine: the demo harness or an explicit `RACER_MUTE=1`.
pub fn quiet() -> bool {
    enabled() || std::env::var("RACER_DEMO").as_deref() == Ok("timed")
        || std::env::var("RACER_MUTE").as_deref() == Ok("1")
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
    // `RACER_CAM=chase|orbit|hood` pins one camera for focused screenshots.
    let pinned = match std::env::var("RACER_CAM").as_deref() {
        Ok("chase") => Some(CameraMode::Chase),
        Ok("orbit") => Some(CameraMode::Orbit),
        Ok("hood") => Some(CameraMode::Hood),
        _ => None,
    };
    if let Some(m) = pinned {
        if camera.mode != m {
            camera.mode = m;
            camera.snap_requested = true;
        }
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
            .add_systems(Update, (demo_enter_drive, demo_telemetry, demo_auto_reset, demo_camera_cycle, demo_clearance_probe, demo_wheel_dir));
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

/// Keep the demo on the skidpad: if the car crawls for several seconds while
/// the script demands throttle, or drives off the pad, put it back at the pad
/// spawn. (The game's own reset goes to the nearest checkpoint, which from the
/// pad edge is out on the connector roads.)
#[allow(clippy::type_complexity)]
fn demo_auto_reset(
    mut stuck_since: Local<Option<f64>>,
    time: Res<Time>,
    runtime: Res<VehicleRuntime>,
    mode: Res<State<AppState>>,
    mut car_q: Query<
        (&mut Position, &mut Rotation, &mut LinearVelocity, &mut AngularVelocity),
        With<PlayerCar>,
    >,
    mut camera: ResMut<CameraState>,
) {
    if *mode != AppState::Driving {
        *stuck_since = None;
        return;
    }
    let Ok((mut pos, mut rot, mut vel, mut ang)) = car_q.single_mut() else {
        return;
    };
    let wants_motion = runtime.telemetry.throttle > 0.5;
    let stuck = wants_motion && runtime.telemetry.speed < 0.3;
    let now = time.elapsed_secs_f64();
    let off_pad = pos.0.x * pos.0.x + pos.0.z * pos.0.z > 40.0 * 40.0 || pos.0.y < -1.0;
    let beached = match (*stuck_since, stuck) {
        (None, true) => {
            *stuck_since = Some(now);
            false
        }
        (Some(_), false) => {
            *stuck_since = None;
            false
        }
        (Some(s), true) => now - s > 3.0,
        (None, false) => false,
    };
    if off_pad || beached {
        pos.0 = Vec3::new(0.0, 0.6, 18.0);
        rot.0 = Quat::IDENTITY;
        vel.0 = Vec3::ZERO;
        ang.0 = Vec3::ZERO;
        camera.snap_requested = true;
        *stuck_since = None;
        info!("demo: {}, back to pad spawn", if off_pad { "left the pad" } else { "car beached" });
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
    // `RACER_HOLD=1` keeps the car parked, for rest-pose and ride-height shots.
    if std::env::var("RACER_HOLD").as_deref() == Ok("1") {
        return DriverInput::default();
    }
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

/// Every 2 s while all four wheels are grounded: lowest world-space point of
/// the car body and of the tires against the contact-patch height, and each
/// wheel's surface speed against ground speed. Exposes visual ground clipping
/// and wheel-spin mismatches in the log without eyeballing screenshots.
#[allow(clippy::type_complexity)]
fn demo_clearance_probe(
    mut next: Local<f64>,
    mut worst: Local<(f32, f64)>,
    time: Res<Time>,
    runtime: Res<VehicleRuntime>,
    meshes: Query<(Entity, &bevy::camera::primitives::Aabb, &GlobalTransform)>,
    parents: Query<&ChildOf>,
    wheel_nodes: Query<(), With<crate::assets::WheelMeshNode>>,
    player: Query<(), With<PlayerCar>>,
    vel: Query<&LinearVelocity, With<PlayerCar>>,
    mass_q: Query<&ComputedMass, With<PlayerCar>>,
    visuals: Option<Res<crate::session::WheelVisuals>>,
    vis_q: Query<(&GlobalTransform, &Transform), With<CarVisual>>,
    body_q: Query<(&Position, &GlobalTransform), With<PlayerCar>>,
) {
    // Settled = at rest at the static ride height (not a bounce turning point).
    let avg_susp = runtime.suspension.iter().sum::<f32>() / 4.0;
    let at_ride_height = visuals.map(|v| (avg_susp - v.rest_len).abs() < 0.012).unwrap_or(false);
    let settled = at_ride_height && vel.single().map(|v| v.0.length() < 0.05).unwrap_or(false);
    let now = time.elapsed_secs_f64();
    if now < *next || runtime.telemetry.grounded_wheels < 3 {
        return;
    }
    *next = now + 0.1;
    let still = runtime.wheels.iter().all(|w| w.angular_velocity.abs() < 0.5);
    let (mut body_min, mut tire_min) = (f32::MAX, f32::MAX);
    for (e, aabb, gt) in meshes.iter() {
        let (mut is_wheel, mut is_car, mut cur) = (false, false, e);
        loop {
            is_wheel |= wheel_nodes.get(cur).is_ok();
            if player.get(cur).is_ok() {
                is_car = true;
                break;
            }
            match parents.get(cur) {
                Ok(p) => cur = p.parent(),
                Err(_) => break,
            }
        }
        if !is_car {
            continue;
        }
        let (c, h) = (Vec3::from(aabb.center), Vec3::from(aabb.half_extents));
        let mut lo = f32::MAX;
        for i in 0..8 {
            let corner = c + h * Vec3::new(
                if i & 1 == 0 { -1.0 } else { 1.0 },
                if i & 2 == 0 { -1.0 } else { 1.0 },
                if i & 4 == 0 { -1.0 } else { 1.0 },
            );
            lo = lo.min(gt.transform_point(corner).y);
        }
        if is_wheel {
            // A spinning tire's rotated bounding box overstates how low it is.
            if still {
                tire_min = tire_min.min(lo);
            }
        } else {
            body_min = body_min.min(lo);
        }
    }
    let ground = runtime.wheels.iter().map(|w| w.contact_point.y).sum::<f32>() / 4.0;
    // Body clearance over the ground under it, worst case per 3 s window.
    if worst.1 == 0.0 || worst.0 == 0.0 {
        *worst = (f32::MAX, now);
    }
    worst.0 = worst.0.min(body_min - ground);
    if now - worst.1 > 3.0 {
        info!("clearance worst_body={:.3} (negative = body below ground)", worst.0);
        *worst = (f32::MAX, now);
    }
    if !settled || runtime.telemetry.speed > 0.3 {
        return;
    }
    let v = runtime.telemetry.speed.max(0.01);
    let ratio: Vec<String> = runtime
        .wheels
        .iter()
        .map(|w| format!("{:.2}", w.angular_velocity * 0.32 / v))
        .collect();
    let vis = vis_q.iter().map(|(g, t)| (g.translation().y, t.translation.y)).collect::<Vec<_>>();
    let body = body_q.single().map(|(p, g)| (p.0.y, g.translation().y)).ok();
    info!("probe-geom visual(world_y,local_y)={:?} body(position_y,gt_y)={:?}", vis, body);
    info!(
        "probe mass={:?} loads={:?} ground={:.3} body_min={:.3} tire_min={:.3} susp={:?} spin/rolling={:?} speed={:.1}",
        mass_q.single().map(|m| m.value()).ok(), runtime.wheels.map(|w| w.load.round()), ground, body_min, tire_min, runtime.suspension, ratio, v
    );
}

/// Once a second while driving: the sign of each front wheel node's world
/// angular velocity about the car's right axis (negative = rolling forward) and
/// steering visual vs yaw rate, to catch reversed wheel spin or steer.
fn demo_wheel_dir(
    mut next: Local<f64>,
    mut prev: Local<[Quat; 4]>,
    time: Res<Time>,
    runtime: Res<VehicleRuntime>,
    nodes: Query<(&crate::assets::WheelMeshNode, &GlobalTransform)>,
    car: Query<&Rotation, With<PlayerCar>>,
) {
    let dt = time.delta_secs();
    let Ok(rot) = car.single() else { return };
    let right = rot.0 * Vec3::X;
    let mut spin = [f32::NAN; 4];
    for (n, gt) in nodes.iter() {
        let q = gt.rotation();
        let dq = q * prev[n.index].inverse();
        let (axis, angle) = dq.normalize().to_axis_angle();
        let angle = if angle > std::f32::consts::PI { angle - std::f32::consts::TAU } else { angle };
        if dt > 0.0 {
            spin[n.index] = (axis * angle / dt).dot(right);
        }
        prev[n.index] = q;
    }
    let now = time.elapsed_secs_f64();
    if now < *next || runtime.telemetry.speed < 3.0 {
        return;
    }
    *next = now + 1.0;
    info!(
        "wheeldir speed={:.1} node_spin_about_right={:?} sim_omega={:?} steer_visual={:.2} yaw_rate={:.2}",
        runtime.telemetry.speed,
        spin.map(|v| (v * 10.0).round() / 10.0),
        runtime.wheels.map(|w| (w.angular_velocity * 10.0).round() / 10.0),
        runtime.steer_visual,
        runtime.yaw_rate
    );
}
