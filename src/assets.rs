//! Car asset pipeline: loads the three real GLB models, normalizes each scene
//! (meters, -Z forward, ground contact), applies material corrections, and
//! re-parents rim/tire nodes onto steerable/spinning wheel pivots.
//!
//! The GLBs are byte-identical copies of the recovered dealership models
//! (see assets/cars/manifest.json). All adjustments happen at the Bevy
//! material/transform level — original geometry and textures are untouched.

use crate::game_state::*;
use avian3d::prelude::*;
use bevy::asset::AssetEvent;
use bevy::gltf::Gltf;
use bevy::prelude::*;
use bevy::scene::SceneRoot;
use core::tuning::{CarId, ALL_CARS};
use std::collections::HashMap;

pub struct CarAssetsPlugin;

impl Plugin for CarAssetsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CarHandles>()
            .init_resource::<LoadingProgress>()
            .add_systems(PreStartup, start_loading)
            .add_systems(
                Update,
                (watch_car_load.run_if(in_state(AppState::Loading)),),
            );
    }
}

/// Loaded GLB handles per car.
#[derive(Resource, Default)]
pub struct CarHandles {
    pub models: HashMap<CarId, Handle<Gltf>>,
}

fn start_loading(
    server: Res<AssetServer>,
    mut handles: ResMut<CarHandles>,
    mut progress: ResMut<LoadingProgress>,
) {
    progress.total = ALL_CARS.len() as u32;
    progress.message = "Loading car models".into();
    for car in ALL_CARS {
        let handle: Handle<Gltf> = server.load(car.model_asset());
        handles.models.insert(car, handle);
    }
}

fn watch_car_load(
    handles: Res<CarHandles>,
    mut events: MessageReader<AssetEvent<Gltf>>,
    mut progress: ResMut<LoadingProgress>,
    mut next: ResMut<NextState<AppState>>,
    gltfs: Res<Assets<Gltf>>,
    mut done: Local<bool>,
) {
    if *done {
        return;
    }
    let mut loaded = 0;
    for car in ALL_CARS {
        let h = &handles.models[&car];
        if gltfs.get(h).is_some() {
            loaded += 1;
        }
    }
    // Dependency-complete events (textures/materials ready).
    let mut ready = 0;
    for event in events.read() {
        if let AssetEvent::LoadedWithDependencies { id } = event {
            if handles.models.values().any(|h| h.id() == *id) {
                ready += 1;
            }
        }
    }
    progress.loaded = loaded;
    progress.message = format!("Loading car models ({loaded}/{})", ALL_CARS.len());
    if loaded as usize == ALL_CARS.len() && ready as usize >= ALL_CARS.len() {
        *done = true;
        info!("All car models loaded; entering garage");
        next.set(AppState::Garage);
    }
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

/// Marker for the car's collision child entity (impacts are read from it).
#[derive(Component, Debug, Clone, Copy)]
pub struct CarCollider;

/// Spawned car context needed for post-load processing and per-wheel visuals.
#[derive(Component)]
pub struct CarModelInfo {
    pub car: CarId,
    /// Wheel node name markers per index (FL, FR, RL, RR), matched by
    /// substring.
    pub wheel_markers: &'static [(&'static str, usize)],
}

/// Per-wheel mesh node marker, added to a player car's top-level wheel nodes
/// (rim + tire siblings of one axle). `base` is the node's local transform at
/// rest; the session system composes steering/spin/suspension on top of it
/// each frame.
#[derive(Component, Debug, Clone, Copy)]
pub struct WheelMeshNode {
    pub index: usize,
    pub base: Transform,
}

/// Pending scene load: (car root entity, car id).
#[derive(Component)]
pub struct PendingCarScene {
    pub car: CarId,
}

/// Name conventions per model. Front axle is -Z for all three models.
/// Each entry maps a node-name substring to a wheel index (FL, FR, RL, RR);
/// duplicate entries allow N nodes per wheel (rim + tire pairs).
fn wheel_markers(car: CarId) -> &'static [(&'static str, usize)] {
    // (name substring, wheel index)
    match car {
        // RX-7: each axle = 2 rim nodes + 1 tire node, all named "@axl_N".
        CarId::Rx7 => &[
            ("@axl_0", 0),
            ("@axl_0", 0),
            ("@axl_0", 0),
            ("@axl_1", 1),
            ("@axl_1", 1),
            ("@axl_1", 1),
            ("@axl_2", 2),
            ("@axl_2", 2),
            ("@axl_2", 2),
            ("@axl_3", 3),
            ("@axl_3", 3),
            ("@axl_3", 3),
        ],
        // 350Z: each wheel = a rim + tire sibling pair sharing the axle
        // prefix; verified against node translations (front = -Z).
        CarId::FairladyZ => &[
            ("whl_8_", 0),
            ("whl_8_", 0),
            ("whl_10_", 1),
            ("whl_10_", 1),
            ("whl_12_", 2),
            ("whl_12_", 2),
            ("whl_14_", 3),
            ("whl_14_", 3),
        ],
        // R34: each axle = 1 rim + 1 tire node, both "@axl_N".
        CarId::Skyline => &[
            ("@axl_0", 0),
            ("@axl_0", 0),
            ("@axl_1", 1),
            ("@axl_1", 1),
            ("@axl_2", 2),
            ("@axl_2", 2),
            ("@axl_3", 3),
            ("@axl_3", 3),
        ],
    }
}

