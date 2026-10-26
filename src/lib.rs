//! rust-racer library: all game modules live here so integration tests can
//! compose the real plugin graph headlessly.

pub mod assets;
pub mod audio;
pub mod camera;
pub mod demo;
pub mod fx;
pub mod game_state;
pub mod input;
pub mod persistence;
pub mod score_game;
pub mod session;
pub mod ui;
pub mod vehicle;
pub mod world;

use avian3d::prelude::*;
use bevy::prelude::*;
use game_state::*;

// Convenience re-exports for integration tests and embedders.
pub use game_state::{
    AppState, GameMode, GameSettings, ImpactSignal, PhysicsTiming, ScoreRuntime, SelectedCar,
    StepControl, UiSoundSignal, VehicleRuntime, PlayerCar, ControlRequests, CameraState,
};

/// Build and run the game application.
pub fn run() {
    // Bevy's release builds resolve `assets/` relative to the executable;
    // prefer the working directory when it has an assets folder (cargo run),
    // falling back to the executable's directory (launched binary).
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let root = if std::path::Path::new("assets").is_dir() {
        std::env::current_dir().expect("current dir")
    } else {
        exe_dir.expect("executable dir")
    };
    std::env::set_var("BEVY_ASSET_ROOT", root);

    App::new()
        .add_plugins((
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "rust-racer — Coastal Drift Park".into(),
                        resolution: {
                            let mut res = bevy::window::WindowResolution::new(3200, 1800);
                            res.set_scale_factor_override(Some(2.0)); // 1600x900 logical on the Retina dev Mac
                            res
                        },
                        ..default()
                    }),
                    primary_cursor_options: Some(default()),
                    exit_condition: bevy::window::ExitCondition::OnPrimaryClosed,
                    close_when_requested: true,
                })
                .set(bevy::render::RenderPlugin {
                    render_creation: bevy::render::settings::RenderCreation::Automatic(
                        bevy::render::settings::WgpuSettings::default(),
                    ),
                    ..default()
                }),
            PhysicsPlugins::default().set(PhysicsInterpolationPlugin::interpolate_all()),
        ))
        // Authoritative simulation clock: exactly 120 Hz.
        .insert_resource(Time::<Fixed>::from_hz(1.0 / core::FIXED_DT as f64))
        .insert_resource(Gravity(Vec3::NEG_Y * 9.81))
        .init_state::<AppState>()
        .init_resource::<GameMode>()
        .init_resource::<CameraState>()
        .init_resource::<ScoreRuntime>()
        .init_resource::<ImpactSignal>()
        .init_resource::<UiSoundSignal>()
        .init_resource::<DebugView>()
        .init_resource::<StepControl>()
        .init_resource::<PhysicsTiming>()
        .init_resource::<SelectedCar>()
        .init_resource::<MinimapTexture>()
        .add_plugins((
            assets::CarAssetsPlugin,
            world::build::WorldPlugin,
            vehicle::VehiclePlugin,
            score_game::ScoreGamePlugin,
            session::SessionPlugin,
            camera::CameraRigPlugin,
            input::InputPlugin,
            demo::DemoPlugin,
            fx::FxPlugin,
            audio::GameAudioPlugin,
            ui::UiPlugin,
            persistence::PersistencePlugin,
        ))
        .run();
}
