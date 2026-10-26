//! Vehicle bridge: runs the core simulation inside avian's physics schedule,
//! applies resulting forces to the chassis rigid body, detects impacts, and
//! keeps render-side runtime state (VehicleRuntime) fresh for camera/UI/FX.
//!
//! Controller systems live in `PhysicsSystems::Prepare` so they run exactly
//! once per physics tick, automatically respecting pause/stepping.

use crate::game_state::PlayerCar;
use crate::game_state::*;
use crate::input::DriverInputRes;
use crate::world::build::WorldBuilt;
use avian3d::prelude::*;
use bevy::prelude::*;
use core::score::RUN_DURATION_SECONDS;
use core::sim::{BodyState, DriverInput, GroundQuery, RayHit, VehicleConfig, VehicleSim};
use core::tuning::{CarId, LiveTuning};

/// Pre-run countdown length for timed runs (seconds).
pub const COUNTDOWN_SECONDS: f32 = 3.0;

pub struct VehiclePlugin;

impl Plugin for VehiclePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VehicleSimRes>()
            .init_resource::<VehicleRuntime>()
            .init_resource::<ResetRequest>()
            .add_systems(
                // FixedUpdate runs before avian's PhysicsSchedule (which lives
                // in FixedPostUpdate) within the same fixed tick: the tire
                // forces written here are consumed by the solver this tick.
                FixedUpdate,
                (vehicle_control, gameplay_tick.after(vehicle_control)),
            )
            .add_systems(Update, (handle_reset, physics_clock_control));
    }
}

/// The authoritative controller instance plus its selected car.
#[derive(Resource)]
pub struct VehicleSimRes {
    pub sim: VehicleSim,
    pub car: CarId,
}

impl Default for VehicleSimRes {
    fn default() -> Self {
        VehicleSimRes {
            sim: VehicleSim::new(VehicleConfig::new(
                CarId::FairladyZ.preset(),
                LiveTuning::default(),
            )),
            car: CarId::FairladyZ,
        }
    }
}

impl VehicleSimRes {
    /// Recreate the sim for a (new) car with fresh tuning.
    pub fn select_car(&mut self, car: CarId, tuning: LiveTuning) {
        self.car = car;
        self.sim = VehicleSim::new(VehicleConfig::new(car.preset(), tuning));
    }
}

/// Reset/retry requests with a monotonic sequence number.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct ResetRequest {
    pub seq: u64,
    /// Full retry: restarts session score/timer (timed runs), not just pose.
    pub full: bool,
}

/// Ground adapter over avian's spatial query, excluding the car and its
/// collider child so the chassis box never blocks wheel rays.
struct AvianGround<'w, 's> {
    query: &'w SpatialQuery<'w, 's>,
    car: Entity,
    collider: Entity,
}