/// Spawn a car with physics body and visual scene. Returns the root entity.
/// The scene is processed (wheels/materials) once its children exist.
#[allow(clippy::too_many_arguments)]
pub fn spawn_car(
    commands: &mut Commands,
    handles: &CarHandles,
    gltfs: &Assets<Gltf>,
    car: CarId,
    pos: Vec3,
    yaw: f32,
) -> Entity {
    let preset = car.preset();
    // Physics: box collider as an offset child so the box clears the ground;
    // the center of mass stays pinned at the body origin (driving height).
    let hx = preset.half_extents[0];
    let hy = preset.half_extents[1];
    let hz = preset.half_extents[2];
    let volume = 8.0 * hx * hy * hz;
    let density = preset.mass_kg / volume;
    let lift = preset.collider_offset_y();

    let root = commands
        .spawn((
            RigidBody::Dynamic,
            // Pin the COM at the body origin; the child collider would
            // otherwise pull it upward.
            CenterOfMass(Vec3::ZERO),
            Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(yaw)),
            LinearVelocity::ZERO,
            AngularVelocity::ZERO,
            ConstantForce(Vec3::ZERO),
            ConstantTorque(Vec3::ZERO),
            // The player car must respond to input immediately, even after
            // sitting still with brakes held.
            SleepingDisabled,
            PlayerCar,
            CarModelInfo {
                car,
                wheel_markers: wheel_markers(car),
            },
            PendingCarScene { car },
        ))
        .id();

    let collider = commands
        .spawn((
            CarCollider,
            Collider::cuboid(hx * 2.0, hy * 2.0, hz * 2.0),
            ColliderDensity(density),
            Transform::from_translation(Vec3::new(0.0, lift, 0.0)),
        ))
        .id();
    commands.entity(root).add_child(collider);

    // Visual child root: the model's origin is at ground level; the physics
    // origin is at the center of mass. Offset the visual down by the resting
    // center-of-mass height.
    let com_rest =
        preset.wheel_positions[0][1] + preset.suspension_rest_length * 0.8 + preset.wheel_radius;
    let visual = commands
        .spawn((
            CarVisual,
            Transform::from_translation(Vec3::new(0.0, -com_rest, 0.0)),
            Visibility::default(),
        ))
        .id();

    // Attach the scene now if already loaded; otherwise mark pending and let
    // `attach_pending_scenes` complete it.
    if let Some(handle) = handles.models.get(&car) {
        if let Some(gltf) = gltfs.get(handle) {
            commands
                .entity(visual)
                .insert(SceneRoot(gltf.scenes[0].clone()));
        } else {
            commands.entity(visual).insert(PendingVisualChild { car });
        }
    } else {
        commands.entity(visual).insert(PendingVisualChild { car });
    }

    commands.entity(root).add_child(visual);
    root
}

/// Marker for a visual root still waiting for its GLB scene.
#[derive(Component)]
pub struct PendingVisualChild {
    pub car: CarId,
}

/// Attach pending scenes once handles exist, then process hierarchy.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn attach_pending_scenes(
    mut commands: Commands,
    handles: Res<CarHandles>,
    gltfs: Res<Assets<Gltf>>,
    pending: Query<(Entity, &PendingVisualChild)>,
) {
    for (visual_entity, p) in pending.iter() {
        let Some(handle) = handles.models.get(&p.car) else {
            continue;
        };
        let Some(gltf) = gltfs.get(handle) else {
            continue;
        };
        commands
            .entity(visual_entity)
            .insert(SceneRoot(gltf.scenes[0].clone()));
        commands
            .entity(visual_entity)
            .remove::<PendingVisualChild>();
        // NOTE: `PendingCarScene` stays on the root until
        // `process_car_scenes` finishes (wheel tagging + materials), even
        // though scene children spawn a frame later.
    }
}

