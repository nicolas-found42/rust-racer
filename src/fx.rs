//! Tire smoke and skidmark FX.
//!
//! Both effects are two dynamic meshes (one skidmark ring buffer, one smoke
//! billboard pool) whose vertex streams are rewritten in place. Vertices are
//! authored directly in world space, so the owning entities never move.
//!
//! Skidmark quads are appended from the fixed tick (the vehicle writes
//! [`VehicleRuntime`] during physics) and fade on the render clock. Smoke
//! particles are integrated on the render clock, because they are purely
//! cosmetic and must stay smooth even when the fixed tick is throttled.
//!
//! Units are SI: meters, seconds, newtons, radians.

use std::sync::atomic::{AtomicU64, Ordering};

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::image::ImageSampler;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, Mesh, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::game_state::{AppState, GameSettings, VehicleRuntime};

/// Ring-buffer capacity in quads. At ~130 quads/s (four wheels marking every
/// 0.45 m at 15 m/s) this holds roughly the full 18 s mark lifetime.
const SKID_QUADS: usize = 2400;

/// Width of a mark across the tire (m).
const SKID_WIDTH: f32 = 0.24;
/// Lift above the ground along the contact normal (m), avoids z-fighting.
const SKID_LIFT: f32 = 0.015;
/// Minimum travel since the previous mark before a segment is appended (m).
const SKID_MIN_DISTANCE: f32 = 0.45;
/// Wheels below this combined skid intensity leave no marks.
const SKID_MIN_INTENSITY: f32 = 0.12;
/// Wheels below this normal load leave no marks (N).
const SKID_MIN_LOAD: f32 = 200.0;
/// Peak vertex alpha at full skid intensity.
const SKID_MAX_ALPHA: f32 = 0.55;
/// Quads are retired after this age (s).
const SKID_LIFETIME: f32 = 18.0;
/// Exponential alpha decay constant (s); at 18 s alpha is ~13% of write value.
const SKID_FADE_TAU: f32 = 9.0;
/// How often the colour attribute is rebuilt (s).
const SKID_REBUILD_INTERVAL: f32 = 0.25;

/// Hard cap of the smoke pool (highest graphics quality).
const SMOKE_CAPACITY: usize = 340;
/// Spawn rate per qualifying wheel (particles/s).
const SMOKE_RATE: f32 = 40.0;
/// Minimum wheel skid intensity that produces smoke.
const SMOKE_MIN_SKID: f32 = 0.25;
/// Minimum planar speed that produces smoke (m/s).
const SMOKE_MIN_SPEED: f32 = 4.0;
const SMOKE_LIFE_MIN: f32 = 0.9;
const SMOKE_LIFE_MAX: f32 = 1.4;
/// Billboard edge length at birth and at death (m).
const SMOKE_SIZE_START: f32 = 0.35;
const SMOKE_SIZE_END: f32 = 1.6;
/// Peak per-particle alpha.
const SMOKE_ALPHA: f32 = 0.28;
/// Exponential velocity drag (1/s).
const SMOKE_DRAG: f32 = 2.2;
/// Vertical acceleration (m/s²).
const SMOKE_BUOYANCY: f32 = 0.35;

/// One-shot request to wipe every transient FX pool, normally issued by the
/// vehicle module on a reset. Consumers detect a `seq` change.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct FxClearRequest(pub u64);

/// Ask the FX plugin to clear skidmarks and smoke on the next update.
///
/// The sequence is monotonic, so repeated requests in one frame collapse into a
/// single clear without the caller having to read the current value.
pub fn clear_all(commands: &mut Commands) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    commands.insert_resource(FxClearRequest(seq));
}

/// Adds skidmarks and tire smoke to the app.
///
/// Add after `DefaultPlugins`: the startup system allocates a mesh, material,
/// and image through the render asset collections.
pub struct FxPlugin;

