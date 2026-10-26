//! Analytic world geometry: the single source of truth for ground height,
//! road corridors, clipping zones, checkpoints, and the minimap layout.
//!
//! Everything (terrain mesh, collision heightfield, test ground, camera
//! queries) derives from these pure functions, so visuals and physics cannot
//! drift apart.
//!
//! Coordinates: world XZ plane, Y up, meters. The car faces -Z at spawn.

use bevy_math::Vec3;

/// Map half-extent. Terrain spans [-extent, extent] in X and Z.
pub const EXTENT: f32 = 460.0;
/// Ocean water plane height.
pub const WATER_Y: f32 = -2.4;

/// Skidpad: center of the practice pad.
pub const PAD_CENTER: Vec2ish = Vec2ish { x: 0.0, z: 0.0 };
pub const PAD_RADIUS: f32 = 45.0;
/// Painted practice circles on the pad.
pub const PAD_RINGS: [f32; 3] = [12.0, 24.0, 36.0];

/// Industrial circuit bounding region (flat, elevated slab).
pub const CIRCUIT_MIN_X: f32 = 110.0;
pub const CIRCUIT_MAX_X: f32 = 370.0;
pub const CIRCUIT_MIN_Z: f32 = -160.0;
pub const CIRCUIT_MAX_Z: f32 = 140.0;
pub const CIRCUIT_Y: f32 = 0.4;

/// Coastal loop.
pub const COAST_CENTER_X: f32 = -190.0;
pub const COAST_CENTER_Z: f32 = -300.0;
pub const COAST_RADIUS: f32 = 168.0;

/// Standard road half-width.
pub const ROAD_HALF_WIDTH: f32 = 7.5;

/// A 2D point (x, z) helper to avoid tuple soup in the API.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec2ish {
    pub x: f32,
    pub z: f32,
}

impl From<(f32, f32)> for Vec2ish {
    fn from(v: (f32, f32)) -> Self {
        Vec2ish { x: v.0, z: v.1 }
    }
}

impl Vec2ish {
    pub fn to_vec3(&self, y: f32) -> Vec3 {
        Vec3::new(self.x, y, self.z)
    }
}

/// Centerline of one drivable road/pad, sampled densely for distance queries.
#[derive(Debug, Clone)]
pub struct Centerline {
    pub points: Vec<Vec3>,
    /// True when the line is a closed loop.
    pub closed: bool,
    /// Extra half-width for corridor blending (rest areas etc.).
    pub half_width: f32,
}

impl Centerline {
    pub fn from_xz(
        points: &[(f32, f32)],
        height_fn: impl Fn(f32, f32) -> f32,
        closed: bool,
        half_width: f32,
    ) -> Self {
        let pts = points
            .iter()
            .map(|(x, z)| Vec3::new(*x, height_fn(*x, *z), *z))
            .collect();
        Centerline {
            points: pts,
            closed,
            half_width,
        }
    }
}

/// Static world description, built once at startup.
pub struct WorldGeo {
    /// Connector road centerlines between areas.
    pub connectors: Vec<Centerline>,
    /// Industrial circuit loop centerline (closed).
    pub circuit: Centerline,
    /// Coastal loop centerline (closed).
    pub coast: Centerline,
    /// Spatial grid over connector samples for O(1) distance queries:
    /// grid[gz][gx] = indices into `all_samples`.
    grid: Vec<Vec<u32>>,
    grid_start: Vec3,
    grid_cell: f32,
    grid_dim: usize,
    all_samples: Vec<Vec3>,
    /// Clipping zones: (center, radius) — proximity scoring only during a
    /// qualifying drift.
    pub zones: Vec<(Vec3, f32)>,
    /// Safe reset poses (position, yaw radians).
    pub checkpoints: Vec<(Vec3, f32)>,
    /// Direction marker signs: (position, yaw, area color, chevron direction).
    pub signs: Vec<SignDef>,
}

#[derive(Debug, Clone, Copy)]
pub struct SignDef {
    pub pos: Vec3,
    /// Facing direction (radians, rotation about Y).
    pub yaw: f32,
    /// Area id: 0 = skidpad, 1 = circuit, 2 = coastal.
    pub area: u8,
    /// Chevron direction in sign-local space: -1 left, 0 straight, 1 right.
    pub turn: i8,
}

/// Smooth low-frequency terrain noise (hills away from roads).
fn terrain_noise(x: f32, z: f32) -> f32 {
    2.6 * (x * 0.011 + 1.7).sin() * (z * 0.009).cos()
        + 1.3 * (x * 0.021).sin() * (z * 0.017 + 0.6).sin()
        + 0.6 * (x * 0.043 + z * 0.037).sin()
}

