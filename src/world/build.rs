//! World construction: terrain, roads, pad, circuit props, coastal guardrails,
//! ocean, signage, lighting, and their collision geometry. All geometry derives
//! from `geo::WorldGeo` so collision matches visuals.

use super::geo::{self, WorldGeo, ROAD_HALF_WIDTH};
use avian3d::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::math::Vec3;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use std::f32::consts::PI;

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        // Build at Startup: OnEnter(Loading) fires during `init_state`, before
        // this plugin is added, so the world would never spawn there.
        app.add_systems(Startup, (spawn_lighting, build_world));
    }
}

/// Handle to the built world for other systems.
#[derive(Resource)]
pub struct WorldBuilt {
    pub geo: std::sync::Arc<WorldGeo>,
}

// ---------------------------------------------------------------------------
// Lighting: golden hour sun, cool ambient, atmospheric fog.
// ---------------------------------------------------------------------------

fn spawn_lighting(mut commands: Commands) {
    // Low western sun, warm.
    commands.spawn((
        DirectionalLight {
            shadows_enabled: true,
            illuminance: 16000.0,
            color: Color::srgb(1.0, 0.72, 0.45),
            shadow_depth_bias: 0.02,
            shadow_normal_bias: 0.6,
            affects_lightmapped_mesh_diffuse: false,
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, -2.3, -0.42, 0.0)),
    ));
    // Cool ambient fill from sky/ocean.
    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.45, 0.58, 0.78),
        brightness: 900.0,
        affects_lightmapped_meshes: false,
    });
    // Atmospheric depth: cool haze.
    commands.spawn((DistanceFog {
        color: Color::srgb(0.76, 0.68, 0.62),
        falloff: FogFalloff::Exponential { density: 0.0026 },
        directional_light_color: Color::srgb(1.0, 0.72, 0.45),
        directional_light_exponent: 8.0,
    },));
}

// ---------------------------------------------------------------------------
// Procedural textures.
// ---------------------------------------------------------------------------

