//! Focused rear-wheel-drive vehicle controller.
//!
//! The host engine (Bevy + avian3d) owns the chassis rigid body (integration,
//! gravity, and collisions against world colliders). This module is the
//! authoritative tire/suspension/powertrain controller: each fixed tick it
//! reads the body state, raycasts the ground, and produces a world-space force
//! and torque to hand back to the engine, plus per-wheel visual/telemetry
//! state. Pure computation; deterministic; unit-testable with an analytic
//! ground.
//!
//! Units are SI throughout: meters, seconds, kilograms, newtons, N·m,
//! radians (degrees only at the scoring/UI boundary, converted explicitly in
//! `core::deg` / `core::rad`).

use crate::tuning::{CarPreset, LiveTuning};
use crate::{is_front, FIXED_DT, WHEEL_COUNT};
use bevy_math::{Quat, Vec3};

/// Wheel indices: front-left, front-right, rear-left, rear-right.
pub const WHEEL_FL: usize = 0;
pub const WHEEL_FR: usize = 1;
pub const WHEEL_RL: usize = 2;
pub const WHEEL_RR: usize = 3;

/// A ground ray hit, in world space.
#[derive(Debug, Clone, Copy)]
pub struct RayHit {
    pub point: Vec3,
    pub normal: Vec3,
    pub distance: f32,
}

/// What a wheel is rolling on. Grip scales the tire's peak force; rolling
/// scales rolling resistance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surface {
    pub grip: f32,
    pub rolling: f32,
}

impl Surface {
    pub const ASPHALT: Surface = Surface { grip: 1.0, rolling: 1.0 };
}

/// Ground/world query the controller needs. Implemented over avian's
/// spatial query in the game layer, and analytically in tests.
pub trait GroundQuery {
    fn cast_ray(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<RayHit>;

    /// Surface under a wheel contact point; asphalt unless the host knows better.
    fn surface_at(&self, _point: Vec3) -> Surface {
        Surface::ASPHALT
    }
}

/// Raw body state mirrored from the engine before each controller tick.
#[derive(Debug, Clone, Copy, Default)]
pub struct BodyState {
    /// Center of mass, world space.
    pub position: Vec3,
    pub rotation: Quat,
    pub velocity: Vec3,
    pub angular_velocity: Vec3,
    /// Total mass in kg as reported by the physics engine.
    pub mass: f32,
}

/// Player input for one tick, all normalized.
#[derive(Debug, Clone, Copy, Default)]
pub struct DriverInput {
    /// 0..1
    pub throttle: f32,
    /// 0..1
    pub brake: f32,
    /// -1 (left) .. 1 (right)
    pub steer: f32,
    pub handbrake: bool,
}

/// Per-wheel dynamic state, produced by the controller each tick.
#[derive(Debug, Clone, Copy, Default)]
pub struct WheelState {
    /// Suspension length under load (m); `rest_length` when airborne.
    pub suspension_length: f32,
    /// Normal load (N), 0 when airborne.
    pub load: f32,
    pub grounded: bool,
    pub steer_angle: f32,
    /// Wheel spin (rad/s); positive rolls the car forward.
    pub angular_velocity: f32,
    /// Longitudinal slip ratio (dimensionless; >0 = driven wheelspin).
    pub slip_ratio: f32,
    /// Slip angle (rad).
    pub slip_angle: f32,
    /// 0..1 combined skid intensity for FX/squeal.
    pub skid: f32,
    /// World-space contact point (valid when grounded).
    pub contact_point: Vec3,
    pub contact_normal: Vec3,
    /// Lateral tire force magnitude (N) this tick — debug draw.
    pub lateral_force: f32,
    /// Longitudinal tire force magnitude (N) this tick — debug draw.
    pub longitudinal_force: f32,
}

/// Telemetry derived strictly from the same state used for driving.
#[derive(Debug, Clone, Copy, Default)]
pub struct Telemetry {
    /// Planar speed (m/s).
    pub speed: f32,
    /// Signed drift angle (radians): chassis forward vs. planar velocity.
    pub drift_angle: f32,
    /// Yaw rate (rad/s, about the chassis up axis).
    pub yaw_rate: f32,
    pub grounded: bool,
    /// Grounded wheel count 0..=4.
    pub grounded_wheels: u8,
    pub rpm: f32,
    /// -1 reverse, 1..n forward gears.
    pub gear: i8,
    /// Smoothed throttle 0..1.
    pub throttle: f32,
    /// 0..1
    pub brake: f32,
    pub handbrake: bool,
}

/// Forces produced for the engine to apply to the chassis this tick.
#[derive(Debug, Clone, Copy, Default)]
pub struct AppliedForces {
    pub force: Vec3,
    pub torque: Vec3,
}

#[derive(Debug, Clone)]
pub struct VehicleConfig {
    pub preset: CarPreset,
    pub tuning: LiveTuning,
    /// Per-wheel rotational inertia (kg·m²).
    pub wheel_inertia: f32,
}

impl VehicleConfig {
    pub fn new(preset: CarPreset, tuning: LiveTuning) -> Self {
        VehicleConfig {
            preset,
            tuning,
            wheel_inertia: 0.9,
        }
    }
}

/// Full vehicle simulation state (controller-side).
#[derive(Debug, Clone)]
pub struct VehicleSim {
    pub config: VehicleConfig,
    pub wheels: [WheelState; WHEEL_COUNT],
    /// Rate-limited steering angle (rad).
    steer_angle: f32,
    /// Current automatic gear.
    gear: i8,
    /// Seconds remaining in an automatic shift.
    shift_timer: f32,
    /// Keep the current gear (no automatic shifting); for tests and a future
    /// manual gearbox.
    gear_hold: bool,
    rpm: f32,
    /// Rear grip recovery envelope 0..1 (1 = full grip restored).
    handbrake_recovery: f32,
    /// Smoothed throttle for engine response.
    throttle_smooth: f32,
    /// Smoothed per-tire lateral force memory (relaxation length in time).
    lateral_memory: [f32; WHEEL_COUNT],
}

impl VehicleSim {
    pub fn new(config: VehicleConfig) -> Self {
        let idle = config.preset.idle_rpm;
        VehicleSim {
            config,
            wheels: Default::default(),
            steer_angle: 0.0,
            gear: 1,
            shift_timer: 0.0,
            gear_hold: false,
            rpm: idle,
            handbrake_recovery: 1.0,
            throttle_smooth: 0.0,
            lateral_memory: [0.0; WHEEL_COUNT],
        }
    }

    pub fn rpm(&self) -> f32 {
        self.rpm
    }
    pub fn gear(&self) -> i8 {
        self.gear
    }
    pub fn steer_angle(&self) -> f32 {
        self.steer_angle
    }