/// After a car's scene has loaded: normalize, fix materials, and tag the
/// player car's wheel nodes. Runs until no pending car remains.
#[allow(clippy::too_many_arguments)]
pub fn process_car_scenes(
    mut commands: Commands,
    handles: Res<CarHandles>,
    gltfs: Res<Assets<Gltf>>,
    pending: Query<(Entity, &CarModelInfo), With<PendingCarScene>>,
    player: Query<(), With<PlayerCar>>,
    scene_nodes: Query<(Entity, Option<&Name>, &ChildOf, &Transform), Added<ChildOf>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (root_entity, info) in pending.iter() {
        let Some(handle) = handles.models.get(&info.car) else {
            continue;
        };
        let Some(_gltf) = gltfs.get(handle) else {
            continue;
        };

        // Collect candidate wheel nodes by name.
        let mut candidates: Vec<(Entity, usize)> = Vec::new();
        let mut found_any = false;
        for (entity, name, _childof, _t) in scene_nodes.iter() {
            let Some(name) = name else { continue };
            let lower = name.as_str().to_ascii_lowercase();
            if lower.contains("whl")
                || lower.contains("wheel")
                || lower.contains("axl")
                || lower.contains("tir")
            {
                found_any = true;
            }
            if lower.contains("shadow") {
                commands.entity(entity).insert(Visibility::Hidden);
                continue;
            }
            for (marker, idx) in info.wheel_markers.iter() {
                if lower.contains(marker) {
                    candidates.push((entity, *idx));
                    break;
                }
            }
        }
        if !found_any {
            // Scene children not spawned yet; try again next frame.
            continue;
        }

        // Keep only top-level wheel nodes (drop children that also matched).
        let candidate_set: std::collections::HashSet<Entity> =
            candidates.iter().map(|(e, _)| *e).collect();
        let mut per_wheel: [Vec<(Entity, Transform)>; 4] = [vec![], vec![], vec![], vec![]];
        for (entity, idx) in candidates {
            let is_child_of_wheel = scene_nodes
                .get(entity)
                .ok()
                .and_then(|(_, _, childof, _)| Some(childof.parent()))
                .map(|p| candidate_set.contains(&p))
                .unwrap_or(false);
            if !is_child_of_wheel {
                let t = *scene_nodes.get(entity).unwrap().3;
                per_wheel[idx].push((entity, t));
            }
        }
        let is_player = player.get(root_entity).is_ok();
        if is_player {
            let mut tagged = 0;
            for (idx, nodes) in per_wheel.iter().enumerate() {
                for (entity, base) in nodes {
                    commands
                        .entity(*entity)
                        .insert(WheelMeshNode { index: idx, base: *base });
                    tagged += 1;
                }
            }
            info!(
                "Wheel nodes tagged for {:?}: {} (per wheel {:?})",
                info.car,
                tagged,
                per_wheel.iter().map(|v| v.len()).collect::<Vec<_>>()
            );
        }

        fix_materials(_gltf, &mut materials);
        commands.entity(root_entity).remove::<PendingCarScene>();
        info!("Car scene processed for {:?}", info.car);
    }
}

/// Name-driven material corrections on the glTF's materials (shared handles —
/// edits propagate to every mesh using them).
fn fix_materials(gltf: &Gltf, materials: &mut Assets<StandardMaterial>) {
    for (name, handle) in gltf.named_materials.iter() {
        let lower = name.to_ascii_lowercase();
        let Some(mat) = materials.get_mut(handle) else {
            continue;
        };
        if lower.contains("drop_shadow") {
            // Shadow blob meshes are hidden via visibility on their nodes; the
            // material itself is left alone.
            continue;
        } else if lower.contains("glass") || lower.contains("window") || lower.contains("wind") {
            mat.metallic = 0.0;
            mat.perceptual_roughness = 0.12;
            mat.base_color.set_alpha(0.42);
            mat.alpha_mode = AlphaMode::Blend;
        } else if lower.contains("chrome") {
            mat.metallic = 0.9;
            mat.perceptual_roughness = 0.14;
        } else if lower.contains("tir") || lower.contains("tire") || lower.contains("tyre") {
            mat.metallic = 0.0;
            mat.perceptual_roughness = 0.95;
            mat.base_color = mat.base_color.with_alpha(1.0);
        } else if lower.contains("rim") || lower.contains("whl") {
            mat.metallic = 0.65;
            mat.perceptual_roughness = 0.35;
        } else if lower.contains("light") || lower.contains("lamp") || lower.contains("tl1") {
            mat.emissive = LinearRgba::rgb(1.4, 1.3, 1.1);
        } else {
            // General fix: recovered game exports often over-metallic painted
            // surfaces; clamp and keep textures.
            mat.metallic = mat.metallic.min(0.1);

            mat.perceptual_roughness = mat.perceptual_roughness.clamp(0.35, 0.92);
        }
    }
}

/// Remove the current car entity (car switching / retries).
pub fn despawn_car(mut commands: Commands, car: Query<Entity, With<PlayerCar>>) {
    for e in car.iter() {
        commands.entity(e).despawn();
    }
}