fn asphalt_image() -> Image {
    let size = 256usize;
    let mut data = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let i = (y * size + x) * 4;
            // Grainy dark asphalt.
            let n = (x
                .wrapping_mul(374761393)
                .wrapping_add(y.wrapping_mul(668265263)))
                & 0xffff;
            let n = (n >> 3) as f32 / 8192.0; // 0..~1
            let base = 0.085 + 0.05 * n * n;
            // Faint tire-polish banding across v.
            let band = 0.015 * ((y as f32 / size as f32 * 40.0).sin()).abs();
            let v = ((base + band) * 255.0).clamp(0.0, 255.0) as u8;
            data[i] = v;
            data[i + 1] = v;
            data[i + 2] = (v as f32 * 1.06).min(255.0) as u8;
            data[i + 3] = 255;
        }
    }
    Image::new(
        Extent3d {
            width: size as u32,
            height: size as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

fn asphalt_with_markings() -> Image {
    // Road texture: asphalt + white edge lines + dashed center line.
    // v repeats along the road every 16 m; u across the width.
    let size = 256usize;
    let mut img = asphalt_image();
    let Some(data) = img.data.as_mut() else {
        return img;
    };
    let line = |t: f32| -> bool { t.abs() < 0.025 };
    for y in 0..size {
        let v = y as f32 / size as f32;
        let center_dash = (v * 8.0).fract() < 0.45; // dash pattern along road
        for x in 0..size {
            let u = x as f32 / size as f32 - 0.5;
            let i = (y * size + x) * 4;
            let white = if line(u * 2.0 - 0.92) || line(u * 2.0 + 0.92) {
                true
            } else {
                center_dash && line(u * 2.0)
            };
            if white {
                let wear = 0.55 + 0.45 * (((x * 7 + y * 13) & 0xff) as f32 / 255.0);
                data[i] = (225.0 * wear) as u8;
                data[i + 1] = (225.0 * wear) as u8;
                data[i + 2] = (215.0 * wear) as u8;
            }
        }
    }
    img
}

fn concrete_image(tint: [f32; 3]) -> Image {
    let size = 128usize;
    let mut data = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let i = (y * size + x) * 4;
            let n = (x
                .wrapping_mul(1597334677)
                .wrapping_add(y.wrapping_mul(2654435761)))
                & 0xff;
            let v = ((tint[0] + 0.03 * (n as f32 / 255.0)) * 255.0).clamp(0.0, 255.0) as u8;
            data[i] = v;
            data[i + 1] = ((tint[1] + 0.03 * (n as f32 / 255.0)) * 255.0).clamp(0.0, 255.0) as u8;
            data[i + 2] = ((tint[2] + 0.03 * (n as f32 / 255.0)) * 255.0).clamp(0.0, 255.0) as u8;
            data[i + 3] = 255;
        }
    }
    Image::new(
        Extent3d {
            width: size as u32,
            height: size as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

fn curb_image() -> Image {
    // Red/white stripes along v.
    let size = 64usize;
    let mut data = vec![0u8; size * size * 4];
    for y in 0..size {
        let red = (y / 16) % 2 == 0;
        for x in 0..size {
            let i = (y * size + x) * 4;
            let n = ((x.wrapping_mul(506832529).wrapping_add(y)) & 0xf) as f32 / 255.0;
            if red {
                data[i] = (168.0 + n * 30.0) as u8;
                data[i + 1] = 28;
                data[i + 2] = 30;
            } else {
                data[i] = 225;
                data[i + 1] = 222;
                data[i + 2] = 214;
            }
            data[i + 3] = 255;
        }
    }
    Image::new(
        Extent3d {
            width: size as u32,
            height: size as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

fn sign_image(area: u8, turn: i8) -> Image {
    // Chevron direction sign, color-coded by area.
    let (w, h) = (256usize, 256usize);
    let mut data = vec![0u8; w * h * 4];
    let (r, g, b) = match area {
        0 => (40u8, 120u8, 220u8), // skidpad: blue
        1 => (220u8, 130u8, 30u8), // circuit: orange
        _ => (30u8, 170u8, 130u8), // coastal: teal
    };
    // Dark background plate.
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            data[i] = 24;
            data[i + 1] = 28;
            data[i + 2] = 34;
            data[i + 3] = 255;
        }
    }
    // Border.
    for t in 0..w {
        for e in [0usize, h - 1] {
            let i = (e * w + t) * 4;
            data[i] = r;
            data[i + 1] = g;
            data[i + 2] = b;
        }
    }
    for t in 0..h {
        for e in [0usize, w - 1] {
            let i = (t * w + e) * 4;
            data[i] = r;
            data[i + 1] = g;
            data[i + 2] = b;
        }
    }
    // Chevrons: three arrows pointing in `turn` direction (0 = up = straight).
    let draw_chevron = |cx: f32, cy: f32, s: f32, dir: f32, data: &mut [u8]| {
        let (dx, dy) = (dir.sin(), -dir.cos());
        for t_i in (-(s as i32))..=(s as i32) {
            let t = t_i as f32;
            let tt = t as f32;
            for thick_i in -6..=6 {
                let thick = thick_i as f32;
                // V shape pointing along dir.
                for k in [1.0, -1.0] {
                    let px =
                        cx + tt * (dx * k * 0.7 + dy * k * 0.7 * -1.0 * dir.abs()) + thick * dy;
                    let py = cy + tt * 0.6 + tt.abs() * 0.6 * -1.0 + thick * dx;
                    let px = px.round() as i64;
                    let py = py.round() as i64;
                    if px >= 0 && px < w as i64 && py >= 0 && py < h as i64 {
                        let i = (py as usize * w + px as usize) * 4;
                        data[i] = r;
                        data[i + 1] = g;
                        data[i + 2] = b;
                    }
                }
            }
        }
    };
    let dir = match turn {
        -1 => -PI / 2.0,
        1 => PI / 2.0,
        _ => 0.0,
    };
    draw_chevron(w as f32 * 0.5, h as f32 * 0.28, 44.0, dir, &mut data);
    draw_chevron(w as f32 * 0.5, h as f32 * 0.52, 44.0, dir, &mut data);
    draw_chevron(w as f32 * 0.5, h as f32 * 0.76, 44.0, dir, &mut data);
    Image::new(
        Extent3d {
            width: w as u32,
            height: h as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

// ---------------------------------------------------------------------------
// World build.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn build_world(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let geo = std::sync::Arc::new(WorldGeo::build());
    commands.insert_resource(WorldBuilt {
        geo: geo.clone(),
    });
    let geo = &geo;

    // --- Terrain mesh + collider (one authoritative surface) ---
    let cell = 3.0_f32;
    let n = ((geo::EXTENT * 2.0) / cell).ceil() as usize + 1;
    let mut positions = Vec::with_capacity(n * n);
    let mut normals = Vec::with_capacity(n * n);
    let mut uvs = Vec::with_capacity(n * n);
    let mut colors = Vec::with_capacity(n * n);
    for iz in 0..n {
        for ix in 0..n {
            let x = -geo::EXTENT + ix as f32 * cell;
            let z = -geo::EXTENT + iz as f32 * cell;
            let y = geo.height(x, z);
            positions.push([x, y, z]);
            // Central-difference normal.
            let e = 1.5;
            let hx = geo.height(x + e, z) - geo.height(x - e, z);
            let hz = geo.height(x, z + e) - geo.height(x, z - e);
            let n = Vec3::new(-hx, 2.0 * e, -hz).normalize();
            normals.push([n.x, n.y, n.z]);
            uvs.push([ix as f32 * 0.25, iz as f32 * 0.25]);
            // Ground color: golden dry grass with dirt near roads, sand near water.
            let (d_road, _) = geo.road_distance_height(x, z);
            let near_road =
                (1.0 - geo::smoothstep(ROAD_HALF_WIDTH, ROAD_HALF_WIDTH + 10.0, d_road)).max(0.0);
            let noise2 = ((x * 0.07).sin() * (z * 0.06).cos() * 0.5 + 0.5) as f32;
            let mut col = Vec3::new(0.52, 0.50, 0.30) + Vec3::splat(0.06 * noise2); // dry grass
            let sand = 1.0 - geo::smoothstep(geo::WATER_Y + 0.6, geo::WATER_Y + 3.5, y);
            col = col.lerp(Vec3::new(0.72, 0.63, 0.44), sand);
            col = col.lerp(Vec3::new(0.42, 0.36, 0.27), near_road * 0.7);
            colors.push([col.x, col.y, col.z, 1.0]);
        }
    }
    let mut indices = Vec::with_capacity((n - 1) * (n - 1) * 6);
    for iz in 0..n - 1 {
        for ix in 0..n - 1 {
            let a = (iz * n + ix) as u32;
            let b = a + 1;
            let c = a + n as u32;
            let d = c + 1;
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    let mut terrain_mesh = Mesh::new(
        bevy::mesh::PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    terrain_mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions.clone());
    terrain_mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    terrain_mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    terrain_mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    terrain_mesh.insert_indices(bevy::mesh::Indices::U32(indices.clone()));

    let grass_texture = images.add(concrete_image([0.35, 0.40, 0.26]));
    commands.spawn((
        Mesh3d(meshes.add(terrain_mesh.clone())),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color_texture: Some(grass_texture.clone()),
            base_color: Color::WHITE,
            perceptual_roughness: 0.95,
            metallic: 0.0,
            ..default()
        })),
    ));
    // Static collision: same triangles.
    commands.spawn((
        RigidBody::Static,
        Collider::trimesh(
            positions.iter().map(|p| Vec3::from_array(*p)).collect(),
            indices.chunks(3).map(|c| [c[0], c[1], c[2]]).collect(),
        ),
        Transform::default(),
    ));

    // --- Road ribbons ---
    let road_tex = images.add(asphalt_with_markings());
    let road_mat = materials.add(StandardMaterial {
        base_color_texture: Some(road_tex.clone()),
        perceptual_roughness: 0.92,
        metallic: 0.0,
        // Flat overlays follow path direction, so winding varies per ribbon;
        // render both faces.
        cull_mode: None,
        ..default()
    });
    for line in geo
        .connectors
        .iter()
        .chain(once(&geo.circuit))
        .chain(once(&geo.coast))
    {
        let ribbon = road_ribbon_mesh(geo, &line.points, line.closed, line.half_width);
        commands.spawn((Mesh3d(meshes.add(ribbon)), MeshMaterial3d(road_mat.clone())));
    }

    // --- Skidpad disc + painted rings + zone markers ---
    let pad_tex = images.add(asphalt_image());
    let pad_mat = materials.add(StandardMaterial {
        base_color_texture: Some(pad_tex.clone()),
        perceptual_roughness: 0.9,
        metallic: 0.0,
        cull_mode: None,
        ..default()
    });
    commands.spawn((
        Mesh3d(meshes.add(disc_mesh(geo::PAD_RADIUS - 0.4))),
        MeshMaterial3d(pad_mat.clone()),
        Transform::from_translation(Vec3::new(geo::PAD_CENTER.x, 0.015, geo::PAD_CENTER.z)),
    ));

    let paint_white = materials.add(StandardMaterial {
        base_color: Color::srgb(0.92, 0.92, 0.9),
        perceptual_roughness: 0.8,
        metallic: 0.0,
        cull_mode: None,
        ..default()
    });
    for r in geo::PAD_RINGS {
        commands.spawn((
            Mesh3d(meshes.add(ring_mesh(r, 0.35, 96))),
            MeshMaterial3d(paint_white.clone()),
            Transform::from_translation(Vec3::new(geo::PAD_CENTER.x, 0.03, geo::PAD_CENTER.z)),
        ));
    }
    // Practice figure-eight guides (two crossing circles at r=24 offset).
    for sx in [-12.0, 12.0] {
        commands.spawn((
            Mesh3d(meshes.add(ring_mesh(12.0, 0.25, 72))),
            MeshMaterial3d(paint_white.clone()),
            Transform::from_translation(Vec3::new(
                geo::PAD_CENTER.x + sx,
                0.028,
                geo::PAD_CENTER.z,
            )),
        ));
    }

    // Zone markers (bright rings + cones).
    let zone_paint = materials.add(StandardMaterial {
        base_color: Color::srgb(0.2, 0.85, 0.95),
        emissive: LinearRgba::rgb(0.25, 0.9, 1.0) * 1.2,
        cull_mode: None,
        ..default()
    });
    for (c, r) in &geo.zones {
        // Painted flat ring on the ground.
        commands.spawn((
            Mesh3d(meshes.add(ring_mesh(*r * 0.75, 0.5, 48))),
            MeshMaterial3d(zone_paint.clone()),
            Transform::from_translation(*c + Vec3::Y * 0.04),
        ));
        // Small cones at zone center.
        for k in 0..3 {
            let ang = k as f32 / 3.0 * PI * 2.0;
            let p = *c + Vec3::new(ang.cos() * r * 0.8, 0.0, ang.sin() * r * 0.8);
            spawn_cone(&mut commands, &mut meshes, &mut materials, p);
        }
    }

    // --- Curbs on circuit corners ---
    let curb_tex = images.add(curb_image());
    let curb_mat = materials.add(StandardMaterial {
        base_color_texture: Some(curb_tex.clone()),
        metallic: 0.0,
        perceptual_roughness: 0.7,
        ..default()
    });
    for (start, end) in curb_segments(geo) {
        spawn_curb(&mut commands, &mut meshes, curb_mat.clone(), start, end);
    }

    // --- Warehouses & containers (with colliders) ---
    let warehouse_mat = materials.add(StandardMaterial {
        base_color_texture: Some(images.add(concrete_image([0.38, 0.40, 0.42]))),
        metallic: 0.05,
        perceptual_roughness: 0.65,
        ..default()
    });
    let roof_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.28, 0.30, 0.33),
        metallic: 0.3,
        perceptual_roughness: 0.5,
        ..default()
    });
    let warehouses: [(Vec3, Vec3); 3] = [
        (Vec3::new(220.0, 7.0, 60.0), Vec3::new(64.0, 14.0, 26.0)),
        (Vec3::new(300.0, 5.0, -30.0), Vec3::new(44.0, 10.0, 34.0)),
        (Vec3::new(160.0, 6.0, -80.0), Vec3::new(30.0, 12.0, 46.0)),
    ];
    for (pos, size) in warehouses {
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
            MeshMaterial3d(warehouse_mat.clone()),
            Transform::from_translation(pos),
            RigidBody::Static,
            Collider::cuboid(size.x, size.y, size.z),
        ));
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(size.x + 1.6, 0.8, size.z + 1.6))),
            MeshMaterial3d(roof_mat.clone()),
            Transform::from_translation(pos + Vec3::Y * (size.y * 0.5 + 0.4)),
        ));
    }
    // Container stacks near hairpin.
    let container_colors = [
        Color::srgb(0.72, 0.25, 0.15),
        Color::srgb(0.15, 0.4, 0.65),
        Color::srgb(0.75, 0.6, 0.15),
        Color::srgb(0.2, 0.5, 0.3),
    ];
    for k in 0..7 {
        let pos = Vec3::new(
            268.0 + (k % 3) as f32 * 2.6,
            1.3 + (k / 3) as f32 * 2.6,
            24.0 + (k % 4) as f32 * 0.4,
        );
        let col = container_colors[k % container_colors.len()];
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(12.2, 2.6, 2.6))),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: col,
                perceptual_roughness: 0.6,
                metallic: 0.25,
                ..default()
            })),
            Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(0.5)),
            RigidBody::Static,
            Collider::cuboid(12.2, 2.6, 2.6),
        ));
    }

    // --- Guardrails: coastal loop outer edge + circuit outside ---
    let rail_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.75, 0.78, 0.8),
        metallic: 0.8,
        perceptual_roughness: 0.35,
        ..default()
    });
    let rail_post_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.35, 0.37, 0.4),
        metallic: 0.5,
        perceptual_roughness: 0.5,
        ..default()
    });
    let mut guardrail_arcs: Vec<(Vec<Vec3>, f32)> = Vec::new();
    // Coastal outer rail: theta offset outward by half width + 2.
    {
        let mut pts = Vec::new();
        for i in 0..112 {
            let th = i as f32 / 112.0 * PI * 2.0;
            let r = geo::COAST_RADIUS + ROAD_HALF_WIDTH + 3.5;
            let x = geo::COAST_CENTER_X + r * th.cos();
            let z = geo::COAST_CENTER_Z + r * th.sin();
            pts.push(Vec3::new(x, geo.height(x, z), z));
        }
        guardrail_arcs.push((pts, 8.0));
    }
    // Circuit outer rails along straights (skip connectors' openings).
    for seg in circuit_rail_segments() {
        guardrail_arcs.push((seg, 6.0));
    }
    for (pts, spacing) in guardrail_arcs {
        spawn_guardrail(
            &mut commands,
            &mut meshes,
            rail_mat.clone(),
            rail_post_mat.clone(),
            &pts,
            spacing,
            geo,
        );
    }

    // --- Tire barriers around the pad (with gaps at connectors) ---
    let tire_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.06, 0.06, 0.07),
        perceptual_roughness: 0.95,
        metallic: 0.0,
        ..default()
    });
    for i in 0..72 {
        let th = i as f32 / 72.0 * PI * 2.0;
        // Gaps toward connectors (east and south).
        let ang_east = (th - 0.0).abs();
        let ang_south = (th - (-PI / 2.0).rem_euclid(PI * 2.0)).abs() % (PI * 2.0);
        if ang_east < 0.22 || ang_south < 0.22 {
            continue;
        }
        let r = geo::PAD_RADIUS + 5.0;
        let x = geo::PAD_CENTER.x + r * th.cos();
        let z = geo::PAD_CENTER.z + r * th.sin();
        spawn_tire_stack(
            &mut commands,
            &mut meshes,
            tire_mat.clone(),
            Vec3::new(x, 0.0, z),
        );
    }

    // --- Boundary fence (keeps the player in the park) ---
    let fence_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.55, 0.5, 0.42),
        perceptual_roughness: 0.9,
        ..default()
    });
    let wall_len = geo::EXTENT * 2.0 + 40.0;
    for (dx, dz, rot) in [
        (0.0, -geo::EXTENT - 20.0_f32, 0.0),
        (0.0, geo::EXTENT + 20.0, 0.0),
        (-geo::EXTENT - 20.0, 0.0, PI / 2.0),
        (geo::EXTENT + 20.0, 0.0, PI / 2.0),
    ] {
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(wall_len, 4.0, 1.0))),
            MeshMaterial3d(fence_mat.clone()),
            Transform::from_translation(Vec3::new(dx, 2.0, dz))
                .with_rotation(Quat::from_rotation_y(rot)),
            RigidBody::Static,
            Collider::cuboid(wall_len, 4.0, 1.0),
        ));
    }

    // --- Direction signs ---
    for sign in &geo.signs {
        let tex = images.add(sign_image(sign.area, sign.turn));
        let mat = materials.add(StandardMaterial {
            base_color_texture: Some(tex),
            emissive: LinearRgba::rgb(0.9, 0.9, 0.9) * 0.35,
            metallic: 0.0,
            perceptual_roughness: 0.5,
            ..default()
        });
        let h = geo.height(sign.pos.x, sign.pos.z);
        let pole = materials.add(StandardMaterial {
            base_color: Color::srgb(0.4, 0.42, 0.45),
            metallic: 0.6,
            perceptual_roughness: 0.4,
            ..default()
        });
        commands.spawn((
            Mesh3d(meshes.add(Cylinder::new(0.09, 3.2))),
            MeshMaterial3d(pole.clone()),
            Transform::from_translation(Vec3::new(sign.pos.x, h + 1.6, sign.pos.z)),
        ));
        commands.spawn((
            Mesh3d(meshes.add(Plane3d::default().mesh().size(2.6, 2.6))),
            MeshMaterial3d(mat),
            Transform::from_translation(Vec3::new(sign.pos.x, h + 3.6, sign.pos.z))
                .with_rotation(Quat::from_euler(EulerRot::YXZ, sign.yaw, -PI / 2.0, 0.0)),
        ));
    }

    // --- Light poles (few, quality-gated) ---
    let lamp_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.9, 0.7),
        emissive: LinearRgba::rgb(1.0, 0.85, 0.55) * 2.2,
        ..default()
    });
    let pole_m = materials.add(StandardMaterial {
        base_color: Color::srgb(0.3, 0.32, 0.35),
        metallic: 0.6,
        perceptual_roughness: 0.5,
        ..default()
    });
    let lamps = [
        Vec3::new(200.0, 0.0, -100.0),
        Vec3::new(330.0, 0.0, 60.0),
        Vec3::new(150.0, 0.0, 90.0),
        Vec3::new(300.0, 0.0, -130.0),
        Vec3::new(30.0, 0.0, -20.0),
    ];
    for p in lamps {
        let h = geo.height(p.x, p.z);
        commands.spawn((
            Mesh3d(meshes.add(Cylinder::new(0.14, 9.0))),
            MeshMaterial3d(pole_m.clone()),
            Transform::from_translation(Vec3::new(p.x, h + 4.5, p.z)),
        ));
        commands.spawn((
            Mesh3d(meshes.add(Sphere::new(0.4))),
            MeshMaterial3d(lamp_mat.clone()),
            Transform::from_translation(Vec3::new(p.x, h + 9.1, p.z)),
            PointLight {
                intensity: 2600.0,
                color: Color::srgb(1.0, 0.85, 0.6),
                range: 40.0,
                shadows_enabled: false,
                ..default()
            },
        ));
    }

    // --- Ocean ---
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(4200.0, 4200.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.10, 0.22, 0.32),
            metallic: 0.35,
            perceptual_roughness: 0.18,
            ..default()
        })),
        Transform::from_translation(Vec3::new(0.0, geo::WATER_Y, 0.0)),
    ));

    // --- Distant hills (no collision, beyond boundary) ---
    let hill_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.30, 0.33, 0.30),
        perceptual_roughness: 1.0,
        ..default()
    });
    for i in 0..26 {
        let th = i as f32 / 26.0 * PI * 2.0 + 0.13;
        let r = 620.0 + ((i * 37) % 90) as f32;
        let s = 60.0 + ((i * 53) % 80) as f32;
        let x = r * th.cos();
        let z = r * th.sin();
        commands.spawn((
            Mesh3d(meshes.add(Cone::new(s * 0.9, s))),
            MeshMaterial3d(hill_mat.clone()),
            Transform::from_translation(Vec3::new(x, -6.0, z)),
        ));
    }

    // --- Overlook deck on the coastal loop west side ---
    let th = PI;
    let (pt, ph) = WorldGeo::coast_point_height(th);
    let deck_mat = materials.add(StandardMaterial {
        base_color_texture: Some(images.add(concrete_image([0.45, 0.44, 0.42]))),
        perceptual_roughness: 0.9,
        ..default()
    });
    let deck_pos = Vec3::new(pt.x - 8.0, ph + 0.15, pt.z);
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(16.0, 0.3, 26.0))),
        MeshMaterial3d(deck_mat.clone()),
        Transform::from_translation(deck_pos),
        RigidBody::Static,
        Collider::cuboid(16.0, 0.3, 26.0),
    ));
    // Railing posts.
    for k in -2..=2 {
        commands.spawn((
            Mesh3d(meshes.add(Cylinder::new(0.08, 1.1))),
            MeshMaterial3d(rail_post_mat.clone()),
            Transform::from_translation(deck_pos + Vec3::new(-7.5, 0.7, k as f32 * 6.0)),
        ));
    }
}

