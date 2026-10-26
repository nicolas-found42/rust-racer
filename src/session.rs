//! Game session wiring: spawn/despawn cars on state transitions, live tuning
//! application, wheel visual updates, and debug gizmos.

use crate::assets::{self, CarHandles, WheelMeshNode};
use crate::game_state::*;
use crate::vehicle::{VehicleSimRes, COUNTDOWN_SECONDS};
use avian3d::prelude::*;
use bevy::camera::primitives::Aabb;
use bevy::gizmos::prelude::Gizmos;
use bevy::prelude::*;
use core::score::RUN_DURATION_SECONDS;

pub struct SessionPlugin;

impl Plugin for SessionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WheelVisuals>()
            .add_systems(OnEnter(AppState::Driving), start_session)
            .add_systems(OnEnter(AppState::Garage), enter_garage_preview)
            .add_systems(OnExit(AppState::Garage), exit_garage_preview)
            .add_systems(
                Update,
                (water_reset, flip_reset).run_if(in_state(AppState::Driving)),
            )
            .add_systems(
                Update,
                (
                    attach_pending_scenes_system,
                    process_car_scenes_system,
                    apply_live_tuning,
                    update_wheel_visuals,
                    draw_debug_gizmos,
                )
                    .run_if(in_state(AppState::Driving).or(in_state(AppState::Paused))),
            )
            .add_systems(
                Update,
                garage_preview_respawn.run_if(in_state(AppState::Garage)),
            );
    }
}

/// Garage: spawn the selected car on the skidpad so the orbit camera has a
/// real model to inspect; cycling selection swaps it.
fn enter_garage_preview(
    mut commands: Commands,
    handles: Res<CarHandles>,
    gltfs: Res<Assets<bevy::gltf::Gltf>>,
    settings: Option<Res<GameSettings>>,
    selected: Option<Res<SelectedCar>>,
) {
    let car = settings
        .as_deref()
        .map(|s| s.settings.selected_car)
        .or_else(|| selected.as_deref().and_then(|s| s.car))
        .unwrap_or(core::tuning::CarId::FairladyZ);
    assets::spawn_car(&mut commands, &handles, &gltfs, car, Vec3::new(0.0, 1.0, 10.0), 0.6);
}

fn exit_garage_preview(mut commands: Commands, car: Query<Entity, With<PlayerCar>>) {
    for e in car.iter() {
        commands.entity(e).despawn();
    }
}

/// Swap the garage preview model when the selection changes.
fn garage_preview_respawn(
    mut commands: Commands,
    handles: Res<CarHandles>,
    gltfs: Res<Assets<bevy::gltf::Gltf>>,
    selected: Option<Res<SelectedCar>>,
    mut prev: Local<Option<core::tuning::CarId>>,
    car: Query<Entity, With<PlayerCar>>,
) {
    let Some(sel) = selected.as_deref() else {
        return;
    };
    let Some(want) = sel.car else {
        return;
    };
    if *prev == Some(want) {
        return;
    }
    *prev = Some(want);
    for e in car.iter() {
        commands.entity(e).despawn();
    }
    assets::spawn_car(&mut commands, &handles, &gltfs, want, Vec3::new(0.0, 1.0, 10.0), 0.6);
}

/// Recover a car that left the drivable world (into the ocean sink): request
/// a checkpoint reset instead of letting it beach below the water line.
fn water_reset(
    car: Query<&Position, With<PlayerCar>>,
    mode: Res<State<AppState>>,
    mut reset: ResMut<crate::vehicle::ResetRequest>,
    mut announced: Local<bool>,
) {
    if *mode != AppState::Driving {
        *announced = false;
        return;
    }
    let Ok(pos) = car.single() else {
        return;
    };
    if pos.0.y < -3.5 && !*announced {
        reset.seq += 1;
        reset.full = false;
        *announced = true;
        info!("car below water line, auto-reset to checkpoint");
    }
    if pos.0.y > -1.0 {
        *announced = false;
    }
}