/// Coastal loop elevation profile: continuous around the circle (integer
/// harmonics).
fn coast_height(theta: f32) -> f32 {
    5.5 * (2.0 * theta + 1.0).sin() + 2.2 * (3.0 * theta).cos() + 1.5
}

/// Point on the coastal loop centerline at angle theta.
fn coast_point(theta: f32) -> Vec2ish {
    Vec2ish {
        x: COAST_CENTER_X + COAST_RADIUS * theta.cos(),
        z: COAST_CENTER_Z + COAST_RADIUS * theta.sin(),
    }
}

/// Height along the coastal loop centerline at a world position (projected).
fn coast_height_at(x: f32, z: f32) -> f32 {
    let theta = (z - COAST_CENTER_Z).atan2(x - COAST_CENTER_X);
    coast_height(theta)
}

/// Base (non-road) height: hills, flattened areas, ocean falloff.
pub fn base_height(x: f32, z: f32) -> f32 {
    // Skidpad disc: flat.
    let d_pad = ((x - PAD_CENTER.x).powi(2) + (z - PAD_CENTER.z).powi(2)).sqrt();
    // Circuit region: flat slab with rounded corners.
    let in_circuit = in_circuit_region(x, z);
    // Ocean falloff: sink terrain beyond the south-west rim toward water.
    let ocean_d = ((x - (COAST_CENTER_X - COAST_RADIUS * 0.35)).powi(2)
        + (z - (COAST_CENTER_Z - COAST_RADIUS * 1.55)).powi(2))
    .sqrt();
    let ocean_sink = -10.0 * smoothstep(180.0, 420.0, ocean_d);

    let mut h = terrain_noise(x, z) + ocean_sink;
    // Flatten the pad and its surroundings.
    h = lerp(
        h,
        0.0,
        1.0 - smoothstep(PAD_RADIUS * 0.8, PAD_RADIUS * 1.5, d_pad),
    );
    if in_circuit {
        // Fully flat at the slab and inside it; blend out over ~30 m beyond
        // the boundary so the circuit meets surrounding terrain smoothly.
        h = lerp(h, CIRCUIT_Y, 1.0 - smoothstep(0.0, 30.0, circuit_region_distance(x, z)));
    }
    h
}

fn in_circuit_region(x: f32, z: f32) -> bool {
    x > CIRCUIT_MIN_X - 20.0
        && x < CIRCUIT_MAX_X + 20.0
        && z > CIRCUIT_MIN_Z - 20.0
        && z < CIRCUIT_MAX_Z + 20.0
}

/// Signed distance to the circuit rectangle (negative inside).
fn circuit_region_distance(x: f32, z: f32) -> f32 {
    let dx = (x - CIRCUIT_MIN_X).max(CIRCUIT_MAX_X - x);
    let dz = (z - CIRCUIT_MIN_Z).max(CIRCUIT_MAX_Z - z);
    let ox = (-dx).max(0.0);
    let oz = (-dz).max(0.0);
    if dx > 0.0 && dz > 0.0 {
        -dx.min(dz)
    } else {
        (ox * ox + oz * oz).sqrt()
    }
}