use std::iter::once;

fn road_ribbon_mesh(geo: &WorldGeo, points: &[Vec3], closed: bool, half_width: f32) -> Mesh {
    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    let mut normals = Vec::new();
    let mut indices = Vec::new();
    let count = points.len();
    let step = if closed { count } else { count - 1 };
    let mut v_dist = 0.0_f32;
    for i in 0..count {
        let p = points[i];
        let prev = points[(i + count - 1) % count];
        let next = points[(i + 1) % count];
        let tangent = if !closed && (i == 0 || i == count - 1) {
            if i == 0 {
                next - p
            } else {
                p - prev
            }
        } else {
            next - prev
        }
        .normalize_or_zero();
        let side = Vec3::new(-tangent.z, 0.0, tangent.x).normalize_or_zero();
        for s in [-1.0, 1.0] {
            let x = p.x + side.x * half_width * s;
            let z = p.z + side.z * half_width * s;
            let y = geo.height(x, z) + 0.045;
            positions.push([x, y, z]);
            normals.push([0.0, 1.0, 0.0]);
            uvs.push([s * 0.5 + 0.5, v_dist / 16.0]);
        }
        if i < count - 1 {
            v_dist += points[i + 1].distance(p);
        }
    }
    for i in 0..step {
        let a = (i * 2) as u32;
        let b = a + 1;
        let c = ((i + 1) % count * 2) as u32;
        let d = c + 1;
        indices.extend_from_slice(&[a, c, b, b, c, d]);
    }
    let mut mesh = Mesh::new(
        bevy::mesh::PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(bevy::mesh::Indices::U32(indices));
    mesh
}

fn disc_mesh(radius: f32) -> Mesh {
    let seg = 96;
    let mut positions = vec![[0.0, 0.0, 0.0]];
    let mut uvs = vec![[0.5, 0.5]];
    let mut normals = vec![[0.0, 1.0, 0.0]];
    for i in 0..=seg {
        let th = i as f32 / seg as f32 * PI * 2.0;
        positions.push([th.cos() * radius, 0.0, th.sin() * radius]);
        uvs.push([0.5 + 0.5 * th.cos(), 0.5 + 0.5 * th.sin()]);
        normals.push([0.0, 1.0, 0.0]);
    }
    let mut indices = Vec::new();
    // Counter-clockwise when viewed from +Y so the disc's front face is up.
    for i in 1..=seg {
        indices.extend_from_slice(&[0, (i + 1) as u32, i as u32]);
    }
    let mut mesh = Mesh::new(
        bevy::mesh::PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(bevy::mesh::Indices::U32(indices));
    mesh
}

fn ring_mesh(radius: f32, width: f32, seg: usize) -> Mesh {
    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    let mut normals = Vec::new();
    for i in 0..=seg {
        let th = i as f32 / seg as f32 * PI * 2.0;
        let (c, s) = (th.cos(), th.sin());
        positions.push([c * (radius + width), 0.0, s * (radius + width)]);
        positions.push([c * (radius - width), 0.0, s * (radius - width)]);
        uvs.push([0.0, 0.0]);
        uvs.push([1.0, 1.0]);
        normals.push([0.0, 1.0, 0.0]);
        normals.push([0.0, 1.0, 0.0]);
    }
    let mut indices = Vec::new();
    for i in 0..seg {
        let a = (i * 2) as u32;
        let b = a + 1;
        let c = a + 2;
        let d = a + 3;
        indices.extend_from_slice(&[a, c, b, b, c, d]);
    }
    let mut mesh = Mesh::new(
        bevy::mesh::PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(bevy::mesh::Indices::U32(indices));
    mesh
}

fn spawn_cone(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    pos: Vec3,
) {
    let mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.9, 0.35, 0.1),
        perceptual_roughness: 0.7,
        ..default()
    });
    commands.spawn((
        Mesh3d(meshes.add(Cone::new(0.35, 0.75))),
        MeshMaterial3d(mat),
        Transform::from_translation(pos + Vec3::Y * 0.37),
    ));
}

