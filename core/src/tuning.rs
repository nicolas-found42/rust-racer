//! Handling presets and live-tunable parameters. Tuning is data, kept apart
//! from the simulation implementation. Values are game-specific creations
//! (arcade-accessible drift tuning) and deliberately do not claim to reproduce
//! the real vehicles' performance.

use serde::{Deserialize, Serialize};

/// Three selectable cars, each mapped to a real catalog model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CarId {
    /// Forgiving beginner: recovered 1995 Mazda RX-7 (FD) model.
    Rx7,
    /// Balanced drift car: recovered 2004 Nissan Fairlady Z (Z33) model.
    FairladyZ,
    /// Powerful expert: recovered 1999 Nissan Skyline GT-R (R34) model.
    Skyline,
}

pub const ALL_CARS: [CarId; 3] = [CarId::Rx7, CarId::FairladyZ, CarId::Skyline];

impl CarId {
    pub fn model_asset(self) -> &'static str {
        match self {
            CarId::Rx7 => "cars/rx7_1995.glb",
            CarId::FairladyZ => "cars/fairladyz_350z_2004.glb",
            CarId::Skyline => "cars/skyline_r34_1999.glb",
        }
    }

    /// Real catalog identity from the asset source (dealership catalog.json).
    pub fn catalog_name(self) -> &'static str {
        match self {
            CarId::Rx7 => "1995 RX7",
            CarId::FairladyZ => "vp_350z_04 (2004 Nissan Fairlady Z / 350Z)",
            CarId::Skyline => "1999 SKYLINE GT-R V-SPEC",
        }
    }

    pub fn source_game(self) -> &'static str {
        match self {
            CarId::Rx7 => "Midnight Club: Los Angeles",
            CarId::FairladyZ => "Midnight Club 3: DUB Edition Remix",
            CarId::Skyline => "Midnight Club: Los Angeles",
        }
    }

    pub fn short_name(self) -> &'static str {
        match self {
            CarId::Rx7 => "RX-7 FD",
            CarId::FairladyZ => "350Z",
            CarId::Skyline => "R34 GT-R",
        }
    }

    pub fn difficulty(self) -> &'static str {
        match self {
            CarId::Rx7 => "BEGINNER",
            CarId::FairladyZ => "BALANCED",
            CarId::Skyline => "EXPERT",
        }
    }

    pub fn preset(self) -> CarPreset {
        match self {
            CarId::Rx7 => CarPreset::beginner(),
            CarId::FairladyZ => CarPreset::balanced(),
            CarId::Skyline => CarPreset::expert(),
        }
    }

    pub fn paint_color(self) -> [f32; 3] {
        match self {
            CarId::Rx7 => [0.82, 0.16, 0.12],
            CarId::FairladyZ => [0.95, 0.62, 0.10],
            CarId::Skyline => [0.55, 0.62, 0.90],
        }
    }
}

/// Static handling preset per car. Units: SI (meters, seconds, kilograms,
/// newtons, radians). Torque values are game-specific engine torque numbers,
/// not measured engine data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CarPreset {
    pub mass_kg: f32,
    /// Chassis half extents (x width, y height, z length) for the collision box.
    pub half_extents: [f32; 3],
    /// Center of mass height above ground contact plane (m).
    pub com_height: f32,
    /// Wheel contact positions in chassis space (x right, y up at rest, z forward+).
    pub wheel_positions: [[f32; 3]; 4],
    pub wheel_radius: f32,
    /// Peak drive torque at the rear axle total (N·m, game-tuned axle-level
    /// value; gear ratios affect only the RPM simulation, not this force).
    pub engine_torque: f32,
    /// Engine idle / redline RPM for sound and gearing feel.
    pub idle_rpm: f32,
    pub redline_rpm: f32,
    /// Forward gear ratios (auto box). Reverse ratio separate.
    pub gear_ratios: Vec<f32>,
    pub final_drive: f32,
    pub reverse_ratio: f32,
    /// Peak lateral friction coefficient on asphalt.
    pub tire_grip_front: f32,
    pub tire_grip_rear: f32,
    /// Load sensitivity exponent: grip scales as (load/ref_load)^-exponent.
    pub load_sensitivity: f32,
    pub suspension_rest_length: f32,
    pub suspension_stiffness: f32,
    pub suspension_damping: f32,
    /// Anti-roll bar contribution (N per radian of relative travel).
    pub anti_roll: f32,
    pub max_steer_deg: f32,
    /// Steering authority retained at high speed (fraction of max_steer).
    pub min_steer_fraction: f32,
    /// Speed (m/s) at which steering reduction saturates.
    pub steer_reduction_speed: f32,
    /// Bounded countersteer/heading assist strength (0..1 of tuning range).
    pub base_assist: f32,
    /// Front brake force N. Rear brake force is front * rear_brake_bias.
    pub brake_force: f32,
    pub rear_brake_bias: f32,
    /// Handbrake: fraction of rear grip removed at full pull.
    pub handbrake_grip_cut: f32,
    /// Handbrake rear brake force N.
    pub handbrake_force: f32,
    /// Grip recovery time constant after handbrake release (s).
    pub handbrake_recover: f32,
    /// Aerodynamic drag coefficient: F = drag * v * |v|.
    pub drag: f32,
    /// Rolling resistance N per m/s.
    pub rolling_resistance: f32,
    pub downforce: f32,
}

impl CarPreset {
    /// Vertical offset from the body origin (the center of mass) to the
    /// chassis collision box center, so the box clears the ground (its bottom
    /// sits ~12 cm up) while the center of mass stays at driving height.
    pub fn collider_offset_y(&self) -> f32 {
        0.12 + self.half_extents[1] - self.com_height
    }