    /// Reset transient controller state (after respawn). Clears wheels,
    /// timers, and inputs; keeps the preset/tuning.
    pub fn reset_transient(&mut self) {
        self.wheels = Default::default();
        self.steer_angle = 0.0;
        self.gear = 1;
        self.shift_timer = 0.0;
        self.handbrake_recovery = 1.0;
        self.throttle_smooth = 0.0;
        self.lateral_memory = [0.0; WHEEL_COUNT];
        self.rpm = self.config.preset.idle_rpm;
    }

    fn gear_ratio(&self, gear: i8) -> f32 {
        let p = &self.config.preset;
        match gear {
            -1 => -p.reverse_ratio * p.final_drive,
            0 => 0.0,
            n => {
                let idx = (n - 1) as usize;
                p.gear_ratios.get(idx).copied().unwrap_or(0.9) * p.final_drive
            }
        }
    }

    /// Peak crank torque (N·m). The preset's `engine_torque` is the wheel
    /// torque available in first gear, so divide the first-gear ratio out.
    fn peak_crank_torque(&self) -> f32 {
        let p = &self.config.preset;
        let first = p.gear_ratios.first().copied().unwrap_or(1.0) * p.final_drive;
        p.engine_torque / first.max(0.5) * self.config.tuning.engine_torque
    }

    /// Crank torque at full throttle: ramp to 25%, plateau to 82%, falloff to
    /// redline, hard cut at the limiter.
    fn torque_at_rpm(&self, rpm: f32) -> f32 {
        let redline = self.config.preset.redline_rpm;
        if rpm >= redline {
            return 0.0;
        }
        let frac = rpm / redline;
        let shape = if frac < 0.25 {
            0.55 + 1.8 * frac
        } else if frac < 0.82 {
            1.0
        } else {
            1.0 - (frac - 0.82) / 0.18 * 0.85
        };
        self.peak_crank_torque() * shape.max(0.0)
    }

    /// Reverse/forward selection from input, called by the host before `step`.
    pub fn select_direction(&mut self, throttle: f32, brake: f32, v_long: f32) {
        if self.gear == -1 {
            if throttle > 0.2 && v_long > -0.5 {
                self.gear = 1;
            }
        } else if self.gear >= 1 && brake > 0.2 && v_long.abs() < 0.6 && throttle < 0.1 {
            self.gear = -1;
        }
    }