fn spawn_tire_stack(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    mat: Handle<StandardMaterial>,
    pos: Vec3,
) {
    let h = crate::world::geo::base_height(pos.x, pos.z);
    commands.spawn((
        Mesh3d(meshes.add(Cylinder::new(0.55, 1.1))),
        MeshMaterial3d(mat),
        Transform::from_translation(pos + Vec3::new(0.0, h + 0.55, 0.0)),
        RigidBody::Static,
        Collider::cylinder(0.55, 1.1),
    ));
}

fn curb_segments(geo: &WorldGeo) -> Vec<(Vec3, Vec3)> {
    // Curbs along the insides of the main circuit corners.
    let mut out = Vec::new();
    let hairpin: [(f32, f32); 6] = [
        (352.0, 28.0),
        (340.0, 48.0),
        (318.0, 56.0),
        (300.0, 52.0),
        (318.0, 74.0),
        (338.0, 92.0),
    ];
    for w in hairpin.windows(2) {
        let (x0, z0) = w[0];
        let (x1, z1) = w[1];
        out.push((
            Vec3::new(x0, geo.height(x0, z0), z0),
            Vec3::new(x1, geo.height(x1, z1), z1),
        ));
    }
    // NE corner curb.
    out.push((Vec3::new(336.0, 0.4, -122.0), Vec3::new(352.0, 0.4, -80.0)));
    // West kink curb.
    out.push((Vec3::new(126.0, 0.4, -70.0), Vec3::new(128.0, 0.4, -40.0)));
    let _ = geo;
    out
}