    pub fn beginner() -> Self {
        CarPreset {
            mass_kg: 1250.0,
            half_extents: [0.92, 0.60, 2.05],
            com_height: 0.52,
            wheel_positions: [
                [-0.75, 0.046, -1.22],
                [0.75, 0.046, -1.22],
                [-0.75, 0.046, 1.22],
                [0.75, 0.046, 1.22],
            ],
            wheel_radius: 0.31,
            engine_torque: 2200.0,
            idle_rpm: 850.0,
            redline_rpm: 8000.0,
            gear_ratios: vec![3.2, 2.1, 1.5, 1.15, 0.95],
            final_drive: 3.9,
            reverse_ratio: 3.2,
            tire_grip_front: 1.05,
            tire_grip_rear: 1.02,
            load_sensitivity: 0.10,
            suspension_rest_length: 0.32,
            suspension_stiffness: 45000.0,
            suspension_damping: 4500.0,
            anti_roll: 16000.0,
            max_steer_deg: 38.0,
            min_steer_fraction: 0.32,
            steer_reduction_speed: 42.0,
            base_assist: 0.30,
            brake_force: 11000.0,
            rear_brake_bias: 0.42,
            handbrake_grip_cut: 0.50,
            handbrake_force: 5200.0,
            handbrake_recover: 0.55,
            drag: 0.42,
            rolling_resistance: 11.0,
            downforce: 1.2,
        }
    }

    pub fn balanced() -> Self {
        CarPreset {
            mass_kg: 1480.0,
            half_extents: [0.95, 0.65, 2.15],
            com_height: 0.50,
            wheel_positions: [
                [-0.78, 0.060, -1.31],
                [0.78, 0.060, -1.31],
                [-0.78, 0.060, 1.31],
                [0.78, 0.060, 1.31],
            ],
            wheel_radius: 0.32,
            engine_torque: 3000.0,
            idle_rpm: 800.0,
            redline_rpm: 7600.0,
            gear_ratios: vec![3.4, 2.2, 1.55, 1.2, 0.98],
            final_drive: 3.7,
            reverse_ratio: 3.4,
            tire_grip_front: 1.14,
            tire_grip_rear: 1.05,
            load_sensitivity: 0.11,
            suspension_rest_length: 0.30,
            suspension_stiffness: 48000.0,
            suspension_damping: 5000.0,
            anti_roll: 21000.0,
            max_steer_deg: 37.0,
            min_steer_fraction: 0.30,
            steer_reduction_speed: 45.0,
            base_assist: 0.24,
            brake_force: 13500.0,
            rear_brake_bias: 0.40,
            handbrake_grip_cut: 0.45,
            handbrake_force: 6200.0,
            handbrake_recover: 0.5,
            drag: 0.45,
            rolling_resistance: 12.0,
            downforce: 1.6,
        }
    }

    pub fn expert() -> Self {
        CarPreset {
            mass_kg: 1560.0,
            half_extents: [0.98, 0.66, 2.28],
            com_height: 0.47,
            wheel_positions: [
                [-0.80, 0.084, -1.33],
                [0.80, 0.084, -1.33],
                [-0.80, 0.084, 1.33],
                [0.80, 0.084, 1.33],
            ],
            wheel_radius: 0.33,
            engine_torque: 4400.0,
            idle_rpm: 900.0,
            redline_rpm: 9000.0,
            gear_ratios: vec![3.5, 2.3, 1.65, 1.28, 1.02],
            final_drive: 3.55,
            reverse_ratio: 3.5,
            tire_grip_front: 1.20,
            tire_grip_rear: 1.06,
            load_sensitivity: 0.12,
            suspension_rest_length: 0.28,
            suspension_stiffness: 52000.0,
            suspension_damping: 5400.0,
            anti_roll: 26000.0,
            max_steer_deg: 36.0,
            min_steer_fraction: 0.28,
            steer_reduction_speed: 48.0,
            base_assist: 0.18,
            brake_force: 16000.0,
            rear_brake_bias: 0.38,
            handbrake_grip_cut: 0.60,
            handbrake_force: 7200.0,
            handbrake_recover: 0.45,
            drag: 0.48,
            rolling_resistance: 13.0,
            downforce: 2.2,
        }
    }
}

/// Player-adjustable live tuning, bounded. Stored per selected car; restore
/// defaults returns to `CarPreset`-derived values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LiveTuning {
    /// Steering/countersteer assist multiplier 0..2 (1 = preset default).
    pub assist: f32,
    /// Rear grip multiplier 0.5..1.5 (1 = preset default).
    pub rear_grip: f32,
    /// Engine torque multiplier 0.5..1.5 (1 = preset default).
    pub engine_torque: f32,
}

impl Default for LiveTuning {
    fn default() -> Self {
        LiveTuning {
            assist: 1.0,
            rear_grip: 1.0,
            engine_torque: 1.0,
        }
    }
}

impl LiveTuning {
    pub const ASSIST_RANGE: (f32, f32) = (0.0, 2.0);
    pub const REAR_GRIP_RANGE: (f32, f32) = (0.5, 1.5);
    pub const TORQUE_RANGE: (f32, f32) = (0.5, 1.5);

    pub fn clamp_all(&mut self) {
        self.assist = self
            .assist
            .clamp(Self::ASSIST_RANGE.0, Self::ASSIST_RANGE.1);
        self.rear_grip = self
            .rear_grip
            .clamp(Self::REAR_GRIP_RANGE.0, Self::REAR_GRIP_RANGE.1);
        self.engine_torque = self
            .engine_torque
            .clamp(Self::TORQUE_RANGE.0, Self::TORQUE_RANGE.1);
    }
}