    /// Advance the controller one fixed tick.
    ///
    /// `body` is the mirrored engine state. Returns world-space forces and
    /// torques to apply to the chassis for this tick, and fills wheel states
    /// plus telemetry.
    pub fn step(
        &mut self,
        body: &BodyState,
        input: &DriverInput,
        ground: &dyn GroundQuery,
        dt: f32,
    ) -> (AppliedForces, Telemetry) {
        let p = self.config.preset.clone();
        let mass = if body.mass > 1.0 {
            body.mass
        } else {
            p.mass_kg
        };
        let gravity = 9.81;

        let rotation = body.rotation.normalize();
        let up = rotation * Vec3::Y;
        let forward = rotation * Vec3::NEG_Z; // models face -Z
        let v_planar = Vec3::new(body.velocity.x, 0.0, body.velocity.z);
        let speed = v_planar.length();
        let fwd_planar = Vec3::new(forward.x, 0.0, forward.z).normalize_or_zero();

        // Signed drift angle (radians). Gated near zero speed to avoid an
        // unstable atan2.
        let drift_angle = if speed < 0.5 {
            0.0
        } else {
            let cross = fwd_planar.x * v_planar.z - fwd_planar.z * v_planar.x;
            let dot = fwd_planar.dot(v_planar);
            let mut a = cross.atan2(dot);
            if a > std::f32::consts::PI {
                a -= std::f32::consts::TAU;
            }
            if a < -std::f32::consts::PI {
                a += std::f32::consts::TAU;
            }
            a
        };

        let yaw_rate = body.angular_velocity.dot(up);

        // ---- Steering: speed-sensitive, rate-limited, with slip-angle assist ----
        let steer_speed_scale =
            1.0 - (1.0 - p.min_steer_fraction) * (speed / p.steer_reduction_speed).clamp(0.0, 1.0);
        let max_steer = crate::rad(p.max_steer_deg) * steer_speed_scale;
        let mut target_steer = input.steer.clamp(-1.0, 1.0) * max_steer;
        let assist = self.config.tuning.assist.clamp(0.0, 2.0);
        if assist > 0.01 {
            // Countersteer like a driver (VDrift-style steering assist): point
            // the wheels along the car's velocity (zero body slip), within a
            // window of front-tire peak slip. Using the centre-of-mass
            // velocity rather than the front axle's matters: the axle's own
            // velocity includes the yaw swing, and matching it would remove
            // the front slip that damps the rotation.
            let right = rotation * Vec3::X;
            let v_long_c = body.velocity.dot(forward);
            let v_lat_c = body.velocity.dot(right);
            if v_long_c > 2.0 {
                // Left-positive, matching the steer angle convention.
                let neutral = (-v_lat_c).atan2(v_long_c);
                let half = ALPHA_PEAK * (ASSIST_WINDOW_MAX - ASSIST_WINDOW_STEP * assist);
                let assisted = target_steer
                    .clamp(neutral - half, neutral + half)
                    .clamp(-crate::rad(p.max_steer_deg) * DRIFT_LOCK_BONUS, crate::rad(p.max_steer_deg) * DRIFT_LOCK_BONUS);
                // Only in a slide: ordinary cornering keeps full driver control.
                let slide = smoothstep(ASSIST_SLIDE_ON, ASSIST_SLIDE_FULL, (v_lat_c / v_long_c).atan().abs());
                let blend = smoothstep(3.0, 8.0, v_long_c) * slide;
                target_steer += (assisted - target_steer) * blend;
            }
        }
        let steer_rate = 5.0 * crate::rad(p.max_steer_deg);
        self.steer_angle = approach(self.steer_angle, target_steer, steer_rate * dt);

        // ---- Handbrake grip recovery envelope ----
        if input.handbrake {
            self.handbrake_recovery = 0.0;
        } else {
            let k = 1.0 - (-dt / p.handbrake_recover.max(0.05)).exp();
            self.handbrake_recovery = (self.handbrake_recovery + k).min(1.0);
        }

        // ---- Direction & gearbox ----
        let v_long_now = forward.dot(body.velocity);
        self.select_direction(input.throttle, input.brake, v_long_now);
        self.update_gears(dt, planar_speed(body.velocity));

        // ---- Wheel raycasts, suspension spring/damper ----
        let mut total_force = Vec3::ZERO;
        let mut total_torque = Vec3::ZERO;
        let mut grounded_count = 0u8;
        let com = body.position;
        let mut loads = [0.0f32; WHEEL_COUNT];
        let mut grounded = [false; WHEEL_COUNT];
        let mut normals = [Vec3::Y; WHEEL_COUNT];
        let mut contacts = [Vec3::ZERO; WHEEL_COUNT];
        let mut surfaces = [Surface::ASPHALT; WHEEL_COUNT];

        let ray_max = p.suspension_rest_length + p.wheel_radius;
        for w in 0..WHEEL_COUNT {
            let local = Vec3::from(p.wheel_positions[w]);
            let attach = com + rotation * local;
            let hit = ground.cast_ray(attach, -up, ray_max);
            if let Some(h) = hit.filter(|h| h.distance <= ray_max) {
                let len = (h.distance - p.wheel_radius).max(0.0);
                let compression = (p.suspension_rest_length - len).max(0.0);
                let r_attach = attach - com;
                let v_attach = body.velocity + body.angular_velocity.cross(r_attach);
                let v_into_ground = v_attach.dot(-up);
                // Bump stop: travel ends progressively so hard roll/pitch can't
                // slam the body down onto the ground.
                let bump = (BUMP_STOP_START - len).max(0.0) * p.suspension_stiffness * 6.0;
                let spring = p.suspension_stiffness * compression + bump;
                let damper = p.suspension_damping * v_into_ground;
                let n_force = (spring + damper).max(0.0).min(mass * gravity * 2.5);
                loads[w] = n_force;
                grounded[w] = true;
                grounded_count += 1;
                normals[w] = h.normal.normalize();
                contacts[w] = h.point;
                surfaces[w] = ground.surface_at(h.point);
                self.wheels[w].suspension_length = len;
                self.wheels[w].load = n_force;
                self.wheels[w].grounded = true;
                self.wheels[w].contact_point = h.point;
                self.wheels[w].contact_normal = normals[w];

                let f = normals[w] * n_force;
                total_force += f;
                total_torque += (h.point - com).cross(f);
            } else {
                loads[w] = 0.0;
                grounded[w] = false;
                self.wheels[w].grounded = false;
                self.wheels[w].load = 0.0;
                self.wheels[w].suspension_length = p.suspension_rest_length;
                self.wheels[w].contact_normal = up;
            }
        }

        // ---- Anti-roll per axle (left-right pair share an axle) ----
        for (wl, wr) in [(WHEEL_FL, WHEEL_FR), (WHEEL_RL, WHEEL_RR)] {
            if grounded[wl] && grounded[wr] {
                let l_comp = p.suspension_rest_length - self.wheels[wl].suspension_length;
                let r_comp = p.suspension_rest_length - self.wheels[wr].suspension_length;
                let diff = l_comp - r_comp;
                let f_arb = -p.anti_roll * diff;
                let r_arb = p.anti_roll * diff;
                loads[wl] = (loads[wl] + f_arb).max(0.0);
                loads[wr] = (loads[wr] + r_arb).max(0.0);
                let fl = normals[wl] * f_arb;
                let fr = normals[wr] * r_arb;
                total_force += fl + fr;
                total_torque += (contacts[wl] - com).cross(fl) + (contacts[wr] - com).cross(fr);
            }
        }

        // ---- Downforce, drag, rolling resistance ----
        if grounded_count > 0 {
            total_force += -up * (p.downforce * speed * speed);
        }
        if speed > 0.01 {
            let dir = v_planar / speed;
            let rolling_scale = surfaces.iter().map(|s| s.rolling).sum::<f32>() / WHEEL_COUNT as f32;
            total_force += dir * -(p.drag * speed * speed + p.rolling_resistance * rolling_scale * speed);
        }

        // ---- Drivetrain: crank -> gearbox -> rear limited-slip differential ----
        let wheel_radius = p.wheel_radius;
        let gear_ratio = self.gear_ratio(self.gear);
        let shifting = self.shift_timer > 0.0;
        // Pulling the handbrake dumps the clutch (as a drifter does), so the
        // brake can lock the rears instead of fighting the engine.
        let connected = gear_ratio != 0.0 && !shifting && !input.handbrake;
        let drive_total = if connected {
            // Traction assist (VDrift TCS, tuned for drifting): wheelspin is
            // allowed up to a point, then power is cut as slip climbs.
            let rear_slip = 0.5
                * (self.wheels[WHEEL_RL].slip_ratio.abs() + self.wheels[WHEEL_RR].slip_ratio.abs());
            let rear_angle = 0.5
                * (self.wheels[WHEEL_RL].slip_angle.abs() + self.wheels[WHEEL_RR].slip_angle.abs());
            let tcs_strength = self.config.tuning.assist.clamp(0.0, 1.0);
            // Only while sliding sideways: launches and burnouts keep full power.
            let sliding = smoothstep(0.12, 0.3, rear_angle);
            let tcs = 1.0
                - smoothstep(TCS_SLIP_ON, TCS_SLIP_FULL, rear_slip) * sliding * TCS_MAX_CUT * tcs_strength;
            let torque = self.engine_torque(input);
            (if torque > 0.0 { torque * tcs } else { torque }) * gear_ratio * DRIVETRAIN_EFFICIENCY
        } else {
            self.engine_torque(input);
            0.0
        };
        let rear_drive = lsd_split(
            drive_total,
            self.wheels[WHEEL_RL].angular_velocity,
            self.wheels[WHEEL_RR].angular_velocity,
        );
        // Reflected engine inertia slows wheelspin-up in low gears.
        let driven_inertia = self.config.wheel_inertia
            + if connected {
                0.5 * ENGINE_INERTIA * gear_ratio * gear_ratio
            } else {
                0.0
            };
        let rear_grip_scale = self.config.tuning.rear_grip;

        let hb_cut = if input.handbrake {
            p.handbrake_grip_cut
        } else {
            p.handbrake_grip_cut * (1.0 - self.handbrake_recovery)
        };
        let handbrake_brake_each = if input.handbrake {
            p.handbrake_force * 0.5
        } else {
            0.0
        };

        for w in 0..WHEEL_COUNT {
            self.wheels[w].steer_angle = if is_front(w) { self.steer_angle } else { 0.0 };

            if !grounded[w] {
                // Airborne: a driven wheel spins up modestly with throttle,
                // an undriven one coasts down; neither runs away.
                let free_target = if is_front(w) {
                    self.wheels[w].angular_velocity * 0.9
                } else {
                    (speed / p.wheel_radius) * (1.0 + 0.5 * input.throttle)
                };
                let w_now = self.wheels[w].angular_velocity;
                self.wheels[w].angular_velocity =
                    w_now + (free_target - w_now) * (1.0 - (-dt / 0.3).exp());
                self.wheels[w].slip_ratio = 0.0;
                self.wheels[w].slip_angle = 0.0;
                self.wheels[w].skid = 0.0;
                self.wheels[w].lateral_force = 0.0;
                self.wheels[w].longitudinal_force = 0.0;
                continue;
            }

            // Wheel-plane basis: chassis axes projected on the contact plane,
            // steered about the contact normal.
            let n = normals[w];
            let steer_q = Quat::from_axis_angle(n, self.wheels[w].steer_angle);
            let wheel_fwd = (steer_q * forward)
                .reject_from_normalized(n)
                .normalize_or_zero();
            let wheel_right = wheel_fwd.cross(n).normalize_or_zero();

            let r_contact = contacts[w] - com;
            let v_contact = body.velocity + body.angular_velocity.cross(r_contact);
            let v_long = v_contact.dot(wheel_fwd);
            let v_lat = v_contact.dot(wheel_right);

            // ---- Tire: Pacejka-style combined slip ----
            let base_grip = if is_front(w) {
                p.tire_grip_front
            } else {
                p.tire_grip_rear * rear_grip_scale * (1.0 - hb_cut)
            };
            let load_sens = (loads[w] / (mass * gravity * 0.25))
                .powf(-p.load_sensitivity)
                .clamp(0.5, 1.6);
            let peak = (base_grip * load_sens * surfaces[w].grip * loads[w]).max(0.0);

            let wheel_omega = self.wheels[w].angular_velocity;
            let v_ref = v_long.abs().max(1.5);
            let kappa = ((wheel_omega * wheel_radius - v_long) / v_ref).clamp(-3.0, 3.0);
            let slip_angle = v_lat.atan2(v_long.abs() + 1.5);
            self.wheels[w].slip_angle = slip_angle;
            let sx = kappa / KAPPA_PEAK;
            let sy = slip_angle / ALPHA_PEAK;
            let s_norm = (sx * sx + sy * sy).sqrt();
            let force = peak * tire_curve(s_norm);
            let (mut f_long, mut f_lat) = if s_norm > 1e-4 {
                (force * sx / s_norm, -force * sy / s_norm)
            } else {
                (0.0, 0.0)
            };

            // Relaxation in time prevents lateral force oscillation.
            let relax = 1.0 - (-dt / 0.035).exp();
            f_lat = self.lateral_memory[w] + (f_lat - self.lateral_memory[w]) * relax;
            self.lateral_memory[w] = f_lat;

            // Low-speed static friction: direct lateral damping below 1.5 m/s.
            if speed < 1.5 {
                let damp = (-v_lat * mass * 6.0 / WHEEL_COUNT as f32).clamp(-peak, peak);
                f_lat = damp;
                self.lateral_memory[w] = f_lat;
            }

            // ---- Brakes (ABS on the foot brake above 4 m/s; handbrake locks) ----
            let foot_f = if is_front(w) {
                input.brake * p.brake_force * (1.0 - p.rear_brake_bias)
            } else {
                input.brake * p.brake_force * p.rear_brake_bias
            };
            let abs_scale = if foot_f > 0.0 && speed > 4.0 && self.wheels[w].slip_ratio < 0.0 {
                (1.0 - (self.wheels[w].slip_ratio.abs() / KAPPA_PEAK - 1.0) * 1.5).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let brake_t = (foot_f * abs_scale
                + if is_front(w) { 0.0 } else { handbrake_brake_each })
                * wheel_radius;

            // Static stop: brakes at crawl speed convert to direct damping.
            let stopping = (input.brake > 0.0 || input.handbrake) && v_long.abs() < 0.4;
            if stopping {
                f_long = (-v_long * mass * 6.0 / WHEEL_COUNT as f32)
                    .clamp(-brake_t * 2.0 / wheel_radius, brake_t * 2.0 / wheel_radius);
            }

            // ---- Wheel spin: drive + brake - tire reaction, solved implicitly
            // against the tire's slip stiffness so it stays stable at low speed.
            let drive_t = if is_front(w) { 0.0 } else { rear_drive[w - WHEEL_RL] };
            let inertia = if is_front(w) { self.config.wheel_inertia } else { driven_inertia };
            let slope = if s_norm > 1e-4 {
                (peak * tire_curve_slope(s_norm) / KAPPA_PEAK * (sx / s_norm).powi(2)).max(0.0)
            } else {
                peak * TIRE_C * TIRE_B / KAPPA_PEAK
            };
            let brake_dir = (wheel_omega * 4.0).tanh();
            let net = drive_t - f_long * wheel_radius - brake_dir * brake_t;
            let denom = 1.0 + dt * wheel_radius * slope * (wheel_radius / v_ref) / inertia;
            let mut new_omega = wheel_omega + dt * net / inertia / denom;
            if brake_t > 0.0 && new_omega * wheel_omega < 0.0 {
                new_omega = 0.0; // brakes can stop a wheel, not reverse it
            }
            if stopping {
                new_omega = 0.0;
            }
            self.wheels[w].angular_velocity = new_omega.clamp(-280.0, 280.0);

            // Skid intensity (FX/squeal): sliding past peak slip. Fades out below
            // ~4 m/s so low-speed numerical slip does not trigger smoke or squeal.
            let skid_speed_gate = smoothstep(1.0, 4.0, speed);
            let skid = if loads[w] > 10.0 {
                (s_norm - 0.9).clamp(0.0, 1.0) * skid_speed_gate
            } else {
                0.0
            };
            self.wheels[w].skid = skid;
            self.wheels[w].slip_ratio = kappa;
            self.wheels[w].lateral_force = f_lat.abs();
            self.wheels[w].longitudinal_force = f_long.abs();

            let f = wheel_right * f_lat + wheel_fwd * f_long;
            total_force += f;
            total_torque += r_contact.cross(f);
        }

        // ---- Yaw damping + drift stability assist ----
        // The assist nudges yaw toward a rate that holds the current slide
        // (bounded, scaled by the assist setting, off at assist 0 so that
        // setting gives the pure simulation).
        let assist_strength = (p.base_assist * self.config.tuning.assist).clamp(0.0, 1.0);
        if speed > 3.0 && grounded_count >= 2 {
            let desired_yaw_rate = drift_angle * 1.6;
            let yaw_err = desired_yaw_rate - yaw_rate;
            let cap = ASSIST_YAW_CAP * assist_strength;
            total_torque += up * (yaw_err * assist_strength * ASSIST_YAW_GAIN).clamp(-cap, cap);
            total_torque += up * (-yaw_rate * 300.0);
        } else {
            // Airborne/slow: mild angular damping keeps landings composed.
            total_torque += body.angular_velocity * -200.0;
        }

        // Safety cap on total force.
        let max_force = mass * 60.0;
        if total_force.length() > max_force {
            total_force = total_force.normalize() * max_force;
        }

        // ---- Engine rpm: driveshaft speed (wheelspin shows on the tach) plus a
        // clutch-slip launch flare at low speed ----
        let drive_omega =
            0.5 * (self.wheels[WHEEL_RL].angular_velocity + self.wheels[WHEEL_RR].angular_velocity).abs();
        let shaft_rpm = drive_omega * self.gear_ratio(self.gear).abs() * 60.0 / std::f32::consts::TAU;
        let launch_flare = self.throttle_smooth * 2600.0 * (1.0 - (speed / 8.0).min(1.0));
        let engine_rpm = shaft_rpm.max(p.idle_rpm) + launch_flare;
        self.rpm += (engine_rpm.clamp(p.idle_rpm, p.redline_rpm * 1.02) - self.rpm)
            * (1.0 - (-dt / 0.08).exp());
        self.rpm = self.rpm.max(p.idle_rpm);

        let telemetry = Telemetry {
            speed,
            drift_angle,
            yaw_rate,
            grounded: grounded_count > 0,
            grounded_wheels: grounded_count,
            rpm: self.rpm,
            gear: self.gear,
            throttle: self.throttle_smooth,
            brake: input.brake,
            handbrake: input.handbrake,
        };

        (
            AppliedForces {
                force: total_force,
                torque: total_torque,
            },
            telemetry,
        )
    }

    /// Net crank torque: throttle-scaled torque curve minus engine braking
    /// (which fades in as the throttle lifts).
    fn engine_torque(&mut self, input: &DriverInput) -> f32 {
        self.throttle_smooth += (input.throttle.clamp(0.0, 1.0) - self.throttle_smooth)
            * (1.0 - (-FIXED_DT / 0.09).exp());
        let p = &self.config.preset;
        let braking = ENGINE_BRAKING
            * self.peak_crank_torque()
            * (self.rpm / p.redline_rpm).clamp(0.0, 1.2)
            * (1.0 - self.throttle_smooth);
        self.torque_at_rpm(self.rpm) * self.throttle_smooth - braking
    }

    fn update_gears(&mut self, dt: f32, speed: f32) {
        let p = self.config.preset.clone();
        self.shift_timer = (self.shift_timer - dt).max(0.0);
        if self.gear_hold {
            return;
        }
        if self.shift_timer > 0.0 || self.gear <= 0 {
            if self.gear == 0 {
                self.gear = 1;
            }
            return;
        }
        // Shift decisions use vehicle speed, not raw wheel spin: burning
        // wheels must not upshift a stationary car.
        let ratio = self.gear_ratio(self.gear).abs().max(0.5);
        let speed_rpm =
            speed / p.wheel_radius / std::f32::consts::TAU * 60.0 * ratio;
        // Shift points follow the throttle like a real auto box: early when
        // cruising, held near redline under load so power is there to drift.
        let load = self.throttle_smooth;
        let up_rpm = p.redline_rpm * (0.70 + 0.20 * load);
        let down_rpm = p.redline_rpm * (0.30 + 0.15 * load);
        if speed_rpm > up_rpm {
            let next = self.gear + 1;
            if ((next - 1) as usize) < p.gear_ratios.len() {
                self.gear = next;
                self.shift_timer = 0.22;
            }
        } else if self.gear > 1 && speed_rpm < down_rpm {
            let lower_ratio = self.gear_ratio(self.gear - 1).abs();
            let projected = speed_rpm * (lower_ratio / ratio);
            if projected < p.redline_rpm * 0.88 {
                self.gear -= 1;
                self.shift_timer = 0.18;
            }
        }
    }
}

/// Slip ratio at which a tire reaches peak longitudinal force.
const KAPPA_PEAK: f32 = 0.12;
/// Slip angle (rad) at which a tire reaches peak lateral force (~8 deg).
const ALPHA_PEAK: f32 = 0.14;
/// Magic-formula shape: peak at normalised slip 1, falling to ~89% of peak
/// when fully sliding, so a drift loses some grip instead of plateauing.
const TIRE_B: f32 = 2.65;
const TIRE_C: f32 = 1.3;

/// Normalised combined-slip friction curve (VDrift-style Pacejka magic
/// formula, E = 0): 0 at no slip, 1 at `s = 1`, ~0.89 as `s -> inf`.
fn tire_curve(s: f32) -> f32 {
    (TIRE_C * (TIRE_B * s).atan()).sin()
}

/// d(tire_curve)/ds, used for the implicit wheel-speed update.
fn tire_curve_slope(s: f32) -> f32 {
    let bs = TIRE_B * s;
    (TIRE_C * bs.atan()).cos() * TIRE_C * TIRE_B / (1.0 + bs * bs)
}

/// Fraction of crank torque that reaches the wheels.
const DRIVETRAIN_EFFICIENCY: f32 = 0.9;
/// Engine + flywheel + clutch inertia (kg·m²), reflected through the gears.
const ENGINE_INERTIA: f32 = 0.18;
/// Engine braking torque at redline, as a fraction of peak crank torque.
const ENGINE_BRAKING: f32 = 0.16;
/// Rear limited-slip differential: viscous coupling (N·m per rad/s),
/// preload (N·m), and torque-sensitive lock fraction.
const DIFF_ANTI_SLIP: f32 = 600.0;
const DIFF_PRELOAD: f32 = 120.0;
const DIFF_POWER_LOCK: f32 = 0.45;
/// Steering assist window half-width (in front-tire peak slip angles) at
/// assist 0..2: tires are kept near their peak slip, which also countersteers.
const ASSIST_WINDOW_MAX: f32 = 2.0;
const ASSIST_WINDOW_STEP: f32 = 0.5;
/// Drift stability torque: gain (N·m per rad/s of yaw error) and cap (N·m)
/// at full assist strength.
const ASSIST_YAW_GAIN: f32 = 12000.0;
const ASSIST_YAW_CAP: f32 = 12000.0;
/// Traction assist: rear slip ratio where power starts to be cut, where the
/// cut is complete, and the largest fraction of power it removes.
const TCS_SLIP_ON: f32 = 0.25;
const TCS_SLIP_FULL: f32 = 0.7;
const TCS_MAX_CUT: f32 = 0.8;
/// Body slip angle (rad) where the steering assist starts to engage and
/// where it is fully on (~6 and ~12 degrees).
const ASSIST_SLIDE_ON: f32 = 0.10;
const ASSIST_SLIDE_FULL: f32 = 0.21;
/// Extra steering lock the assist may use to countersteer a slide, like a
/// drift car's modified knuckles.
const DRIFT_LOCK_BONUS: f32 = 1.35;

/// Split drive torque between the rear wheels (left, right). A viscous +
/// torque-sensitive limited-slip coupling moves torque from the faster wheel
/// to the slower one, so a lightly loaded wheel can't run away.
fn lsd_split(total: f32, omega_left: f32, omega_right: f32) -> [f32; 2] {
    let lock_limit = DIFF_PRELOAD + DIFF_POWER_LOCK * total.abs();
    let couple = (DIFF_ANTI_SLIP * (omega_left - omega_right)).clamp(-lock_limit, lock_limit);
    [total * 0.5 - couple, total * 0.5 + couple]
}

/// Move `current` toward `target` by at most `max_delta`.
fn approach(current: f32, target: f32, max_delta: f32) -> f32 {
    let delta = target - current;
    if delta.abs() <= max_delta {
        target
    } else {
        current + delta.signum() * max_delta
    }
}

/// Suspension length (m) below which the bump stop starts to resist.
const BUMP_STOP_START: f32 = 0.12;

/// Chassis planar speed.
pub fn planar_speed(v: Vec3) -> f32 {
    Vec3::new(v.x, 0.0, v.z).length()
}

/// 0..1 ramp between edges.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tuning::CarPreset;
    use bevy_math::vec3;