/// Recover a car lying on its roof or side: once it has been tipped past ~70°
/// and nearly stopped for 1.5 s, request a checkpoint reset (same as R).
fn flip_reset(
    car: Query<&Rotation, With<PlayerCar>>,
    runtime: Res<VehicleRuntime>,
    time: Res<Time>,
    mut reset: ResMut<crate::vehicle::ResetRequest>,
    mut since: Local<Option<f64>>,
) {
    let Ok(rot) = car.single() else {
        return;
    };
    let tipped = (rot.0 * Vec3::Y).y < 0.35 && runtime.telemetry.speed < 4.0;
    let now = time.elapsed_secs_f64();
    match (*since, tipped) {
        (None, true) => *since = Some(now),
        (Some(_), false) => *since = None,
        (Some(t0), true) if now - t0 > 1.5 => {
            reset.seq += 1;
            reset.full = false;
            *since = None;
            info!("car flipped, auto-reset to checkpoint");
        }
        _ => {}
    }
}

fn attach_pending_scenes_system(
    commands: Commands,
    handles: Res<CarHandles>,
    gltfs: Res<Assets<bevy::gltf::Gltf>>,
    pending: Query<(Entity, &assets::PendingVisualChild)>,
) {
    assets::attach_pending_scenes(commands, handles, gltfs, pending);
}

#[allow(clippy::too_many_arguments)]
fn process_car_scenes_system(
    commands: Commands,
    handles: Res<CarHandles>,
    gltfs: Res<Assets<bevy::gltf::Gltf>>,
    pending: Query<(Entity, &assets::CarModelInfo), With<assets::PendingCarScene>>,
    player: Query<(), With<PlayerCar>>,
    scene_nodes: Query<(Entity, Option<&Name>, &ChildOf, &Transform), Added<ChildOf>>,
    materials: ResMut<Assets<StandardMaterial>>,
) {
    assets::process_car_scenes(
        commands,
        handles,
        gltfs,
        pending,
        player,
        scene_nodes,
        materials,
    );
}

/// Base geometry for animating the player car's tagged wheel mesh nodes.
#[derive(Resource, Default)]
pub struct WheelVisuals {
    /// Wheel base local positions (from the car preset).
    pub base_pos: [Vec3; 4],
    pub suspension_rest: f32,
    /// Suspension length the visual rest pose assumes: the static-load
    /// equilibrium, so the body sits at its true ride height.
    pub rest_len: f32,
    pub wheel_radius: f32,
    pub spin_accum: [f32; 4],
    /// Lowest tire point in the visual root's space at the model's rest pose,
    /// measured once the scene's meshes have bounds. `None` until then.
    pub tire_bottom: Option<f32>,
}

/// Enter Driving: (re)spawn the selected car and reset session state.
fn start_session(
    mut commands: Commands,
    handles: Res<CarHandles>,
    gltfs: Res<Assets<bevy::gltf::Gltf>>,
    mut sim: ResMut<VehicleSimRes>,
    settings: Res<GameSettings>,
    mut score: ResMut<ScoreRuntime>,
    mode: Res<GameMode>,
    wheels_query: Query<Entity, With<PlayerCar>>,
    mut camera: ResMut<CameraState>,
    mut runtime: ResMut<VehicleRuntime>,
) {
    // Despawn any previous car (and its visuals).
    for e in wheels_query.iter() {
        commands.entity(e).despawn();
    }

    let selected = settings.settings.selected_car;
    let tuning = settings.tuning.clone();
    sim.select_car(selected, tuning);

    // Session state.
    *score = ScoreRuntime {
        score: core::score::ScoreState::default(),
        zone_factor: 1.0,
        run_time_left: if mode.timed {
            RUN_DURATION_SECONDS + COUNTDOWN_SECONDS
        } else {
            0.0
        },
        run_active: mode.timed,
        run_result: None,
        is_practice: false,
        new_best: false,
    };

    // Spawn at the skidpad checkpoint facing the pad center.
    // Spawn at ride height so the car doesn't drop and bounce on arrival.
    let preset = selected.preset();
    let rest_len = preset.suspension_rest_length
        - preset.mass_kg * 9.81 * 0.25 / preset.suspension_stiffness;
    let spawn = Vec3::new(
        0.0,
        rest_len + preset.wheel_radius - preset.wheel_positions[0][1] + 0.02,
        18.0,
    );
    let root = assets::spawn_car(&mut commands, &handles, &gltfs, selected, spawn, 0.0);
    debug!("Spawned {:?} at {:?}", selected, spawn);

    // Wheel animation geometry; the model's own wheel mesh nodes get tagged
    // by `process_car_scenes` once the scene spawns.
    let visuals = WheelVisuals {
        base_pos: preset.wheel_positions.map(Vec3::from),
        suspension_rest: preset.suspension_rest_length,
        // Static equilibrium of the spring under a quarter of the weight.
        rest_len,
        wheel_radius: preset.wheel_radius,
        ..default()
    };
    commands.insert_resource(visuals);
    runtime.car_entity = Some(root);
    camera.snap_requested = true;
    commands.insert_resource(SelectedCar {
        car: Some(selected),
    });
}

