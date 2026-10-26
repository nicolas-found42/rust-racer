//! Score/gameplay glue that is not physics-tick-bound: course minimap baking
//! and results-screen world freeze.

use crate::game_state::PlayerCar;
use crate::game_state::*;
use crate::world::build::WorldBuilt;
use avian3d::prelude::*;
use avian3d::schedule::Physics;
use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

pub struct ScoreGamePlugin;

impl Plugin for ScoreGamePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, bake_minimap)
            .add_systems(
                OnEnter(AppState::Results),
                |mut time: ResMut<Time<Physics>>| time.pause(),
            )
            .add_systems(
                OnExit(AppState::Results),
                |mut time: ResMut<Time<Physics>>| time.unpause(),
            )
            .add_systems(
                Update,
                update_minimap_car.run_if(in_state(AppState::Driving)),
            );
    }
}

/// Bake the compact course map into a texture at startup (256², top-down,
/// world -EXTENT..+EXTENT mapped to UV 0..1).
fn bake_minimap(
    mut commands: Commands,
    world: Option<Res<WorldBuilt>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(world) = world else { return };
    let geo = &world.geo;
    let size = 256usize;
    let mut px = vec![0u8; size * size * 4];
    let to_px = |x: f32, z: f32| -> (i64, i64) {
        let u = (x / crate::world::geo::EXTENT * 0.5 + 0.5).clamp(0.0, 1.0);
        let v = (z / crate::world::geo::EXTENT * 0.5 + 0.5).clamp(0.0, 1.0);
        (
            (u * (size - 1) as f32).round() as i64,
            (v * (size - 1) as f32).round() as i64,
        )
    };
    let put = |px: &mut [u8], x: i64, z: i64, c: [u8; 3], r: i64| {
        for dz in -r..=r {
            for dx in -r..=r {
                let (nx, nz) = (x + dx, z + dz);
                if nx < 0 || nz < 0 || nx >= size as i64 || nz >= size as i64 {
                    continue;
                }
                if dx * dx + dz * dz > r * r {
                    continue;
                }
                let i = (nz as usize * size + nx as usize) * 4;
                px[i] = c[0];
                px[i + 1] = c[1];
                px[i + 2] = c[2];
                px[i + 3] = 255;
            }
        }
    };
    // Background: dark land.
    for i in 0..size * size {
        px[i * 4] = 26;
        px[i * 4 + 1] = 30;
        px[i * 4 + 2] = 27;
        px[i * 4 + 3] = 255;
    }
    // Ocean (southwest corner blob).
    let (ox, oz) = to_px(-380.0, -470.0);
    put(&mut px, ox, oz, [20, 42, 60], 60);

    let road_color = [150u8, 150, 148];
    // Pad.
    let (cx, cz) = to_px(0.0, 0.0);
    put(&mut px, cx, cz, [90, 90, 92], 6);
    // Circuit loop.
    for line in geo.connectors.iter() {
        for p in &line.points {
            let (x, z) = to_px(p.x, p.z);
            put(&mut px, x, z, road_color, 1);
        }
    }
    for p in geo.circuit.points.iter() {
        let (x, z) = to_px(p.x, p.z);
        put(&mut px, x, z, [200u8, 130, 40], 2);
    }
    for p in geo.coast.points.iter() {
        let (x, z) = to_px(p.x, p.z);
        put(&mut px, x, z, [40u8, 170, 135], 2);
    }
    // Zones.
    for (c, _) in &geo.zones {
        let (x, z) = to_px(c.x, c.z);
        put(&mut px, x, z, [60u8, 210, 230], 1);
    }

    let img = Image::new(
        Extent3d {
            width: size as u32,
            height: size as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        px,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    let handle = images.add(img);
    commands.insert_resource(MinimapTexture {
        image: Some(handle),
        ..default()
    });
}

/// Track the car dot on the minimap.
fn update_minimap_car(
    mut minimap: ResMut<MinimapTexture>,
    car: Query<&Position, With<PlayerCar>>,
    runtime: Res<VehicleRuntime>,
) {
    let Ok(pos) = car.single() else { return };
    let u = (pos.0.x / crate::world::geo::EXTENT * 0.5 + 0.5).clamp(0.0, 1.0);
    let v = (pos.0.z / crate::world::geo::EXTENT * 0.5 + 0.5).clamp(0.0, 1.0);
    minimap.car_uv = Vec2::new(u, v);
    let f = runtime.forward;
    minimap.car_forward = Vec2::new(f.x, f.z).normalize_or_zero();
}
