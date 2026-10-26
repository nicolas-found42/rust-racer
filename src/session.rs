//! Game session wiring: spawn/despawn cars on state transitions, live tuning
//! application, wheel visual updates, and debug gizmos.

use crate::assets::{self, CarHandles, WheelMeshNode};
use crate::game_state::*;
use crate::vehicle::{VehicleSimRes, COUNTDOWN_SECONDS};
use avian3d::prelude::*;
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
                water_reset.run_if(in_state(AppState::Driving)),
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
    pub spin_accum: [f32; 4],
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
    let spawn = Vec3::new(0.0, 1.0, 18.0);
    let root = assets::spawn_car(&mut commands, &handles, &gltfs, selected, spawn, 0.0);
    debug!("Spawned {:?} at {:?}", selected, spawn);

    // Wheel animation geometry; the model's own wheel mesh nodes get tagged
    // by `process_car_scenes` once the scene spawns.
    let preset = selected.preset();
    let visuals = WheelVisuals {
        base_pos: preset.wheel_positions.map(Vec3::from),
        suspension_rest: preset.suspension_rest_length,
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

/// Animate the model's own wheel mesh nodes: steering, suspension travel and
/// axle spin, composed onto each node's at-rest base transform.
fn update_wheel_visuals(
    runtime: Res<VehicleRuntime>,
    mut wheels: ResMut<WheelVisuals>,
    mut nodes: Query<(&WheelMeshNode, &mut Transform)>,
    time: Res<Time>,
) {
    let dt = time.delta_secs();
    for (node, mut t) in nodes.iter_mut() {
        let i = node.index;
        let steer = if i < 2 { runtime.steer_visual } else { 0.0 };
        let hang = (wheels.suspension_rest - runtime.suspension[i]).clamp(-0.12, 0.25);
        let spin = {
            let omega = runtime.wheels[i].angular_velocity;
            wheels.spin_accum[i] = (wheels.spin_accum[i] + omega * dt) % std::f32::consts::TAU;
            // Mirrored rim nodes (180° yaw base) have local X pointing the
            // other way; flip so all wheels roll in the same world direction.
            let axle_sign = (node.base.rotation * Vec3::X).x.signum();
            wheels.spin_accum[i] * axle_sign
        };
        // Compressed suspension (length < rest) pulls the wheel up toward
        // the body; hang is positive then.
        t.translation = node.base.translation + Vec3::Y * hang;
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