/// The tuning panel edits `GameSettings.tuning`; mirror it into the sim and
/// track legitimacy. Reset to defaults marks the flags cleared.
fn apply_live_tuning(
    mut settings: ResMut<GameSettings>,
    mut sim: ResMut<VehicleSimRes>,
    mut score: ResMut<ScoreRuntime>,
) {
    let mut t = settings.tuning.clone();
    t.clamp_all();
    sim.sim.config.tuning = t.clone();
    if t != core::tuning::LiveTuning::default() {
        settings.tuning_modified.set(sim.car, true);
        score.is_practice = true;
    }
}

/// Lowest world-space-independent point of every mesh under the tagged wheel
/// nodes, expressed in the visual root's space.
#[allow(clippy::type_complexity)]
fn measure_tire_bottom(
    root: &GlobalTransform,
    nodes: &Query<(Entity, &WheelMeshNode)>,
    children: &Query<&Children>,
    meshes: &Query<(&Aabb, &GlobalTransform)>,
) -> Option<f32> {
    let to_root = root.affine().inverse();
    let mut lowest = f32::MAX;
    let mut found = 0;
    for (entity, _) in nodes.iter() {
        for e in std::iter::once(entity).chain(children.iter_descendants(entity)) {
            let Ok((aabb, gt)) = meshes.get(e) else {
                continue;
            };
            let (c, h) = (Vec3::from(aabb.center), Vec3::from(aabb.half_extents));
            let m = to_root * gt.affine();
            for i in 0..8 {
                let corner = c + h
                    * Vec3::new(
                        if i & 1 == 0 { -1.0 } else { 1.0 },
                        if i & 2 == 0 { -1.0 } else { 1.0 },
                        if i & 4 == 0 { -1.0 } else { 1.0 },
                    );
                lowest = lowest.min(m.transform_point3(corner).y);
            }
            found += 1;
        }
    }
    (found > 0).then_some(lowest)
}