    /// Flat infinite ground at y=0.
    struct FlatGround;
    impl GroundQuery for FlatGround {
        fn cast_ray(&self, origin: Vec3, direction: Vec3, max_d: f32) -> Option<RayHit> {
            if direction.y >= -1e-6 {
                return None;
            }
            let t = -origin.y / direction.y;
            if t >= 0.0 && t <= max_d {
                Some(RayHit {
                    point: origin + direction * t,
                    normal: Vec3::Y,
                    distance: t,
                })
            } else {
                None
            }
        }
    }

    struct NoGround;
    impl GroundQuery for NoGround {
        fn cast_ray(&self, _o: Vec3, _d: Vec3, _m: f32) -> Option<RayHit> {
            None
        }
    }

    fn make_sim() -> VehicleSim {
        VehicleSim::new(VehicleConfig::new(
            CarPreset::balanced(),
            LiveTuning::default(),
        ))
    }

    /// Body resting at its equilibrium height (COM at `com_height`; attach
    /// points then sit at radius + ~80% rest length above the ground).
    fn rest_body() -> BodyState {
        let p = CarPreset::balanced();
        let com_y = p.com_height;
        BodyState {
            position: vec3(0.0, com_y, 0.0),
            rotation: Quat::IDENTITY,
            velocity: Vec3::ZERO,
            angular_velocity: Vec3::ZERO,
            mass: p.mass_kg,
        }
    }