impl Plugin for FxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FxClearRequest>()
            .init_resource::<SkidField>()
            .init_resource::<SmokePool>()
            .add_systems(Startup, setup_fx)
            .add_systems(
                FixedUpdate,
                append_skidmarks.run_if(in_state(AppState::Driving)),
            )
            .add_systems(
                Update,
                (poll_fx_clear, emit_smoke, update_smoke, fade_skidmarks).chain(),
            );
    }
}

// ---------------------------------------------------------------------------
// Skidmarks
// ---------------------------------------------------------------------------

/// Ring-buffer state for the skidmark mesh.
#[derive(Resource)]
struct SkidField {
    mesh: Option<Handle<Mesh>>,
    /// Ring slot that receives the next quad.
    cursor: usize,
    /// Age of each slot in seconds; `f32::INFINITY` marks a free slot.
    ages: Vec<f32>,
    /// Alpha captured when the quad was written (skid * [`SKID_MAX_ALPHA`]).
    base_alpha: Vec<f32>,
    /// Last emitted contact point per wheel (`None` breaks the trail).
    last_pos: [Option<Vec3>; 4],
    fade_timer: f32,
    /// Linear-space mark colour (vertex colours replace `base_color`).
    rgb: [f32; 3],
}

impl Default for SkidField {
    fn default() -> Self {
        Self {
            mesh: None,
            cursor: 0,
            ages: vec![f32::INFINITY; SKID_QUADS],
            base_alpha: vec![0.0; SKID_QUADS],
            last_pos: [None; 4],
            fade_timer: 0.0,
            rgb: [0.0; 3],
        }
    }
}

/// A quad staged during one fixed tick, before the mesh attributes are written.
struct PendingQuad {
    slot: usize,
    corners: [[f32; 3]; 4],
    normal: [f32; 3],
    color: [f32; 4],
}

/// Append one connecting quad per wheel that has travelled far enough.
///
/// Runs in `FixedUpdate` while driving: [`VehicleRuntime`] is refreshed by the
/// vehicle plugin during physics, and a paused simulation simply produces no
/// new marks.
fn append_skidmarks(
    runtime: Option<Res<VehicleRuntime>>,
    mut field: ResMut<SkidField>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Some(runtime) = runtime else {
        return;
    };
    let Some(handle) = field.mesh.clone() else {
        return;
    };

    let mut pending: [Option<PendingQuad>; 4] = [None, None, None, None];

    for (wheel, state) in runtime.wheels.iter().enumerate() {
        let qualifies =
            state.grounded && state.skid > SKID_MIN_INTENSITY && state.load > SKID_MIN_LOAD;
        if !qualifies {
            // Lift-off breaks the trail so no quad spans the gap.
            field.last_pos[wheel] = None;
            continue;
        }

        let Some(previous) = field.last_pos[wheel] else {
            field.last_pos[wheel] = Some(state.contact_point);
            continue;
        };
        let current = state.contact_point;
        if previous.distance(current) < SKID_MIN_DISTANCE {
            continue;
        }
        field.last_pos[wheel] = Some(current);

        let mut normal = state.contact_normal.normalize_or_zero();
        if normal == Vec3::ZERO {
            normal = Vec3::Y;
        }
        let direction = current - previous;
        let side = normal.cross(direction);
        let side = if side.length_squared() > 1e-8 {
            side.normalize()
        } else {
            // Degenerate direction: pick any perpendicular in the ground plane.
            normal.cross(Vec3::X).normalize_or_zero()
        };
        if side == Vec3::ZERO {
            continue;
        }

        let half_width = SKID_WIDTH * 0.5;
        let start = previous + normal * SKID_LIFT;
        let end = current + normal * SKID_LIFT;
        let alpha = (state.skid * SKID_MAX_ALPHA).clamp(0.0, SKID_MAX_ALPHA);

        let slot = field.cursor;
        field.cursor = (field.cursor + 1) % SKID_QUADS;
        field.ages[slot] = 0.0;
        field.base_alpha[slot] = alpha;

        pending[wheel] = Some(PendingQuad {
            slot,
            corners: [
                (start - side * half_width).to_array(),
                (start + side * half_width).to_array(),
                (end - side * half_width).to_array(),
                (end + side * half_width).to_array(),
            ],
            normal: normal.to_array(),
            color: [field.rgb[0], field.rgb[1], field.rgb[2], alpha],
        });
    }

    if pending.iter().all(|quad| quad.is_none()) {
        return;
    }
    let Some(mesh) = meshes.get_mut(&handle) else {
        return;
    };

    if let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    {
        for quad in pending.iter().flatten() {
            let base = quad.slot * 4;
            positions[base..base + 4].copy_from_slice(&quad.corners);
        }
    }
    if let Some(VertexAttributeValues::Float32x3(normals)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_NORMAL)
    {
        for quad in pending.iter().flatten() {
            let base = quad.slot * 4;
            normals[base..base + 4].fill(quad.normal);
        }
    }
    if let Some(VertexAttributeValues::Float32x4(colors)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_COLOR)
    {
        for quad in pending.iter().flatten() {
            let base = quad.slot * 4;
            colors[base..base + 4].fill(quad.color);
        }
    }
}