fn spawn_curb(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    mat: Handle<StandardMaterial>,
    start: Vec3,
    end: Vec3,
) {
    let len = start.distance(end);
    if len < 0.5 {
        return;
    }
    let mid = (start + end) * 0.5;
    let yaw = (end.z - start.z).atan2(end.x - start.x);
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(len, 0.12, 1.4))),
        MeshMaterial3d(mat),
        Transform::from_translation(mid + Vec3::Y * 0.05)
            .with_rotation(Quat::from_rotation_y(-yaw)),
        // Drivable: no wall collider; slight ramp handled by terrain trimesh
        // via a dedicated thin box that matches the visual.
        RigidBody::Static,
        Collider::cuboid(len, 0.12, 1.4),
    ));
}

fn circuit_rail_segments() -> Vec<Vec<Vec3>> {
    // Rails along circuit outer edges on straights, leaving connector gaps.
    let mut rails = Vec::new();
    let straights: [((f32, f32), (f32, f32), usize); 4] = [
        ((134.0, -134.0), (238.0, -142.0), 10),
        ((338.0, -124.0), (356.0, -40.0), 8),
        ((206.0, 144.0), (296.0, 136.0), 8),
        ((128.0, 96.0), (132.0, 16.0), 8),
    ];
    for ((x0, z0), (x1, z1), n) in straights {
        let mut pts = Vec::new();
        for i in 0..=n {
            let t = i as f32 / n as f32;
            let x = x0 + (x1 - x0) * t;
            let z = z0 + (z1 - z0) * t;
            pts.push(Vec3::new(x + (x1 - x0).signum() * 0.0, geo::CIRCUIT_Y, z));
        }
        rails.push(pts);
    }
    rails
}