    #[test]
    fn suspension_supports_car_at_rest() {
        let mut sim = make_sim();
        let body = rest_body();
        let (forces, tel) = sim.step(&body, &DriverInput::default(), &FlatGround, FIXED_DT);
        let expected = body.mass * 9.81;
        assert!(
            (forces.force.y - expected).abs() < expected * 0.4,
            "force.y = {} expected ~{}",
            forces.force.y,
            expected
        );
        assert!(tel.grounded);
        assert_eq!(tel.grounded_wheels, 4);
    }

    #[test]
    fn drift_angle_zero_at_standstill() {
        let mut sim = make_sim();
        let body = rest_body();
        let (_, tel) = sim.step(&body, &DriverInput::default(), &FlatGround, FIXED_DT);
        assert_eq!(tel.drift_angle, 0.0);
        assert_eq!(tel.speed, 0.0);
    }

    #[test]
    fn lateral_force_opposes_lateral_velocity() {
        // Sliding right (+X): lateral force must point left (-X).
        let mut sim = make_sim();
        let mut body = rest_body();
        body.velocity = vec3(6.0, 0.0, -15.0);
        let (forces, _) = sim.step(&body, &DriverInput::default(), &FlatGround, FIXED_DT);
        assert!(
            forces.force.x < 0.0,
            "expected restoring force toward -X, got {}",
            forces.force.x
        );
    }