/// Age marks, rebuild their alpha, and retire expired quads.
fn fade_skidmarks(
    time: Res<Time>,
    mut field: ResMut<SkidField>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut expired: Local<Vec<usize>>,
) {
    let dt = time.delta_secs();
    let mut any_active = false;
    expired.clear();
    for (slot, age) in field.ages.iter_mut().enumerate() {
        if !age.is_finite() {
            continue;
        }
        *age += dt;
        if *age >= SKID_LIFETIME {
            *age = f32::INFINITY;
            expired.push(slot);
        } else {
            any_active = true;
        }
    }

    field.fade_timer += dt;
    let rebuild_due = field.fade_timer >= SKID_REBUILD_INTERVAL;

    if !any_active && expired.is_empty() {
        // Nothing to fade and nothing just retired: leave the GPU buffer alone.
        field.fade_timer = 0.0;
        return;
    }
    if !rebuild_due && expired.is_empty() {
        return;
    }
    if rebuild_due {
        field.fade_timer -= SKID_REBUILD_INTERVAL;
        if field.fade_timer < 0.0 {
            field.fade_timer = 0.0;
        }
    }

    let Some(handle) = field.mesh.clone() else {
        return;
    };
    let Some(mesh) = meshes.get_mut(&handle) else {
        return;
    };

    // Alpha of live quads; retired quads go fully transparent.
    if let Some(VertexAttributeValues::Float32x4(colors)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_COLOR)
    {
        for slot in 0..SKID_QUADS {
            let age = field.ages[slot];
            let alpha = if age.is_finite() {
                field.base_alpha[slot] * (-age / SKID_FADE_TAU).exp()
            } else {
                0.0
            };
            let color = [field.rgb[0], field.rgb[1], field.rgb[2], alpha];
            colors[slot * 4..slot * 4 + 4].fill(color);
        }
    }

    // Retired slots collapse to zero area (their triangles become degenerate).
    if !expired.is_empty() {
        if let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
        {
            for &slot in expired.iter() {
                positions[slot * 4..slot * 4 + 4].fill([0.0; 3]);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Smoke
// ---------------------------------------------------------------------------

struct Particle {
    pos: Vec3,
    vel: Vec3,
    age: f32,
    life: f32,
    /// Per-particle size multiplier (0.8..1.2) to break up the pool.
    size: f32,
}

/// Billboard pool state for smoke.
#[derive(Resource)]
struct SmokePool {
    mesh: Option<Handle<Mesh>>,
    particles: Vec<Particle>,
    /// Per-wheel fractional spawn accumulators (particles).
    emit_accum: [f32; 4],
    /// Deterministic jitter state; no external RNG dependency.
    seed: u32,
    /// Linear-space dusty warm grey.
    tint: [f32; 3],
    /// True while the mesh still contains visible quads.
    drawn_last: bool,
}

impl Default for SmokePool {
    fn default() -> Self {
        Self {
            mesh: None,
            particles: Vec::with_capacity(SMOKE_CAPACITY),
            emit_accum: [0.0; 4],
            seed: 0x9E37_79B9,
            tint: [1.0; 3],
            drawn_last: false,
        }
    }
}

/// Spawn smoke from driven wheels, capped by the graphics-quality budget.
fn emit_smoke(
    time: Res<Time>,
    runtime: Option<Res<VehicleRuntime>>,
    settings: Option<Res<GameSettings>>,
    mut pool: ResMut<SmokePool>,
) {
    let Some(runtime) = runtime else {
        return;
    };
    let dt = time.delta_secs().min(0.1);
    let budget = smoke_budget(settings.as_deref());
    let speed = runtime
        .telemetry
        .speed
        .max(Vec3::new(runtime.velocity.x, 0.0, runtime.velocity.z).length());

    if speed <= SMOKE_MIN_SPEED {
        pool.emit_accum = [0.0; 4];
        return;
    }

    for (wheel, state) in runtime.wheels.iter().enumerate() {
        if !state.grounded || state.skid <= SMOKE_MIN_SKID {
            pool.emit_accum[wheel] = 0.0;
            continue;
        }
        pool.emit_accum[wheel] += SMOKE_RATE * dt;
        let mut spawned = pool.emit_accum[wheel].floor();
        pool.emit_accum[wheel] -= spawned;

        let mut normal = state.contact_normal.normalize_or_zero();
        if normal == Vec3::ZERO {
            normal = Vec3::Y;
        }
        let lateral = runtime.velocity - normal * runtime.velocity.dot(normal);

        while spawned >= 1.0 {
            spawned -= 1.0;
            if pool.particles.len() >= budget {
                // Budget exhausted: drop the rest of this tick's quota.
                pool.emit_accum[wheel] = 0.0;
                break;
            }
            let seed = pool.seed;
            pool.seed = seed.wrapping_add(0x9E37_79B9);
            let jitter = Vec3::new(
                hash_unit(seed, 0) * 2.0 - 1.0,
                hash_unit(seed, 1) * 2.0 - 1.0,
                hash_unit(seed, 2) * 2.0 - 1.0,
            ) * 0.6;
            let life = SMOKE_LIFE_MIN + (SMOKE_LIFE_MAX - SMOKE_LIFE_MIN) * hash_unit(seed, 3);
            let size = 0.8 + 0.4 * hash_unit(seed, 4);

            pool.particles.push(Particle {
                pos: state.contact_point + normal * 0.05,
                vel: normal * 0.8 + lateral * 0.25 + jitter,
                age: 0.0,
                life,
                size,
            });
        }
    }
}

/// Integrate particles and rebuild the billboard vertex stream.
fn update_smoke(
    time: Res<Time>,
    cameras: Query<&GlobalTransform, With<Camera3d>>,
    mut pool: ResMut<SmokePool>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let dt = time.delta_secs().min(0.1);

    let mut index = 0;
    while index < pool.particles.len() {
        let particle = &mut pool.particles[index];
        particle.age += dt;
        if particle.age >= particle.life {
            pool.particles.swap_remove(index);
            continue;
        }
        particle.vel *= (-SMOKE_DRAG * dt).exp();
        particle.vel.y += SMOKE_BUOYANCY * dt;
        particle.pos += particle.vel * dt;
        index += 1;
    }

    let count = pool.particles.len();
    let Some(handle) = pool.mesh.clone() else {
        return;
    };
    if count == 0 && !pool.drawn_last {
        return;
    }
    let Some(camera) = cameras.iter().next() else {
        return;
    };
    let Some(mesh) = meshes.get_mut(&handle) else {
        return;
    };

    let rotation = camera.rotation();
    let right = rotation * Vec3::X;
    let up = rotation * Vec3::Y;
    let forward = rotation * Vec3::NEG_Z;

    if let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    {
        for slot in 0..SMOKE_CAPACITY {
            let base = slot * 4;
            if slot < count {
                let particle = &pool.particles[slot];
                let t = (particle.age / particle.life).clamp(0.0, 1.0);
                let half = 0.5 * particle.size * particle_extent(t);
                let center = particle.pos;
                positions[base] = (center - right * half - up * half).to_array();
                positions[base + 1] = (center + right * half - up * half).to_array();
                positions[base + 2] = (center - right * half + up * half).to_array();
                positions[base + 3] = (center + right * half + up * half).to_array();
            } else {
                positions[base..base + 4].fill([0.0; 3]);
            }
        }
    }

    if let Some(VertexAttributeValues::Float32x3(normals)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_NORMAL)
    {
        for slot in 0..SMOKE_CAPACITY {
            let base = slot * 4;
            if slot < count {
                normals[base..base + 4].fill(forward.to_array());
            } else {
                normals[base..base + 4].fill([0.0; 3]);
            }
        }
    }

    if let Some(VertexAttributeValues::Float32x4(colors)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_COLOR)
    {
        for slot in 0..SMOKE_CAPACITY {
            let base = slot * 4;
            let alpha = if slot < count {
                let particle = &pool.particles[slot];
                let t = (particle.age / particle.life).clamp(0.0, 1.0);
                SMOKE_ALPHA * smoothstep(0.0, 0.12, t) * (1.0 - smoothstep(0.55, 1.0, t))
            } else {
                0.0
            };
            colors[base..base + 4].fill([pool.tint[0], pool.tint[1], pool.tint[2], alpha]);
        }
    }

    pool.drawn_last = count > 0;
}

// ---------------------------------------------------------------------------
// Setup, clear, helpers
// ---------------------------------------------------------------------------

fn setup_fx(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut field: ResMut<SkidField>,
    mut pool: ResMut<SmokePool>,
) {
    // --- Skidmark ring buffer -------------------------------------------
    let mut skid_mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    skid_mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; SKID_QUADS * 4]);
    skid_mesh.insert_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vec![[0.0f32, 1.0, 0.0]; SKID_QUADS * 4],
    );
    skid_mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; SKID_QUADS * 4]);
    let mut skid_indices = Vec::with_capacity(SKID_QUADS * 6);
    for quad in 0..SKID_QUADS {
        let base = (quad * 4) as u32;
        skid_indices.extend_from_slice(&[base, base + 1, base + 2, base + 1, base + 3, base + 2]);
    }
    skid_mesh.insert_indices(Indices::U32(skid_indices));
    let skid_handle = meshes.add(skid_mesh);

    field.mesh = Some(skid_handle.clone());
    field.rgb = linear_rgb(0.06, 0.055, 0.05);

    // Unlit + alpha blend; the (very dark) colour and fade live in the vertex
    // stream, the material only supplies the blend state.
    let skid_material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        double_sided: true,
        ..default()
    });
    commands.spawn((
        Mesh3d(skid_handle),
        MeshMaterial3d(skid_material),
        Transform::default(),
        NoFrustumCulling,
        NotShadowCaster,
    ));

    // --- Smoke billboard pool -------------------------------------------
    let mut smoke_mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    smoke_mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![[0.0f32; 3]; SMOKE_CAPACITY * 4],
    );
    smoke_mesh.insert_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vec![[0.0f32, 1.0, 0.0]; SMOKE_CAPACITY * 4],
    );
    let mut uvs = Vec::with_capacity(SMOKE_CAPACITY * 4);
    for _ in 0..SMOKE_CAPACITY {
        // Bottom-left, bottom-right, top-left, top-right.
        uvs.extend_from_slice(&[[0.0f32, 1.0], [1.0, 1.0], [0.0, 0.0], [1.0, 0.0]]);
    }
    smoke_mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    smoke_mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; SMOKE_CAPACITY * 4]);
    let mut smoke_indices = Vec::with_capacity(SMOKE_CAPACITY * 6);
    for quad in 0..SMOKE_CAPACITY {
        let base = (quad * 4) as u32;
        smoke_indices.extend_from_slice(&[base, base + 1, base + 2, base + 1, base + 3, base + 2]);
    }
    smoke_mesh.insert_indices(Indices::U32(smoke_indices));
    let smoke_handle = meshes.add(smoke_mesh);

    pool.mesh = Some(smoke_handle.clone());
    pool.tint = linear_rgb(0.82 * 1.05, 0.78 * 0.95, 0.72 * 0.85);

    let texture = images.add(soft_circle_image());
    let smoke_material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(texture),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        double_sided: true,
        ..default()
    });
    commands.spawn((
        Mesh3d(smoke_handle),
        MeshMaterial3d(smoke_material),
        Transform::default(),
        NoFrustumCulling,
        NotShadowCaster,
    ));
}

