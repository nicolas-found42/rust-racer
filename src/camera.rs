//! Camera rig: spring-damped chase with slide-revealing framing and collision
//! pull-in, rigid hood cam, and a slow orbit cam (garage + driving).

use crate::game_state::*;
use avian3d::prelude::*;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::math::Vec3;
use bevy::prelude::*;

pub struct CameraRigPlugin;

impl Plugin for CameraRigPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CameraState>()
            .init_resource::<CameraRigState>()
            .add_systems(Startup, spawn_camera)
            .add_systems(Update, (update_camera, camera_shake_decay));
    }
}

#[derive(Component)]
pub struct MainCamera;

#[derive(Resource, Default)]
pub struct CameraRigState {
    /// Smoothed camera position.
    pos: Vec3,
    /// Smoothed look target.
    look: Vec3,
    /// Chase yaw accumulator for smooth heading blend.
    yaw: f32,
    orbit_angle: f32,
    shake: f32,
    last_impact_seq: u64,
}

fn spawn_camera(mut commands: Commands) {
    commands.spawn((
        Camera3d::default(),
        MainCamera,
        Transform::from_translation(Vec3::new(0.0, 6.0, 30.0)).looking_at(Vec3::ZERO, Vec3::Y),
        Tonemapping::TonyMcMapface,
        DistanceFog {
            color: Color::srgb(0.76, 0.68, 0.62),
            falloff: FogFalloff::Exponential { density: 0.0026 },
            directional_light_color: Color::srgb(1.0, 0.72, 0.45),
            directional_light_exponent: 8.0,
        },
    ));
}

fn camera_shake_decay(impact: Res<ImpactSignal>, mut rig: ResMut<CameraRigState>, time: Res<Time>) {
    if impact.seq != rig.last_impact_seq {
        rig.last_impact_seq = impact.seq;
        rig.shake = (impact.strength * 0.12).clamp(0.05, 0.35);
    }
    rig.shake = (rig.shake - time.delta_secs() * 0.9).max(0.0);
}