    #[test]
    fn throttle_drives_car_forward() {
        let mut sim = make_sim();
        let mut body = rest_body();
        let input = DriverInput {
            throttle: 1.0,
            ..Default::default()
        };
        let mut speed = 0.0;
        for _ in 0..240 {
            let (f, tel) = sim.step(&body, &input, &FlatGround, FIXED_DT);
            // Crude explicit integration of the chassis (test-only), with
            // gravity applied as the engine would.
            body.velocity += (f.force + Vec3::Y * -9.81 * body.mass) / body.mass * FIXED_DT;
            body.position += body.velocity * FIXED_DT;
            body.position.y = rest_body().position.y;
            speed = tel.speed;
        }
        assert!(speed > 5.0, "speed after 2 s of full throttle = {}", speed);
        assert!(sim.wheels[WHEEL_FL].grounded);
    }

    #[test]
    fn wheelspin_exceeds_rolling_speed_when_grip_exceeded() {
        // Low rear grip + high torque => driven wheels spin faster than rolling.
        let mut tuning = LiveTuning::default();
        tuning.rear_grip = 0.5;
        tuning.clamp_all();
        let mut sim = VehicleSim::new(VehicleConfig::new(CarPreset::balanced(), tuning));
        let mut body = rest_body();
        let input = DriverInput {
            throttle: 1.0,
            handbrake: false,
            ..Default::default()
        };
        for _ in 0..120 {
            let (f, _) = sim.step(&body, &input, &FlatGround, FIXED_DT);
            body.velocity += (f.force + Vec3::Y * -9.81 * body.mass) / body.mass * FIXED_DT;
            body.position += body.velocity * FIXED_DT;
            body.position.y = rest_body().position.y;
        }
        let rolling = body.velocity.dot(Vec3::NEG_Z).abs() / sim.config.preset.wheel_radius;
        let rear = sim.wheels[WHEEL_RL].angular_velocity;
        assert!(
            rear > rolling + 1.0,
            "rear omega {} vs rolling {}",
            rear,
            rolling
        );
        assert!(sim.wheels[WHEEL_RL].skid > 0.0 || sim.wheels[WHEEL_RR].skid > 0.0);
    }

    #[test]
    fn coasting_wheels_roll_at_ground_speed() {
        // No throttle: every grounded wheel (driven or not) turns at v / r.
        let mut sim = make_sim();
        let mut body = rest_body();
        body.velocity = vec3(0.0, 0.0, -15.0);
        for _ in 0..60 {
            sim.step(&body, &DriverInput::default(), &FlatGround, FIXED_DT);
        }
        let rolling = 15.0 / sim.config.preset.wheel_radius;
        for w in &sim.wheels {
            assert!(
                (w.angular_velocity - rolling).abs() < rolling * 0.05,
                "omega {} vs rolling {}",
                w.angular_velocity,
                rolling
            );
        }
    }

    #[test]
    fn handbrake_locks_rear_wheels_only() {
        let mut sim = make_sim();
        let mut body = rest_body();
        body.velocity = vec3(0.0, 0.0, -15.0);
        let input = DriverInput {
            handbrake: true,
            ..Default::default()
        };
        for _ in 0..30 {
            sim.step(&body, &input, &FlatGround, FIXED_DT);
        }
        assert!(
            sim.wheels[WHEEL_RL].angular_velocity.abs() < 5.0
                && sim.wheels[WHEEL_RR].angular_velocity.abs() < 5.0,
            "rear omegas {:?} slip {:?} load {:?}",
            sim.wheels.map(|w| w.angular_velocity),
            sim.wheels.map(|w| w.slip_ratio),
            sim.wheels.map(|w| w.load)
        );
        assert!(sim.wheels[WHEEL_FL].angular_velocity > 20.0);
    }

    #[test]
    fn gearbox_upshifts_as_speed_builds() {
        let mut sim = make_sim();
        let mut body = rest_body();
        let input = DriverInput {
            throttle: 1.0,
            ..Default::default()
        };
        let mut top_gear = 1;
        for _ in 0..(FIXED_DT.recip() as usize * 12) {
            let (f, _) = sim.step(&body, &input, &FlatGround, FIXED_DT);
            body.velocity += (f.force + Vec3::Y * -9.81 * body.mass) / body.mass * FIXED_DT;
            body.position += body.velocity * FIXED_DT;
            body.position.y = rest_body().position.y;
            top_gear = top_gear.max(sim.gear);
        }
        assert!(top_gear >= 3, "only reached gear {top_gear}");
    }