/// Wipe both pools when the vehicle module requests a reset.
fn poll_fx_clear(
    request: Res<FxClearRequest>,
    mut seen: Local<u64>,
    mut field: ResMut<SkidField>,
    mut pool: ResMut<SmokePool>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if request.0 == *seen {
        return;
    }
    *seen = request.0;

    field.cursor = 0;
    field.ages.fill(f32::INFINITY);
    field.base_alpha.fill(0.0);
    field.last_pos = [None; 4];
    field.fade_timer = 0.0;
    if let Some(handle) = field.mesh.clone() {
        if let Some(mesh) = meshes.get_mut(&handle) {
            if let Some(VertexAttributeValues::Float32x3(positions)) =
                mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
            {
                positions.fill([0.0; 3]);
            }
            if let Some(VertexAttributeValues::Float32x4(colors)) =
                mesh.attribute_mut(Mesh::ATTRIBUTE_COLOR)
            {
                colors.fill([0.0; 4]);
            }
        }
    }

    pool.particles.clear();
    pool.emit_accum = [0.0; 4];
    pool.drawn_last = false;
    if let Some(handle) = pool.mesh.clone() {
        if let Some(mesh) = meshes.get_mut(&handle) {
            if let Some(VertexAttributeValues::Float32x3(positions)) =
                mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
            {
                positions.fill([0.0; 3]);
            }
            if let Some(VertexAttributeValues::Float32x4(colors)) =
                mesh.attribute_mut(Mesh::ATTRIBUTE_COLOR)
            {
                colors.fill([0.0; 4]);
            }
        }
    }
}

