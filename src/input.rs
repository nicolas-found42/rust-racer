//! Input: keyboard (mandatory) + gamepad, mapped to normalized `DriverInput`
//! with smoothing, plus global control keys (pause, camera, reset, debug).

use crate::game_state::*;
use bevy::input::gamepad::{GamepadAxis, GamepadButton};
use bevy::prelude::*;
use core::sim::DriverInput;

pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DriverInputRes>()
            .init_resource::<ControlRequests>()
            .add_systems(PreUpdate, (collect_input, global_keys));
    }
}

/// Resource wrapper because DriverInput is Copy/simple.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct DriverInputRes {
    pub input: DriverInput,
}

fn collect_input(
    keys: Res<ButtonInput<KeyCode>>,
    gamepads: Query<&Gamepad>,
    mut res: ResMut<DriverInputRes>,
    mode: Res<State<AppState>>,
    time: Res<Time>,
    mut throttle_s: Local<f32>,
    mut brake_s: Local<f32>,
) {
    if *mode != AppState::Driving {
        // Keep last values so pause doesn't jerk on resume; freeze instead.
        return;
    }
    let dt = time.delta_secs();

    let mut throttle_t = 0.0_f32;
    let mut brake_t = 0.0_f32;
    let mut steer_t = 0.0_f32;
    let mut handbrake = false;

    if keys.pressed(KeyCode::KeyW) || keys.pressed(KeyCode::ArrowUp) {
        throttle_t = 1.0;
    }
    if keys.pressed(KeyCode::KeyS) || keys.pressed(KeyCode::ArrowDown) {
        brake_t = 1.0;
    }
    if keys.pressed(KeyCode::KeyA) || keys.pressed(KeyCode::ArrowLeft) {
        steer_t -= 1.0;
    }
    if keys.pressed(KeyCode::KeyD) || keys.pressed(KeyCode::ArrowRight) {
        steer_t += 1.0;
    }
    if keys.pressed(KeyCode::Space) {
        handbrake = true;
    }

    // Gamepad overrides when active: left stick X steer, RT throttle, LT
    // brake, B/circle handbrake.
    if let Ok(pad) = gamepads.single() {
        let rt: f32 = pad.get(GamepadAxis::RightZ).unwrap_or(0.0);
        let lt: f32 = pad.get(GamepadAxis::LeftZ).unwrap_or(0.0);
        let st: f32 = pad.get(GamepadAxis::LeftStickX).unwrap_or(0.0);
        if rt > 0.02_f32 {
            throttle_t = throttle_t.max(rt);
        }
        if lt > 0.02_f32 {
            brake_t = brake_t.max(lt);
        }
        if st.abs() > 0.12_f32 {
            steer_t = st.clamp(-1.0, 1.0);
        }
        if pad.pressed(GamepadButton::East) {
            handbrake = true;
        }
    }

    // Smooth digital edges into analog-ish ramps.
    let up = 4.5 * dt;
    let down = 7.0 * dt;
    *throttle_s = if throttle_t > *throttle_s {
        (*throttle_s + up).min(throttle_t)
    } else {
        (*throttle_s - down).max(throttle_t)
    };
    *brake_s = if brake_t > *brake_s {
        (*brake_s + up).min(brake_t)
    } else {
        (*brake_s - down).max(brake_t)
    };

    res.input.throttle = throttle_t.max(*throttle_s);
    res.input.brake = brake_t.max(*brake_s);
    res.input.steer = steer_t;
    res.input.handbrake = handbrake;

    // Demo harness overrides everything while enabled.
    if crate::demo::enabled() {
        res.input = crate::demo::scripted_input(time.elapsed_secs_f64());
    }
}

fn global_keys(
    keys: Res<ButtonInput<KeyCode>>,
    state: Res<State<AppState>>,
    mut requests: ResMut<ControlRequests>,
    mut camera: ResMut<CameraState>,
) {
    let driving = *state == AppState::Driving;
    let paused = *state == AppState::Paused;

    if keys.just_pressed(KeyCode::KeyR) && driving {
        requests.reset += 1;
    }
    if keys.just_pressed(KeyCode::KeyC) && driving {
        camera.mode = match camera.mode {
            CameraMode::Chase => CameraMode::Hood,
            CameraMode::Hood => CameraMode::Orbit,
            CameraMode::Orbit => CameraMode::Chase,
        };
        camera.snap_requested = true;
        requests.camera_next += 1;
    }
    // F3/F4/F5 toggles and Esc transitions live in the UI plugin (it owns
    // menu focus and the controls overlay).
    let _ = paused;
}