/// Animate the model's own wheel mesh nodes so they sit exactly where the
/// simulation says the wheels are: each wheel hangs from the body by its
/// suspension length (tires touch the ground the ray found), steers with the
/// front axle, and spins at the simulated wheel speed.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_wheel_visuals(
    runtime: Res<VehicleRuntime>,
    mut wheels: ResMut<WheelVisuals>,
    mut nodes: Query<(&WheelMeshNode, &mut Transform), Without<CarVisual>>,
    node_entities: Query<(Entity, &WheelMeshNode)>,
    children: Query<&Children>,
    meshes: Query<(&Aabb, &GlobalTransform)>,
    mut visuals: Query<(&mut Transform, &GlobalTransform, &ChildOf), With<CarVisual>>,
    player: Query<(), With<PlayerCar>>,
    time: Res<Time>,
) {
    // Calibrate once: put the model's tire bottoms at the ground for the
    // static ride height. Wheel nodes stay at their base pose until then.
    if wheels.tire_bottom.is_none() {
        for (mut vt, gt, parent) in visuals.iter_mut() {
            if player.get(parent.parent()).is_err() {
                continue;
            }
            if let Some(bottom) = measure_tire_bottom(gt, &node_entities, &children, &meshes) {
                let attach_y = wheels.base_pos[0].y;
                // Wheel attach points sit attach_y above the body origin, so the
                // ground is rest_len + radius - attach_y below it.
                vt.translation.y = -(wheels.rest_len + wheels.wheel_radius - attach_y) - bottom;
                wheels.tire_bottom = Some(bottom);
                debug!("tire calibration: bottom={bottom:.3} visual_y={:.3} attach_y={:.3} rest_len={:.3} r={:.3}", vt.translation.y, attach_y, wheels.rest_len, wheels.wheel_radius);
            }
        }
        if wheels.tire_bottom.is_none() {
            return;
        }
    }

    let dt = time.delta_secs();
    for (node, mut t) in nodes.iter_mut() {
        let i = node.index;
        let steer = if i < 2 { runtime.steer_visual } else { 0.0 };
        // Longer suspension drops the wheel below its rest pose.
        let drop = runtime.suspension[i] - wheels.rest_len;
        let omega = runtime.wheels[i].angular_velocity;
        // Cap the per-frame turn so very fast spin reads as fast forward
        // rotation rather than aliasing into slow or reversed rotation.
        let step = (omega * dt).clamp(-1.0, 1.0);
        wheels.spin_accum[i] = (wheels.spin_accum[i] + step) % std::f32::consts::TAU;
        // Rolling forward (-Z) turns the wheel about -X; mirrored rim nodes
        // (180° yaw base) have local X pointing the other way, so flip.
        let axle_sign = (node.base.rotation * Vec3::X).x.signum();
        let spin = -wheels.spin_accum[i] * axle_sign;
        t.translation = node.base.translation + Vec3::Y * (-drop);
        t.rotation = node.base.rotation
            * Quat::from_rotation_y(steer)
            * Quat::from_rotation_x(spin);
    }
}

fn draw_debug_gizmos(
    mut gizmos: Gizmos,
    debug: Res<DebugView>,
    runtime: Res<VehicleRuntime>,
    car: Query<(&Position, &Rotation, Option<&ComputedMass>), With<PlayerCar>>,
    wheels: Res<WheelVisuals>,
) {
    if !debug.enabled {
        return;
    }
    let Ok((pos, rot, mass)) = car.single() else {
        return;
    };
    let mass = mass.map(|m| m.value() as f32).unwrap_or(1480.0);
    for (i, w) in runtime.wheels.iter().enumerate() {
        let attach = pos.0 + rot.0 * wheels.base_pos[i];
        let end = attach + Vec3::Y * -w.suspension_length;
        let color = if w.grounded {
            Color::srgb(0.2, 0.9, 0.3)
        } else {
            Color::srgb(0.8, 0.3, 0.2)
        };
        gizmos.line(attach, end, color);
        if w.grounded && debug.show_contacts {
            gizmos.sphere(w.contact_point, 0.08, Color::srgb(0.2, 0.9, 1.0));
        }
        if w.grounded && debug.show_forces {
            let scale = 1.0 / mass.max(1.0);
            let right = (rot.0 * Vec3::X)
                .reject_from_normalized(w.contact_normal)
                .normalize_or_zero();
            let fwd = (rot.0 * Vec3::NEG_Z)
                .reject_from_normalized(w.contact_normal)
                .normalize_or_zero();
            let lat = w.lateral_force * scale;
            let long = w.longitudinal_force * scale;
            if lat > 0.01 {
                let sign = if w.slip_angle > 0.0 { -1.0 } else { 1.0 };
                gizmos.arrow(
                    w.contact_point,
                    w.contact_point + right * (lat * sign).clamp(-3.0, 3.0),
                    Color::srgb(1.0, 0.9, 0.2),
                );
            }
            if long > 0.01 {
                gizmos.arrow(
                    w.contact_point + Vec3::Y * 0.02,
                    w.contact_point + fwd * (long * 0.05).clamp(-3.0, 3.0),
                    Color::srgb(1.0, 0.3, 0.2),
                );
            }
        }
    }
}