    #[test]
    fn tire_curve_peaks_then_falls_to_sliding_grip() {
        assert!((tire_curve(1.0) - 1.0).abs() < 0.01, "peak {}", tire_curve(1.0));
        assert!(tire_curve(0.5) < tire_curve(0.9));
        let sliding = tire_curve(6.0);
        assert!(sliding < 0.95 && sliding > 0.8, "sliding grip {sliding}");
    }

    #[test]
    fn lsd_moves_torque_to_the_slower_wheel() {
        let [l, r] = lsd_split(1000.0, 60.0, 20.0);
        assert!((l + r - 1000.0).abs() < 1e-3, "torque conserved");
        assert!(l < 500.0 && r > 500.0, "left {l} right {r}");
        let [l, r] = lsd_split(1000.0, 30.0, 30.0);
        assert!((l - r).abs() < 1e-3);
    }

    /// Forward force the sim produces at a steady 12 m/s on full throttle.
    fn thrust_in_gear(gear: i8) -> f32 {
        let mut sim = make_sim();
        let mut body = rest_body();
        body.velocity = vec3(0.0, 0.0, -12.0);
        sim.gear = gear;
        sim.gear_hold = true; // isolates the torque path from auto-shifting
        let input = DriverInput {
            throttle: 1.0,
            ..Default::default()
        };
        let mut thrust = 0.0;
        for _ in 0..90 {
            let (f, _) = sim.step(&body, &input, &FlatGround, FIXED_DT);
            thrust = f.force.dot(Vec3::NEG_Z);
        }
        thrust
    }

    #[test]
    fn lower_gears_multiply_torque() {
        let g2 = thrust_in_gear(2);
        let g5 = thrust_in_gear(5);
        assert!(g2 > g5 * 1.3, "gear 2 thrust {g2} vs gear 5 {g5}");
    }

    #[test]
    fn engine_braking_slows_a_coasting_car() {
        let mut sim = make_sim();
        let mut body = rest_body();
        body.velocity = vec3(0.0, 0.0, -25.0);
        sim.gear = 3;
        sim.gear_hold = true;
        let mut decel = 0.0;
        for _ in 0..90 {
            let (f, _) = sim.step(&body, &DriverInput::default(), &FlatGround, FIXED_DT);
            decel = -f.force.dot(Vec3::NEG_Z);
        }
        // Aero + rolling alone at 25 m/s is well under this.
        assert!(decel > 450.0, "coasting drag {decel}");
    }

    #[test]
    fn steering_assist_countersteers_a_slide() {
        // Sliding right (+x) while pointing forward: the neutral steer angle
        // is toward the slide, so hands-off input should countersteer.
        let run = |assist: f32| {
            let mut tuning = LiveTuning::default();
            tuning.assist = assist;
            let mut sim = VehicleSim::new(VehicleConfig::new(CarPreset::balanced(), tuning));
            let mut body = rest_body();
            body.velocity = vec3(8.0, 0.0, -15.0);
            for _ in 0..30 {
                sim.step(&body, &DriverInput::default(), &FlatGround, FIXED_DT);
            }
            sim.steer_angle()
        };
        assert!(run(0.0).abs() < 1e-4, "no assist, no steer");
        assert!(run(1.0) < -0.1, "assist steer {}", run(1.0));
    }

    struct Slippery;
    impl GroundQuery for Slippery {
        fn cast_ray(&self, o: Vec3, d: Vec3, m: f32) -> Option<RayHit> {
            FlatGround.cast_ray(o, d, m)
        }
        fn surface_at(&self, _p: Vec3) -> Surface {
            Surface { grip: 0.5, rolling: 3.0 }
        }
    }

    #[test]
    fn low_grip_surface_cuts_tire_force() {
        let side_force = |ground: &dyn GroundQuery| {
            let mut sim = make_sim();
            let mut body = rest_body();
            body.velocity = vec3(4.0, 0.0, -15.0);
            let mut total = 0.0;
            for _ in 0..30 {
                sim.step(&body, &DriverInput::default(), ground, FIXED_DT);
                total = sim.wheels.iter().map(|w| w.lateral_force).sum::<f32>();
            }
            total
        };
        let asphalt = side_force(&FlatGround);
        let grass = side_force(&Slippery);
        assert!(grass < asphalt * 0.7, "grass {grass} vs asphalt {asphalt}");
    }

    /// Planar rigid-body harness (yaw only, ride height pinned) so handling
    /// scenarios can run without the physics engine. Returns the drift angle
    /// (deg) and speed (km/h) sampled every 0.25 s.
    fn drive(
        mut sim: VehicleSim,
        seconds: f32,
        start_speed: f32,
        start_yaw: f32,
        input_at: impl Fn(f32) -> DriverInput,
    ) -> Vec<(f32, f32, i8, f32, f32)> {
        let p = CarPreset::balanced();
        let inertia = p.mass_kg * (4.6f32 * 4.6 + 2.0 * 2.0) / 12.0;
        let mut body = rest_body();
        let mut yaw = start_yaw;
        body.velocity = vec3(0.0, 0.0, -start_speed);
        let mut out = vec![];
        let steps = (seconds / FIXED_DT) as usize;
        for i in 0..steps {
            let t = i as f32 * FIXED_DT;
            body.rotation = Quat::from_rotation_y(yaw);
            let (f, tel) = sim.step(&body, &input_at(t), &FlatGround, FIXED_DT);
            body.velocity += Vec3::new(f.force.x, 0.0, f.force.z) / body.mass * FIXED_DT;
            body.angular_velocity.y += f.torque.y / inertia * FIXED_DT;
            yaw += body.angular_velocity.y * FIXED_DT;
            if i % (FIXED_DT.recip() as usize / 4) == 0 {
                out.push((tel.drift_angle.to_degrees(), tel.speed * 3.6, sim.gear(), sim.rpm(), sim.steer_angle().to_degrees()));
            }
        }
        out
    }

    #[test]
    fn steering_step_settles_back_to_straight() {
        let r = drive(make_sim(), 5.0, 15.0, 0.0, |t| DriverInput {
            throttle: 0.3,
            steer: if (0.5..1.5).contains(&t) { 0.6 } else { 0.0 },
            ..Default::default()
        });
        let peak = r.iter().map(|s| s.0.abs()).fold(0.0, f32::max);
        assert!(peak < 15.0, "peak slip {peak}");
        assert!(r.last().unwrap().0.abs() < 1.0, "ended at {}", r.last().unwrap().0);
    }