impl GroundQuery for AvianGround<'_, '_> {
    fn cast_ray(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<RayHit> {
        let filter =
            SpatialQueryFilter::from_excluded_entities([self.car, self.collider]);
        let dir = bevy::math::Dir3::new(direction).ok()?;
        let hit = self
            .query
            .cast_ray(origin, dir, max_distance, true, &filter)?;
        Some(RayHit {
            point: origin + direction.normalize_or_zero() * hit.distance as f32,
            normal: hit.normal,
            distance: hit.distance as f32,
        })
    }
}

/// True while a timed run is still counting down (inputs locked).
pub fn in_countdown(score: &ScoreRuntime) -> bool {
    score.run_active && score.run_time_left > RUN_DURATION_SECONDS
}

/// The authoritative fixed-tick control step.
#[allow(clippy::type_complexity)]
fn vehicle_control(
    mut sim_res: ResMut<VehicleSimRes>,
    mut car: Query<
        (
            Entity,
            &Position,
            &Rotation,
            &LinearVelocity,
            &AngularVelocity,
            Option<&ComputedMass>,
            &mut ConstantForce,
            &mut ConstantTorque,
        ),
        With<PlayerCar>,
    >,
    colliders: Query<Entity, With<crate::assets::CarCollider>>,
    input: Res<DriverInputRes>,
    spatial: SpatialQuery,
    mut runtime: ResMut<VehicleRuntime>,
    score: Res<ScoreRuntime>,
    mut step: ResMut<StepControl>,
    mut timing: ResMut<PhysicsTiming>,
) {
    let Ok((entity, pos, rot, linvel, angvel, mass, mut force, mut torque)) = car.single_mut()
    else {
        return;
    };
    // ComputedMass appears after avian's first prepare pass; bail until then.
    let Some(mass) = mass else { return };
    let mass = mass.value() as f32;
    let Ok(collider_entity) = colliders.single() else { return };

    let mut input = input.input;
    if in_countdown(&score) {
        input = DriverInput::default();
    }

    let body = BodyState {
        position: pos.0,
        rotation: rot.0,
        velocity: linvel.0,
        angular_velocity: angvel.0,
        mass,
    };
    let ground = AvianGround {
        query: &spatial,
        car: entity,
        collider: collider_entity,
    };
    let dt = core::FIXED_DT;
    let started = std::time::Instant::now();
    let (forces, telemetry) = sim_res.sim.step(&body, &input, &ground, dt);
    let controller_ms = started.elapsed().as_secs_f32() * 1000.0;

    force.0 = forces.force;
    torque.0 = forces.torque;

    timing.controller_ms = controller_ms;
    timing.ticks_pending = timing.ticks_pending.saturating_add(1);
    *runtime = VehicleRuntime {
        telemetry: telemetry,
        wheels: sim_res.sim.wheels,
        rpm: sim_res.sim.rpm(),
        gear: sim_res.sim.gear(),
        steer_visual: sim_res.sim.steer_angle(),
        speed_kmh: telemetry.speed * 3.6,
        drift_angle_deg: core::deg(telemetry.drift_angle),
        yaw_rate: telemetry.yaw_rate,
        forward: body.rotation * Vec3::NEG_Z,
        velocity: body.velocity,
        suspension: [
            sim_res.sim.wheels[0].suspension_length,
            sim_res.sim.wheels[1].suspension_length,
            sim_res.sim.wheels[2].suspension_length,
            sim_res.sim.wheels[3].suspension_length,
        ],
        car_entity: Some(entity),
    };

    if step.pending_ticks > 0 {
        step.consume_step();
    }
}

/// Gameplay advancement inside the physics tick: zone proximity, scoring,
/// impact/spin invalidation, and the timed-run clock.
#[allow(clippy::type_complexity)]
fn gameplay_tick(
    sim: Res<VehicleSimRes>,
    runtime: Res<VehicleRuntime>,
    mut score: ResMut<ScoreRuntime>,
    world: Option<Res<WorldBuilt>>,
    car: Query<(Entity, &Position, Option<&ComputedMass>), With<PlayerCar>>,
    colliders: Query<Entity, With<crate::assets::CarCollider>>,
    collisions: Collisions,
    mut impact: ResMut<ImpactSignal>,
    mut mode_next: ResMut<NextState<AppState>>,
    mut settings: ResMut<GameSettings>,
    step: Res<StepControl>,
    time: Res<Time>,
) {
    let Ok((_entity, pos, mass)) = car.single() else {
        return;
    };
    let mass = mass.map(|m| m.value() as f32).unwrap_or(0.0);
    let Ok(collider_entity) = colliders.single() else { return };
    let Some(world) = world else { return };
    let geo = &world.geo;

    // ---- Impact detection (walls/rails only; curbs are drivable ramps) ----
    let mut max_impulse = 0.0_f32;
    for pair in collisions.collisions_with(collider_entity) {
        max_impulse = max_impulse.max(pair.total_normal_impulse_magnitude() as f32);
    }
    let impact_threshold = mass * 1.5; // ≈ a 1.5 m/s velocity change
    let had_significant_impact = max_impulse > impact_threshold;
    if had_significant_impact {
        impact.strength = (max_impulse / impact_threshold).min(3.0);
        impact.seq += 1;
    }

    // ---- Zone proximity factor (capped 1.25, only used while qualifying) ----
    let mut zone_factor = 1.0_f32;
    for (c, r) in &geo.zones {
        let d = pos.0.distance(*c);
        if d < *r {
            zone_factor = zone_factor.max(1.0 + 0.25 * (1.0 - d / r));
        }
    }
    score.zone_factor = zone_factor.min(core::score::PROXIMITY_MAX_FACTOR);

    // ---- Scoring ----
    if in_countdown(&score) {
        // Pre-run countdown: physics warms up, scoring waits.
        return;
    }
    let tel = runtime.telemetry;
    let angle_deg = runtime.drift_angle_deg;
    let spin = score.score.check_spin(angle_deg, runtime.speed_kmh);
    let zone = score.zone_factor;
    score
        .score
        .tick(runtime.speed_kmh, angle_deg, tel.grounded_wheels > 0, zone);
    if had_significant_impact || spin {
        score.score.invalidate_combo();
    }

    // ---- Timed run clock (uses physics time: freezes with pause) ----
    if score.run_active {
        let dt = time.delta_secs_f64() as f32;
        if score.run_time_left > RUN_DURATION_SECONDS {
            // Countdown phase.
            score.run_time_left -= dt;
        } else {
            score.run_time_left -= dt;
            if score.run_time_left <= 0.0 {
                score.run_time_left = 0.0;
                score.run_active = false;
                // Bank surviving pending, then freeze the result.
                score.score.force_bank();
                score.score.note_best();
                let practice_reason = if score.is_practice {
                    Some(practice_reason(&step, &settings.tuning_modified, sim.car))
                } else {
                    None
                };
                let mut new_best = false;
                if !score.is_practice {
                    let key = core::persistence::Settings::car_key(sim.car);
                    let existing = settings
                        .settings
                        .best_for(sim.car)
                        .map(|b| b.score)
                        .unwrap_or(0.0);
                    if score.score.banked > existing {
                        new_best = true;
                        settings
                            .settings
                            .record_best(sim.car, score.score.banked, chrono_stamp());
                        settings.dirty = true;
                        let _ = key;
                    }
                }
                score.run_result = Some(RunResult {
                    banked: score.score.banked,
                    pending: score.score.pending,
                    is_practice: score.is_practice,
                    new_best,
                    practice_reason,
                });
                mode_next.set(AppState::Results);
            }
        }
    }
}

/// Nearest checkpoint to a world position.
fn nearest_checkpoint(poses: &[(Vec3, f32)], pos: Vec3) -> (Vec3, f32) {
    poses
        .iter()
        .copied()
        .min_by(|a, b| {
            a.0.distance_squared(pos)
                .partial_cmp(&b.0.distance_squared(pos))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or((Vec3::new(0.0, 1.0, 0.0), 0.0))
}

fn practice_reason(
    step: &StepControl,
    tuning: &crate::persistence::TuningModified,
    car: CarId,
) -> String {
    let mut reasons = Vec::new();
    if step.stepped_this_session {
        reasons.push("debug stepping was used");
    }
    if tuning.modified(car) {
        reasons.push("handling tuning was altered");
    }
    if reasons.is_empty() {
        "practice session".to_string()
    } else {
        reasons.join(", ")
    }
}

fn chrono_stamp() -> String {
    // Local, dependency-free timestamp for best-run display.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = now / 86400;
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Physics clock control: pause with the game, and advance exactly one fixed
/// tick per debug-step request while paused.
fn physics_clock_control(
    step: Res<StepControl>,
    mode: Res<State<AppState>>,
    mut time: ResMut<Time<Physics>>,
) {
    match mode.get() {
        AppState::Driving => {
            if time.is_paused() {
                time.unpause();
            }
        }
        AppState::Paused => {
            if !time.is_paused() {
                time.pause();
            }
            if step.pending_ticks > 0 {
                time.advance_by(std::time::Duration::from_secs_f64(core::FIXED_DT as f64));
            }
        }
        _ => {}
    }
}

/// Reset handling (R key): clear velocities/transients, return to the nearest
/// checkpoint. Full retries additionally restart the session.
#[allow(clippy::type_complexity)]
fn handle_reset(
    requests: Res<ControlRequests>,
    resets: Res<ResetRequest>,
    mut seen: Local<u64>,
    mut seen_full: Local<u64>,
    mut car: Query<
        (
            Entity,
            &mut Position,
            &mut Rotation,
            &mut LinearVelocity,
            &mut AngularVelocity,
        ),
        With<PlayerCar>,
    >,
    world: Option<Res<WorldBuilt>>,
    mut sim: ResMut<VehicleSimRes>,
    mut score: ResMut<ScoreRuntime>,
    mut fx_clear: ResMut<crate::fx::FxClearRequest>,
    mut camera: ResMut<CameraState>,
    mode: Res<State<AppState>>,
    mut mode_next: ResMut<NextState<AppState>>,
    game_mode: Res<GameMode>,
) {
    let requested = requests.reset != *seen || resets.seq != *seen_full;
    if !requested {
        return;
    }
    let full = resets.seq != *seen_full;
    *seen = requests.reset;
    *seen_full = resets.seq;

    let Ok((_entity, mut pos, mut rot, mut vel, mut angvel)) = car.single_mut() else {
        return;
    };
    let Some(world) = world else { return };
    let (cp_pos, cp_yaw) = nearest_checkpoint(&world.geo.checkpoints, pos.0);
    pos.0 = cp_pos;
    rot.0 = Quat::from_rotation_y(cp_yaw);
    vel.0 = Vec3::ZERO;
    angvel.0 = Vec3::ZERO;
    sim.sim.reset_transient();
    score.score.invalidate_combo();
    fx_clear.0 += 1;
    camera.snap_requested = true;

    if full {
        // Session restart: same mode, fresh score and timer.
        score.score.reset_session();
        score.run_result = None;
        score.is_practice = score.is_practice && !score.run_active;
        score.run_time_left = if game_mode.timed {
            RUN_DURATION_SECONDS + COUNTDOWN_SECONDS
        } else {
            0.0
        };
        score.run_active = game_mode.timed;
    }
    if *mode.get() == AppState::Paused {
        mode_next.set(AppState::Driving);
    }
}
