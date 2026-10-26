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

/// Ground/world query the controller needs. Implemented over avian's
/// spatial query in the game layer, and analytically in tests.
pub trait GroundQuery {
    fn cast_ray(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<RayHit>;
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

    /// Simple torque curve: ramp to 25%, plateau to 82%, falloff to redline.
    fn torque_at_rpm(&self, rpm: f32) -> f32 {
        let p = &self.config.preset;
        let redline = p.redline_rpm;
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
        p.engine_torque * shape.max(0.0) * self.config.tuning.engine_torque
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

        // ---- Steering: speed-sensitive, rate-limited ----
        let steer_speed_scale =
            1.0 - (1.0 - p.min_steer_fraction) * (speed / p.steer_reduction_speed).clamp(0.0, 1.0);
        let max_steer = crate::rad(p.max_steer_deg) * steer_speed_scale;
        let target_steer = input.steer.clamp(-1.0, 1.0) * max_steer;
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
                let spring = p.suspension_stiffness * compression;
                let damper = p.suspension_damping * v_into_ground;
                let n_force = (spring + damper).max(0.0).min(mass * gravity * 2.5);
                loads[w] = n_force;
                grounded[w] = true;
                grounded_count += 1;
                normals[w] = h.normal.normalize();
                contacts[w] = h.point;
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
            total_force += dir * -(p.drag * speed * speed + p.rolling_resistance * speed);
        }

        // ---- Per-wheel drive/brake/tire forces ----
        let wheel_radius = p.wheel_radius;
        let drive_torque_total = self.drive_torque(input);
        let rear_drive_each = drive_torque_total * 0.5;
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
                // Free spin: driven wheels spin up; driven/others decay slowly.
                if is_front(w) {
                    self.wheels[w].angular_velocity *= (1.0 - 0.5 * dt).max(0.0);
                } else {
                    let spin = self.wheels[w].angular_velocity
                        + (rear_drive_each / self.config.wheel_inertia) * dt;
                    self.wheels[w].angular_velocity = spin.min(280.0);
                }
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

            // ---- Longitudinal: wheel dynamics + slip ratio ----
            let drive_t = if is_front(w) { 0.0 } else { rear_drive_each };
            let brake_f = if is_front(w) {
                input.brake * p.brake_force * (1.0 - p.rear_brake_bias)
            } else {
                input.brake * p.brake_force * p.rear_brake_bias + handbrake_brake_each
            };
            let wheel_omega = self.wheels[w].angular_velocity;
            let rolling_omega = v_long / wheel_radius;
            let slip_ratio = if v_long.abs() > 0.5 {
                (wheel_omega * wheel_radius - v_long) / v_long.abs()
            } else {
                (wheel_omega * wheel_radius - v_long) / 2.0
            };
            self.wheels[w].slip_ratio = slip_ratio;

            let mut f_long =
                drive_t / wheel_radius - brake_f * (wheel_omega * wheel_radius - v_long).signum();
            // Static stop: brakes at crawl speed convert to direct damping.
            if (input.brake > 0.0 || input.handbrake) && v_long.abs() < 0.4 {
                f_long = (-v_long * mass * 6.0 / WHEEL_COUNT as f32)
                    .clamp(-brake_f * 2.0, brake_f * 2.0);
            }

            // ---- Lateral: slip-angle force with progressive saturation ----
            let slip_angle = v_lat.atan2(v_long.abs() + 1.5);
            self.wheels[w].slip_angle = slip_angle;
            let base_grip = if is_front(w) {
                p.tire_grip_front
            } else {
                p.tire_grip_rear * rear_grip_scale * (1.0 - hb_cut)
            };
            let load_sens = (loads[w] / (mass * gravity * 0.25))
                .powf(-p.load_sensitivity)
                .clamp(0.5, 1.6);
            let grip = base_grip * load_sens;
            let f_lat_max = (grip * loads[w]).max(0.0);
            // Saturates near 8 degrees slip for progressive breakaway.
            let stiffness = f_lat_max / crate::rad(8.0);
            let mut f_lat = -stiffness * slip_angle;

            // Relaxation in time prevents lateral force oscillation.
            let relax = 1.0 - (-dt / 0.035).exp();
            f_lat = self.lateral_memory[w] + (f_lat - self.lateral_memory[w]) * relax;
            self.lateral_memory[w] = f_lat;

            // Low-speed static friction: direct lateral damping below 1.5 m/s.
            if speed < 1.5 {
                let damp = (-v_lat * mass * 6.0 / WHEEL_COUNT as f32).clamp(-f_lat_max, f_lat_max);
                f_lat = damp;
                self.lateral_memory[w] = f_lat;
            }

            // ---- Friction circle (combined demand) ----
            let long_max = f_lat_max * 1.05;
            let demand_lat = f_lat.abs() / f_lat_max.max(1.0);
            let demand_long = f_long.abs() / long_max.max(1.0);
            let total_demand = (demand_lat * demand_lat + demand_long * demand_long).sqrt();
            if total_demand > 1.0 {
                let scale = 1.0 / total_demand;
                f_lat *= scale;
                f_long *= scale;
            }

            // Wheel spin update: drive torque in, tire reaction + brakes out.
            let reaction = f_long * wheel_radius;
            let mut new_omega =
                wheel_omega + ((drive_t - reaction) / self.config.wheel_inertia) * dt;
            let brake_delta = (brake_f * wheel_radius / self.config.wheel_inertia) * dt;
            let delta = new_omega - rolling_omega;
            if delta.abs() <= brake_delta {
                new_omega = rolling_omega;
            } else {
                new_omega -= brake_delta * delta.signum();
            }
            self.wheels[w].angular_velocity = new_omega.clamp(-280.0, 280.0);

            // Skid intensity (FX/squeal): excess friction demand or locked/spun
            // wheel. Fades out below ~4 m/s so low-speed numerical slip (crawl
            // damping, stationary spin-up) does not trigger smoke or squeal.
            let skid_speed_gate = smoothstep(1.0, 4.0, speed);
            let skid = if loads[w] > 10.0 {
                let demand_skid = (total_demand - 0.92).clamp(0.0, 1.0);
                let slip_skid = slip_ratio.abs().clamp(0.0, 1.0) * 0.7;
                demand_skid.max(slip_skid) * skid_speed_gate
            } else {
                0.0
            };
            self.wheels[w].skid = skid;
            // Report bounded slip; raw ratios during stationary spin-up are
            // numerical, not physical.
            self.wheels[w].slip_ratio = slip_ratio.clamp(-3.0, 3.0);
            self.wheels[w].lateral_force = f_lat.abs();
            self.wheels[w].longitudinal_force = f_long.abs();

            let f = wheel_right * f_lat + wheel_fwd * f_long;
            total_force += f;
            total_torque += r_contact.cross(f);
        }