/// Particle budget by graphics quality (0 low, 1 medium, 2 high).
fn smoke_budget(settings: Option<&GameSettings>) -> usize {
    match settings.map(|s| s.settings.graphics_quality) {
        Some(0) => 120,
        Some(1) => 220,
        _ => SMOKE_CAPACITY,
    }
}

/// Billboard edge length at normalized lifetime `t` (0..1).
fn particle_extent(t: f32) -> f32 {
    SMOKE_SIZE_START + (SMOKE_SIZE_END - SMOKE_SIZE_START) * t
}

/// Convert an sRGB triple to the linear-space vertex colour bevy's PBR shader
/// expects (it replaces `base_color` with the vertex colour verbatim).
fn linear_rgb(red: f32, green: f32, blue: f32) -> [f32; 3] {
    let linear = Color::srgb(red, green, blue).to_linear();
    [linear.red, linear.green, linear.blue]
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Deterministic hash in `0.0..1.0`; replaces an RNG dependency for jitter.
fn hash_unit(seed: u32, salt: u32) -> f32 {
    let mut x = seed ^ salt.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    x as f32 * (1.0 / 4_294_967_296.0)
}

/// 64x64 white RGBA texture with a radial alpha falloff for soft smoke.
fn soft_circle_image() -> Image {
    const SIZE: u32 = 64;
    let mut data = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            // Normalized offset from the centre in -1..1.
            let dx = (x as f32 + 0.5) / SIZE as f32 * 2.0 - 1.0;
            let dy = (y as f32 + 0.5) / SIZE as f32 * 2.0 - 1.0;
            let radius = (dx * dx + dy * dy).sqrt();
            let falloff = (1.0 - radius).clamp(0.0, 1.0);
            let alpha = falloff * falloff;
            data.extend_from_slice(&[255, 255, 255, (alpha * 255.0) as u8]);
        }
    }
    let mut image = Image::new(
        Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::linear();
    image
}