#[allow(clippy::type_complexity)]
fn update_camera(
    mut rig_cfg: ResMut<CameraState>,
    mut rig: ResMut<CameraRigState>,
    mut q_camera: Query<(&mut Transform, &mut Projection), With<MainCamera>>,
    car: Query<(&Position, &Rotation, &LinearVelocity), With<PlayerCar>>,
    runtime: Res<VehicleRuntime>,
    spatial: SpatialQuery,
    world: Option<Res<WorldBuilt>>,
    mode: Res<State<AppState>>,
    time: Res<Time>,
) {
    let dt = time.delta_secs();
    let Ok((mut cam_t, mut proj)) = q_camera.single_mut() else {
        return;
    };

    // Garage: slow orbit around the car (or park center when no car).
    let (car_pos, car_rot, car_vel) = if let Ok((p, r, v)) = car.single() {
        (p.0, r.0, v.0)
    } else {
        (Vec3::new(0.0, 1.0, 12.0), Quat::IDENTITY, Vec3::ZERO)
    };

    let desired_pos;
    let desired_look;

    match rig_cfg.mode {
        CameraMode::Hood => {
            let forward = car_rot * Vec3::NEG_Z;
            let up = car_rot * Vec3::Y;
            desired_pos = car_pos + up * 0.62 + forward * 0.42;
            desired_look = car_pos + up * 0.75 + forward * 12.0;
            // Rigid: no smoothing.
            rig.pos = desired_pos;
            rig.look = desired_look;
        }
        CameraMode::Orbit => {
            rig.orbit_angle += dt * 0.25;
            let r = 8.5;
            let center = car_pos + Vec3::Y * 0.8;
            desired_pos =
                center + Vec3::new(rig.orbit_angle.cos() * r, 3.0, rig.orbit_angle.sin() * r);
            desired_look = center;
            let t = (dt * 4.0).min(1.0);
            rig.pos = rig.pos.lerp(desired_pos, t);
            rig.look = rig.look.lerp(desired_look, t);
        }
        CameraMode::Chase => {
            // Blend heading toward velocity direction so the slide stays
            // visible, bounded look-ahead along the planar velocity.
            let fwd = runtime.forward;
            let planar_v = Vec3::new(car_vel.x, 0.0, car_vel.z);
            let speed = planar_v.length();
            let blend = 0.35_f32.clamp(0.0, 1.0) * (speed / 12.0).min(1.0);
            let dir = if speed > 1.0 {
                let vdir = planar_v / speed;
                let mut d = fwd.lerp(vdir, blend * 0.45);
                if d.length_squared() < 1e-6 {
                    d = fwd;
                }
                d.normalize()
            } else {
                fwd
            };
            let yaw = dir.z.atan2(dir.x);
            // Spring yaw toward target (shortest arc).
            let mut dyaw = yaw - rig.yaw;
            while dyaw > std::f32::consts::PI {
                dyaw -= std::f32::consts::TAU;
            }
            while dyaw < -std::f32::consts::PI {
                dyaw += std::f32::consts::TAU;
            }
            rig.yaw += dyaw * (dt * 5.0).min(1.0);
            let back = Vec3::new(rig.yaw.cos(), 0.0, rig.yaw.sin());

            // Look-ahead: velocity-proportional, bounded.
            let ahead = (planar_v * 0.35).clamp_length_max(7.0);
            let target = car_pos + Vec3::Y * 1.1 + ahead * 0.55;
            let mut pos_target = car_pos - back * 7.2 + Vec3::Y * 2.9 + ahead * 0.2;

            // Camera collision: ray from target toward the desired position;
            // pull in on hits (walls AND terrain).
            let to_cam = pos_target - target;
            let dist = to_cam.length();
            if dist > 0.5 {
                if let Some(hit) = spatial.cast_ray(
                    target,
                    bevy::math::Dir3::new(to_cam / dist).unwrap(),
                    dist,
                    true,
                    &SpatialQueryFilter::from_excluded_entities([runtime
                        .car_entity
                        .unwrap_or(Entity::PLACEHOLDER)]),
                ) {
                    pos_target = target + (to_cam / dist) * (hit.distance - 0.35).max(0.6);
                }
            }

            // Ground clamp: never below terrain + 0.5.
            if let Some(world) = world.as_ref() {
                let ground_h = world.geo.height(pos_target.x, pos_target.z);
                pos_target.y = pos_target.y.max(ground_h + 0.5);
            }

            let t = (dt * 6.0).min(1.0);
            if rig_cfg.snap_requested {
                rig.pos = pos_target;
                rig.look = target;
                rig.yaw = yaw;
            } else {
                rig.pos = rig.pos.lerp(pos_target, t);
                rig.look = rig.look.lerp(target, (dt * 8.0).min(1.0));
            }
            desired_pos = rig.pos;
            desired_look = rig.look;
        }
    }

    rig_cfg.snap_requested = false;

    // Shake offset.
    let shake = rig.shake;
    let offset = if shake > 0.001 {
        let t = time.elapsed_secs() * 47.0;
        Vec3::new(
            (t * 1.3).sin() * shake,
            (t * 1.7).cos() * shake * 0.6,
            (t * 1.1).sin() * shake,
        )
    } else {
        Vec3::ZERO
    };

    cam_t.translation = desired_pos + offset;
    cam_t.look_at(desired_look, Vec3::Y);

    // Subtle FOV with speed for a sense of pace (chase only).
    if let Projection::Perspective(persp) = proj.as_mut() {
        let target_fov = match rig_cfg.mode {
            CameraMode::Chase => (55.0 + (runtime.speed_kmh / 200.0) * 12.0).clamp(55.0, 68.0),
            _ => 55.0,
        };
        persp.fov += (target_fov.to_radians() - persp.fov) * (dt * 3.0).min(1.0);
    }

    let _ = mode;
}

use crate::world::build::WorldBuilt;