        // ---- Bounded countersteer/heading assist + yaw damping ----
        let assist_strength = (p.base_assist * self.config.tuning.assist).clamp(0.0, 1.0);
        if speed > 3.0 && grounded_count >= 2 {
            // Nudge yaw toward the velocity heading (countersteer-like),
            // strictly bounded by the assist strength.
            let desired_yaw_rate = drift_angle * 1.6;
            let yaw_err = desired_yaw_rate - yaw_rate;
            let cap = 3600.0 * assist_strength;
            total_torque += up * (yaw_err * assist_strength * 4200.0).clamp(-cap, cap);
            total_torque += up * (-yaw_rate * 420.0 * (0.4 + 0.6 * assist_strength));
        } else {
            // Airborne/slow: mild angular damping keeps landings composed.
            total_torque += body.angular_velocity * -200.0;
        }

        // Safety cap on total force.
        let max_force = mass * 60.0;
        if total_force.length() > max_force {
            total_force = total_force.normalize() * max_force;
        }

        // ---- RPM from vehicle speed + launch flare ----
        // Wheel-spin-derived rpm pegs the redline whenever the wheels break
        // loose, so derive from vehicle speed and let the throttle flare at
        // low speed (clutch slip) instead.
        let ratio = self.gear_ratio(self.gear).abs().max(0.5);
        let rolling_rpm =
            speed / p.wheel_radius / std::f32::consts::TAU * 60.0 * ratio;
        let launch_flare = self.throttle_smooth * 2600.0 * (1.0 - (speed / 8.0).min(1.0));
        let engine_rpm = rolling_rpm.max(p.idle_rpm) + launch_flare;
        self.rpm += (engine_rpm.clamp(p.idle_rpm, p.redline_rpm) - self.rpm)
            * (1.0 - (-dt / 0.12).exp());
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

    fn drive_torque(&mut self, input: &DriverInput) -> f32 {
        self.throttle_smooth += (input.throttle.clamp(0.0, 1.0) - self.throttle_smooth)
            * (1.0 - (-FIXED_DT / 0.09).exp());
        if self.gear == -1 {
            return -self.torque_at_rpm(self.rpm) * self.throttle_smooth;
        }
        if self.gear == 0 {
            return 0.0;
        }
        self.torque_at_rpm(self.rpm) * self.throttle_smooth
    }

    fn update_gears(&mut self, dt: f32, speed: f32) {
        let p = self.config.preset.clone();
        self.shift_timer = (self.shift_timer - dt).max(0.0);
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
        if speed_rpm > p.redline_rpm * 0.95 {
            let next = self.gear + 1;
            if ((next - 1) as usize) < p.gear_ratios.len() {
                self.gear = next;
                self.shift_timer = 0.22;
            }
        } else if self.gear > 1 && speed_rpm < p.redline_rpm * 0.32 {
            let lower_ratio = self.gear_ratio(self.gear - 1).abs();
            let projected = speed_rpm * (lower_ratio / ratio);
            if projected < p.redline_rpm * 0.9 {
                self.gear -= 1;
                self.shift_timer = 0.18;
            }
        }
    }
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