#[allow(clippy::too_many_arguments)]
fn spawn_guardrail(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    rail_mat: Handle<StandardMaterial>,
    post_mat: Handle<StandardMaterial>,
    pts: &[Vec3],
    spacing: f32,
    geo: &WorldGeo,
) {
    // Sample the polyline every `spacing` meters and spawn rail boxes with
    // box colliders, plus visual posts between.
    let mut acc = 0.0;
    for i in 0..pts.len() - 1 {
        let a = pts[i];
        let b = pts[i + 1];
        let seg_len = a.distance(b);
        let mut t = 0.0;
        while t < seg_len {
            let t_next = (t + spacing).min(seg_len);
            let p0 = a.lerp(b, t / seg_len);
            let p1 = a.lerp(b, t_next / seg_len);
            let len = p0.distance(p1);
            if len > 0.4 {
                let mid = (p0 + p1) * 0.5;
                let yaw = (p1.z - p0.z).atan2(p1.x - p0.x);
                let h = geo.height(mid.x, mid.z);
                commands.spawn((
                    Mesh3d(meshes.add(Cuboid::new(len, 0.42, 0.12))),
                    MeshMaterial3d(rail_mat.clone()),
                    Transform::from_translation(Vec3::new(mid.x, h + 0.75, mid.z))
                        .with_rotation(Quat::from_rotation_y(-yaw)),
                    RigidBody::Static,
                    Collider::cuboid(len, 1.0, 0.3),
                ));
                // Posts at both ends (visual only).
                for p in [p0, p1] {
                    let ph = geo.height(p.x, p.z);
                    commands.spawn((
                        Mesh3d(meshes.add(Cylinder::new(0.07, 0.85))),
                        MeshMaterial3d(post_mat.clone()),
                        Transform::from_translation(Vec3::new(p.x, ph + 0.42, p.z)),
                    ));
                }
            }
            t = t_next;
        }
        acc += seg_len;
    }
    let _ = acc;
}