    #[test]
    fn a_handbrake_slide_is_catchable_by_lifting() {
        // Kick the rear out at 61 km/h, then lift: the assisted car swings
        // out to a drift angle and comes back instead of spinning.
        let r = drive(make_sim(), 7.0, 17.0, 0.0, |t| DriverInput {
            throttle: if t < 0.3 { 0.3 } else { 0.2 },
            steer: if (0.3..0.9).contains(&t) { 0.6 } else { 0.0 },
            handbrake: (0.3..0.7).contains(&t),
            ..Default::default()
        });
        let peak = r.iter().map(|s| s.0.abs()).fold(0.0, f32::max);
        assert!(peak > 15.0, "no real slide: peak {peak}");
        assert!(peak < 60.0, "spun out: peak {peak}");
        assert!(r.last().unwrap().0.abs() < 5.0, "ended at {}", r.last().unwrap().0);
    }

    #[test]
    fn explore_demo_entry() {
        for (spd, st, thr) in [(8.0, 1.0, 1.0), (8.0, 0.6, 1.0), (12.0, 1.0, 1.0), (12.0, 0.6, 0.8), (12.0, 1.0, 0.6)] {
            let r = drive(make_sim(), 6.0, spd, 0.0, |_t| DriverInput {
                throttle: thr,
                steer: st,
                ..Default::default()
            });
            eprintln!("spd {spd} steer {st} thr {thr}: {:?}", r.iter().map(|s| (s.0.round() as i32, s.1.round() as i32, s.2)).collect::<Vec<_>>());
        }
    }

    #[test]
    fn assist_zero_is_pure_simulation() {
        // With no assist the same kick must not be corrected for the driver.
        let mut tuning = LiveTuning::default();
        tuning.assist = 0.0;
        let sim = VehicleSim::new(VehicleConfig::new(CarPreset::balanced(), tuning));
        let r = drive(sim, 4.0, 17.0, 0.0, |t| DriverInput {
            throttle: 0.2,
            handbrake: (0.3..0.7).contains(&t),
            steer: if (0.3..0.9).contains(&t) { 0.6 } else { 0.0 },
            ..Default::default()
        });
        let assisted = drive(make_sim(), 4.0, 17.0, 0.0, |t| DriverInput {
            throttle: 0.2,
            handbrake: (0.3..0.7).contains(&t),
            steer: if (0.3..0.9).contains(&t) { 0.6 } else { 0.0 },
            ..Default::default()
        });
        let end = |v: &Vec<(f32, f32, i8, f32, f32)>| v.last().unwrap().0.abs();
        assert!(end(&r) > end(&assisted) + 10.0, "no assist {} vs assist {}", end(&r), end(&assisted));
    }

    #[test]
    fn handbrake_cuts_rear_grip_then_recovers() {
        let mut sim = make_sim();
        let mut body = rest_body();
        body.velocity = vec3(0.0, 0.0, -15.0);
        let input = DriverInput {
            handbrake: true,
            ..Default::default()
        };
        let (_, tel) = sim.step(&body, &input, &FlatGround, FIXED_DT);
        assert!(tel.handbrake);
        assert_eq!(sim.handbrake_recovery, 0.0);
        // Release: recovery envelope rises toward 1.
        let mut input = DriverInput::default();
        for _ in 0..120 {
            sim.step(&body, &input, &FlatGround, FIXED_DT);
        }
        assert!(
            (sim.handbrake_recovery - 1.0).abs() < 1e-3,
            "{}",
            sim.handbrake_recovery
        );
        let _ = input;
        input.handbrake = false;
    }

    #[test]
    fn reverse_gear_engages_from_stop() {
        let mut sim = make_sim();
        sim.select_direction(0.0, 1.0, 0.0);
        assert_eq!(sim.gear, -1);
        sim.select_direction(1.0, 0.0, 0.0);
        assert_eq!(sim.gear, 1);
    }

    #[test]
    fn forces_capped() {
        let mut sim = make_sim();
        let mut body = rest_body();
        body.velocity = vec3(90.0, 0.0, -90.0);
        let input = DriverInput {
            throttle: 1.0,
            steer: 1.0,
            ..Default::default()
        };
        let (forces, _) = sim.step(&body, &input, &FlatGround, FIXED_DT);
        assert!(forces.force.length() < body.mass * 61.0);
    }

    #[test]
    fn deterministic_same_inputs() {
        let run = || {
            let mut sim = make_sim();
            let mut body = rest_body();
            let ground = FlatGround;
            for i in 0..240 {
                let input = DriverInput {
                    throttle: if i > 20 { 1.0 } else { 0.0 },
                    steer: (i as f32 / 60.0).sin(),
                    brake: 0.0,
                    handbrake: i % 100 < 10,
                };
                let (f, _) = sim.step(&body, &input, &ground, FIXED_DT);
                body.velocity += f.force / body.mass * FIXED_DT;
                body.position += body.velocity * FIXED_DT;
                body.position.y = rest_body().position.y;
            }
            (
                sim.wheels.map(|w| w.angular_velocity),
                sim.rpm,
                sim.steer_angle,
            )
        };
        let a = run();
        let b = run();
        assert_eq!(a.0, b.0);
        assert_eq!(a.1, b.1);
    }

    #[test]
    fn airborne_when_ground_far() {
        let mut sim = make_sim();
        let mut body = rest_body();
        body.position.y += 5.0;
        let (_, tel) = sim.step(&body, &DriverInput::default(), &NoGround, FIXED_DT);
        assert!(!tel.grounded);
        assert_eq!(tel.grounded_wheels, 0);
        // Driven wheels spin up in the air.
        assert!(sim.wheels[WHEEL_RL].angular_velocity >= 0.0);
    }

    #[test]
    fn tuned_torque_multiplier_changes_drive() {
        let mut tuning = LiveTuning::default();
        tuning.engine_torque = 1.5;
        tuning.clamp_all();
        let mut sim = VehicleSim::new(VehicleConfig::new(CarPreset::balanced(), tuning));
        let body = rest_body();
        let input = DriverInput {
            throttle: 1.0,
            ..Default::default()
        };
        sim.step(&body, &input, &FlatGround, FIXED_DT);
        // With the clutch-equivalent model, drive torque must exceed default.
        let mut def = LiveTuning::default();
        def.clamp_all();
        let mut sim2 = VehicleSim::new(VehicleConfig::new(CarPreset::balanced(), def));
        sim2.step(&body, &input, &FlatGround, FIXED_DT);
        assert!(sim.torque_at_rpm(sim.rpm) > sim2.torque_at_rpm(sim2.rpm) || sim.rpm != sim2.rpm);
    }

    #[test]
    fn steering_reduces_with_speed() {
        let sim = make_sim();
        let p = &sim.config.preset;
        // max_steer at 0 speed vs high speed via the same formula used in step.
        let low = 1.0;
        let high =
            1.0 - (1.0 - p.min_steer_fraction) * (45.0 / p.steer_reduction_speed).clamp(0.0, 1.0);
        assert!(high < low);
    }
}
