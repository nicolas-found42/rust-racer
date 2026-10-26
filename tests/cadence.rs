//! Frame-rate independence: the same fixed-tick input sequence must produce
//! comparable vehicle behavior under different render cadences.
//!
//! Uses a headless Bevy app with a manual time-update strategy so each
//! `app.update()` advances the 120 Hz fixed clock by an exact multiple of the
//! tick, keeping total tick counts identical across cadences.

use avian3d::prelude::*;
use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;
use rust_racer as game;

const TICK_NANOS: u64 = (1_000_000_000.0 / 120.0) as u64; // 8_333_333 ns
const TICK: f64 = TICK_NANOS as f64 / 1_000_000_000.0;

fn headless_app(frame: std::time::Duration) -> (App, Entity) {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins((
            bevy::asset::AssetPlugin::default(),
            bevy::mesh::MeshPlugin,
            bevy::scene::ScenePlugin,
            PhysicsPlugins::default().set(PhysicsInterpolationPlugin::interpolate_all()),
            bevy::state::app::StatesPlugin,
        ))
        .insert_resource(Time::<Fixed>::from_hz(1.0 / TICK))
        .insert_resource(Gravity(Vec3::NEG_Y * 9.81))
        .insert_resource(TimeUpdateStrategy::ManualDuration(frame))
        .init_state::<game::AppState>()
        .init_resource::<game::ScoreRuntime>()
        .init_resource::<game::ImpactSignal>()
        .init_resource::<game::UiSoundSignal>()
        .init_resource::<game::StepControl>()
        .init_resource::<game::PhysicsTiming>()
        .init_resource::<game::SelectedCar>()
        .init_resource::<game::ControlRequests>()
        .init_resource::<game::fx::FxClearRequest>()
        .init_resource::<game::CameraState>()
        .insert_resource(game::input::DriverInputRes::default())
        .insert_resource(game::GameSettings {
            settings: core::persistence::Settings::default(),
            dirty: false,
            controls_shown: true,
            tuning_modified: Default::default(),
            tuning: Default::default(),
        })
        .insert_resource(game::GameMode::default())
        .init_resource::<game::vehicle::VehicleSimRes>()
        .init_resource::<game::VehicleRuntime>()
        .init_resource::<game::vehicle::ResetRequest>()
        .add_plugins(game::vehicle::VehiclePlugin);
    // Plugin::finish hooks (avian diagnostics registration) run here, as
    // `App::run` would.
    app.finish();
    // Flat ground and a car with the balanced preset geometry.
    app.world_mut().spawn((
        RigidBody::Static,
        Collider::cuboid(400.0, 2.0, 400.0),
        Position(Vec3::new(0.0, -1.0, 0.0)),
    ));
    let preset = core::tuning::CarId::FairladyZ.preset();
    let (hx, hy, hz) = (
        preset.half_extents[0],
        preset.half_extents[1],
        preset.half_extents[2],
    );
    let density = preset.mass_kg / (8.0 * hx * hy * hz);
    let car = app
        .world_mut()
        .spawn((
            RigidBody::Dynamic,
            // Mirror the game: COM pinned at the body origin, collision box
            // as an offset child so it clears the ground.
            CenterOfMass(Vec3::ZERO),
            Position(Vec3::new(0.0, preset.com_height + 0.2, 0.0)),
            Rotation::default(),
            LinearVelocity::ZERO,
            AngularVelocity::ZERO,
            ConstantForce(Vec3::ZERO),
            ConstantTorque(Vec3::ZERO),
            SleepingDisabled,
            game::PlayerCar,
        ))
        .id();
    let collider = app
        .world_mut()
        .spawn((
            game::assets::CarCollider,
            Collider::cuboid(hx * 2.0, hy * 2.0, hz * 2.0),
            ColliderDensity(density),
            Transform::from_xyz(0.0, preset.collider_offset_y(), 0.0),
        ))
        .id();
    app.world_mut().entity_mut(car).add_child(collider);
    if std::env::var("NO_VEHICLE").is_ok() {
        app.world_mut().despawn(car);
        let bare = app
            .world_mut()
            .spawn((
                RigidBody::Dynamic,
                Collider::cuboid(1.7, 1.3, 4.2),
                ColliderDensity(1480.0 / (1.7 * 1.3 * 4.2)),
                Position(Vec3::new(0.0, 2.0, 0.0)),
                Rotation::default(),
                LinearVelocity::ZERO,
                AngularVelocity::ZERO,
                ConstantForce(Vec3::new(2000.0, 0.0, 0.0)),
                SleepingDisabled,
            ))
            .id();
        return (app, bare);
    }
    (app, car)
}

fn make_app(frame: std::time::Duration) -> (App, Entity) {
    headless_app(frame)
}

fn drive(total_ticks: u32, ticks_per_update: u32) -> (Vec3, Vec3, f32) {
    // Exact nanosecond multiples so every update runs exactly
    // `ticks_per_update` fixed ticks with no truncation remainder.
    let frame = std::time::Duration::from_nanos(TICK_NANOS * ticks_per_update as u64);
    let (mut app, car) = headless_app(frame);
    let updates = (total_ticks / ticks_per_update) as usize;
    let mut peak_speed = 0.0_f32;
    // Bevy's first update applies the manual delta in `First` after the
    // fixed-main decision, so it runs zero ticks; warm up once so every
    // cadence executes the identical tick sequence.
    app.update();
    for i in 0..updates {
        // Input script in global ticks so the sequence is identical for any
        // batching (frame-quantized seconds would flip at different ticks).
        let tick = (i as u32) * ticks_per_update;
        let input = &mut app
            .world_mut()
            .resource_mut::<game::input::DriverInputRes>()
            .input;
        input.throttle = if (120..540).contains(&tick) { 1.0 } else { 0.0 };
        input.brake = 0.0;
        input.steer = if tick >= 240 { 0.85 } else { 0.0 };
        input.handbrake = (360..432).contains(&tick);
        app.update();
        let v = app
            .world()
            .get::<LinearVelocity>(car)
            .unwrap()
            .0
            .length();
        peak_speed = peak_speed.max(v);
    }
    let pos = app.world().get::<Position>(car).unwrap().0;
    let vel = app.world().get::<LinearVelocity>(car).unwrap().0;
    let el = app.world().resource::<Time>().elapsed_secs_f64();
    println!(
        "drive(tpu={ticks_per_update}) updates={updates} elapsed={el:.6} peak={peak_speed:.2}"
    );
    (pos, vel, peak_speed)
}

#[test]
fn same_tick_sequence_same_behavior_across_cadences() {
    const TICKS: u32 = 720; // 6 simulated seconds
    let (p1, v1, peak) = drive(TICKS, 1); // 120 fps rendering
    let (p2, v2, _) = drive(TICKS, 2); // 60 fps rendering
    let (p6, v6, _) = drive(TICKS, 6); // 20 fps rendering

    for (name, a, b) in [
        ("pos 60fps", p1, p2),
        ("vel 60fps", v1, v2),
        ("pos 20fps", p1, p6),
        ("vel 20fps", v1, v6),
    ] {
        let d = a.distance(b);
        assert!(
            d < 1e-3,
            "{name} diverged: {a:?} vs {b:?} (distance {d})"
        );
    }
    // Sanity: the car actually reaches real speed under throttle.
    assert!(
        peak > 7.0,
        "expected real speed under throttle, peak = {peak}"
    );
}