pub fn smoothstep(a: f32, b: f32, t: f32) -> f32 {
    let t = ((t - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// The industrial circuit loop as control points (clockwise, with hairpin).
pub fn circuit_control_points() -> Vec<(f32, f32)> {
    vec![
        (130.0, -140.0), // start/finish straight (west side, going north)
        (240.0, -148.0), // top straight
        (330.0, -130.0), // NE corner
        (352.0, -60.0),  // east straight
        (350.0, 20.0),   // hairpin entry
        (295.0, 55.0),   // hairpin apex (tight)
        (330.0, 95.0),   // hairpin exit back east... simplified S
        (300.0, 130.0),  // south-east corner
        (200.0, 138.0),  // south straight
        (140.0, 100.0),  // SW corner
        (126.0, 20.0),   // west straight
        (126.0, -60.0),  // west kink
    ]
}

/// Connector polylines (x, z): skidpad ↔ circuit, skidpad ↔ coast,
/// circuit ↔ coast.
pub fn connector_control_points() -> [Vec<(f32, f32)>; 3] {
    [
        vec![
            (PAD_CENTER.x + PAD_RADIUS - 6.0, 0.0),
            (60.0, -4.0),
            (100.0, -20.0),
            (126.0, -70.0),
        ],
        vec![
            (0.0, -(PAD_CENTER.z + PAD_RADIUS - 6.0)),
            (-20.0, -90.0),
            (-60.0, -140.0),
            (-110.0, -165.0),
            (-190.0, -132.0 - 0.0 + 0.0),
        ],
        vec![
            (200.0, 138.0 + 6.0),
            (120.0, 150.0),
            (-40.0, 100.0),
            (-120.0, 10.0),
            (-190.0, -132.0),
        ],
    ]
}

/// Catmull-Rom spline sample of a control-point polyline.
pub fn catmull_rom(points: &[Vec2ish], samples_per_seg: usize, closed: bool) -> Vec<Vec2ish> {
    let n = points.len();
    if n < 2 {
        return points.to_vec();
    }
    let get = |i: i32| -> Vec2ish {
        let i = if closed {
            i.rem_euclid(n as i32)
        } else {
            i.clamp(0, n as i32 - 1)
        };
        points[i as usize]
    };
    let segs = if closed { n } else { n - 1 };
    let mut out = Vec::with_capacity(segs * samples_per_seg);
    for s in 0..segs {
        let s = s as i32;
        let p0 = get(s - 1);
        let p1 = get(s);
        let p2 = get(s + 1);
        let p3 = get(s + 2);
        for t_i in 0..samples_per_seg {
            let t = t_i as f32 / samples_per_seg as f32;
            let t2 = t * t;
            let t3 = t2 * t;
            let x = 0.5
                * ((2.0 * p1.x)
                    + (-p0.x + p2.x) * t
                    + (2.0 * p0.x - 5.0 * p1.x + 4.0 * p2.x - p3.x) * t2
                    + (-p0.x + 3.0 * p1.x - 3.0 * p2.x + p3.x) * t3);
            let z = 0.5
                * ((2.0 * p1.z)
                    + (-p0.z + p2.z) * t
                    + (2.0 * p0.z - 5.0 * p1.z + 4.0 * p2.z - p3.z) * t2
                    + (-p0.z + 3.0 * p1.z - 3.0 * p2.z + p3.z) * t3);
            out.push(Vec2ish { x, z });
        }
    }
    if !closed {
        out.push(*points.last().unwrap());
    }
    out
}

impl WorldGeo {
    /// Build the world description. Expensive-ish (builds the road distance
    /// grid); call once at startup.
    pub fn build() -> Self {
        let cell = 8.0_f32;
        let dim = ((EXTENT * 2.0) / cell).ceil() as usize + 2;

        // Connector centerlines: sample, assign heights by blending endpoints.
        let mut connectors = Vec::new();
        for (idx, ctrl) in connector_control_points().into_iter().enumerate() {
            let pts = catmull_rom(
                &ctrl
                    .iter()
                    .map(|(x, z)| Vec2ish { x: *x, z: *z })
                    .collect::<Vec<_>>(),
                8,
                false,
            );
            let n = pts.len();
            let height_fn = |i: usize| -> f32 {
                let t = i as f32 / (n - 1).max(1) as f32;
                match idx {
                    0 => lerp(0.0, CIRCUIT_Y, smoothstep(0.0, 1.0, t)),
                    1 => lerp(
                        0.0,
                        coast_height_at(pts[n - 1].x, pts[n - 1].z),
                        smoothstep(0.15, 1.0, t),
                    ),
                    _ => lerp(
                        CIRCUIT_Y,
                        coast_height_at(pts[n - 1].x, pts[n - 1].z),
                        smoothstep(0.0, 1.0, t),
                    ),
                }
            };
            let points = pts
                .iter()
                .enumerate()
                .map(|(i, p)| Vec3::new(p.x, height_fn(i), p.z))
                .collect();
            connectors.push(Centerline {
                points,
                closed: false,
                half_width: ROAD_HALF_WIDTH,
            });
        }

        // Coastal loop centerline (closed).
        let mut coast_pts = Vec::new();
        for i in 0..160 {
            let th = i as f32 / 160.0 * std::f32::consts::TAU;
            coast_pts.push(coast_point(th));
        }
        let coast_line = Centerline::from_xz(
            &coast_pts.iter().map(|p| (p.x, p.z)).collect::<Vec<_>>(),
            |x, z| coast_height_at(x, z),
            true,
            ROAD_HALF_WIDTH + 1.5,
        );

        // Circuit centerline (closed).
        let circuit_ctrl = circuit_control_points();
        let circuit_pts = catmull_rom(
            &circuit_ctrl
                .iter()
                .map(|(x, z)| Vec2ish { x: *x, z: *z })
                .collect::<Vec<_>>(),
            10,
            true,
        );
        let circuit_line = Centerline::from_xz(
            &circuit_pts.iter().map(|p| (p.x, p.z)).collect::<Vec<_>>(),
            |x, z| {
                // Inside the circuit slab: flat.
                if in_circuit_region(x, z) {
                    CIRCUIT_Y
                } else {
                    CIRCUIT_Y
                }
            },
            true,
            ROAD_HALF_WIDTH + 1.0,
        );

        // Flatten grid: collect all samples (connectors + circuit + coast).
        let mut all_samples = Vec::new();
        for line in connectors
            .iter()
            .chain(std::iter::once(&circuit_line))
            .chain(std::iter::once(&coast_line))
        {
            all_samples.extend(line.points.iter().copied());
        }

        // Bucket samples into a uniform grid.
        let start = Vec3::new(-EXTENT, 0.0, -EXTENT);
        let mut grid: Vec<Vec<u32>> = vec![Vec::new(); dim * dim];
        for (si, s) in all_samples.iter().enumerate() {
            let gx = (((s.x - start.x) / cell) as usize).min(dim - 1);
            let gz = (((s.z - start.z) / cell) as usize).min(dim - 1);
            grid[gz * dim + gx].push(si as u32);
        }

        // Clipping zones: hairpin apex, tight NE corner, west kink, coastal
        // overlook, and pad rings markers.
        let mut zones = Vec::new();
        zones.push((Vec3::new(300.0, CIRCUIT_Y, 52.0), 14.0)); // hairpin apex
        zones.push((Vec3::new(340.0, CIRCUIT_Y, -128.0), 13.0)); // NE corner
        zones.push((Vec3::new(126.0, CIRCUIT_Y, -95.0), 12.0)); // west kink
        zones.push((Vec3::new(190.0, CIRCUIT_Y, -148.0), 12.0)); // top straight end
        let ov = coast_point(std::f32::consts::PI); // west side of coastal loop
        zones.push((Vec3::new(ov.x, coast_height(ov_theta()), ov.z), 15.0)); // overlook
        for i in 0..4 {
            let th = i as f32 / 4.0 * std::f32::consts::TAU;
            zones.push((Vec3::new(24.0 * th.cos(), 0.02, 24.0 * th.sin()), 10.0));
        }

        // Checkpoints: pad spawn, circuit start, coast north, coast overlook.
        let checkpoints = vec![
            (Vec3::new(0.0, 0.9, 18.0), 0.0),
            (
                Vec3::new(150.0, CIRCUIT_Y + 0.9, -100.0),
                std::f32::consts::FRAC_PI_2,
            ),
            (
                Vec3::new(240.0, CIRCUIT_Y + 0.9, -140.0),
                std::f32::consts::FRAC_PI_2,
            ),
            (Vec3::new(-20.0, 1.4, -95.0), -std::f32::consts::FRAC_PI_4),
            {
                let th = ov_theta();
                let p = coast_point(th);
                (
                    Vec3::new(p.x, coast_height(th) + 0.9, p.z),
                    std::f32::consts::FRAC_PI_2,
                )
            },
            {
                let th = 0.6;
                let p = coast_point(th);
                (
                    Vec3::new(p.x, coast_height(th) + 0.9, p.z),
                    th + std::f32::consts::FRAC_PI_2,
                )
            },
        ];

        // Direction signs at junctions.
        let signs = vec![
            // From pad toward circuit (east).
            SignDef {
                pos: Vec3::new(30.0, 0.0, 0.0),
                yaw: std::f32::consts::FRAC_PI_2,
                area: 1,
                turn: 0,
            },
            // From pad toward coast (south).
            SignDef {
                pos: Vec3::new(0.0, 0.0, -30.0),
                yaw: 0.0,
                area: 2,
                turn: 0,
            },
            // Circuit exit toward coast.
            SignDef {
                pos: Vec3::new(210.0, CIRCUIT_Y, 130.0),
                yaw: -std::f32::consts::FRAC_PI_2,
                area: 2,
                turn: -1,
            },
            // Coast north toward pad.
            SignDef {
                pos: Vec3::new(-186.0, coast_height(-std::f32::consts::FRAC_PI_2), -132.0),
                yaw: std::f32::consts::PI,
                area: 0,
                turn: 0,
            },
            // Overlook sign on the loop.
            SignDef {
                pos: Vec3::new(
                    COAST_CENTER_X - COAST_RADIUS + 2.0,
                    coast_height(std::f32::consts::PI),
                    COAST_CENTER_Z,
                ),
                yaw: std::f32::consts::FRAC_PI_2,
                area: 2,
                turn: 0,
            },
        ];

        let geo = WorldGeo {
            connectors,
            circuit: circuit_line,
            coast: coast_line,
            grid,
            grid_start: start,
            grid_cell: cell,
            grid_dim: dim,
            all_samples,
            zones,
            checkpoints,
            signs,
        };
        // The coastal loop centerline is needed by build systems; expose via a
        // static-ish accessor: rebuild cheaply there instead of storing.
        geo
    }

    /// Distance to the nearest road centerline sample and its height.
    /// Returns (distance, road_height). Grid-accelerated.
    pub fn road_distance_height(&self, x: f32, z: f32) -> (f32, f32) {
        let gx = (((x - self.grid_start.x) / self.grid_cell) as i64)
            .clamp(0, self.grid_dim as i64 - 1) as usize;
        let gz = (((z - self.grid_start.z) / self.grid_cell) as i64)
            .clamp(0, self.grid_dim as i64 - 1) as usize;
        let mut best_d = f32::MAX;
        let mut best_h = 0.0;
        for dz in -1..=1i64 {
            for dx in -1..=1i64 {
                let nx = gx as i64 + dx;
                let nz = gz as i64 + dz;
                if nx < 0 || nz < 0 || nx >= self.grid_dim as i64 || nz >= self.grid_dim as i64 {
                    continue;
                }
                for &si in &self.grid[nz as usize * self.grid_dim + nx as usize] {
                    let s = self.all_samples[si as usize];
                    let d = ((s.x - x) * (s.x - x) + (s.z - z) * (s.z - z)).sqrt();
                    if d < best_d {
                        best_d = d;
                        best_h = s.y;
                    }
                }
            }
        }
        (best_d, best_h)
    }

    /// Master height function used by terrain mesh + collision + tests.
    /// Road corridors conform to road height; elsewhere terrain noise.
    pub fn height(&self, x: f32, z: f32) -> f32 {
        let (d, road_h) = self.road_distance_height(x, z);
        let base = base_height(x, z);
        if d < ROAD_HALF_WIDTH {
            // On the road: pure road height.
            road_h
        } else if d < ROAD_HALF_WIDTH + 14.0 {
            // Shoulder blend.
            let t = smoothstep(ROAD_HALF_WIDTH, ROAD_HALF_WIDTH + 14.0, d);
            // Blend toward whichever is lower/higher smoothly.
            lerp(road_h, base, t)
        } else {
            base
        }
    }

    /// Coastal loop point/elevation helpers for scenery placement.
    pub fn coast_point_height(theta: f32) -> (Vec2ish, f32) {
        (coast_point(theta), coast_height(theta))
    }
}

fn ov_theta() -> f32 {
    std::f32::consts::PI
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coast_height_is_continuous_around_loop() {
        let a = coast_height(0.0);
        let b = coast_height(std::f32::consts::TAU);
        assert!((a - b).abs() < 1e-4);
    }

    #[test]
    fn pad_is_flat() {
        let geo = WorldGeo::build();
        for (x, z) in [(0.0, 0.0), (30.0, 0.0), (0.0, -30.0), (25.0, 25.0)] {
            let h = geo.height(x, z);
            assert!(h.abs() < 0.6, "height at ({x},{z}) = {h}");
        }
    }

    #[test]
    fn circuit_is_flat() {
        let geo = WorldGeo::build();
        for (x, z) in [(150.0, 0.0), (240.0, -140.0), (340.0, 0.0), (240.0, 130.0)] {
            let h = geo.height(x, z);
            assert!((h - CIRCUIT_Y).abs() < 0.8, "height at ({x},{z}) = {h}");
        }
    }

    #[test]
    fn road_height_near_connectors_is_smooth() {
        let geo = WorldGeo::build();
        // Sample along connector 0 near the middle; no jumps.
        let mut last = geo.height(50.0, -4.0);
        for i in 1..20 {
            let x = 50.0 + i as f32 * 2.0;
            let h = geo.height(x, -4.0 - i as f32 * 0.8);
            assert!((h - last).abs() < 1.5, "jump from {last} to {h} at {x}");
            last = h;
        }
    }

    #[test]
    fn checkpoints_are_reasonable_heights() {
        let geo = WorldGeo::build();
        for (pos, _) in &geo.checkpoints {
            let h = geo.height(pos.x, pos.z);
            assert!(
                (h - pos.y + 0.9).abs() < 1.5,
                "checkpoint at {:?} vs terrain {h}",
                pos
            );
        }
    }

    #[test]
    fn zones_are_on_drivable_ground() {
        let geo = WorldGeo::build();
        for (c, r) in &geo.zones {
            let h = geo.height(c.x, c.z);
            assert!(
                (h - c.y).abs() < 2.5,
                "zone at {:?} height {h} vs {}",
                c,
                c.y
            );
            let _ = r;
        }
    }
}
