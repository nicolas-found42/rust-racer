//! Game-layer persistence bridge: owns the settings store, applies settings
//! changes, and tracks per-car tuning modification flags for run legitimacy.

use crate::game_state::GameSettings;
use bevy::prelude::*;
use core::persistence::{Settings, SettingsStore};
use core::tuning::CarId;
use std::path::Path;

/// Tracks which cars have altered live tuning (marks runs as practice).
#[derive(Resource, Debug, Clone, Default)]
pub struct TuningModified {
    flags: [bool; 3],
}

impl TuningModified {
    pub fn set(&mut self, car: CarId, modified: bool) {
        self.flags[car_index(car)] = modified;
    }

    pub fn modified(&self, car: CarId) -> bool {
        self.flags[car_index(car)]
    }

    pub fn reset(&mut self, car: CarId) {
        self.flags[car_index(car)] = false;
    }
}

fn car_index(car: CarId) -> usize {
    match car {
        CarId::Rx7 => 0,
        CarId::FairladyZ => 1,
        CarId::Skyline => 2,
    }
}

pub struct PersistencePlugin;

impl Plugin for PersistencePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PreStartup, load_settings)
            .add_systems(Update, (autosave, save_on_exit));
    }
}

fn store_path() -> std::path::PathBuf {
    // Project-local save directory keeps the build self-contained.
    std::env::current_dir()
        .unwrap_or_else(|_| Path::new(".").to_path_buf())
        .join("saved")
}

/// Resource wrapper (the core type must stay Bevy-free).
#[derive(Resource)]
pub struct StoreResource(pub SettingsStore);

fn load_settings(mut commands: Commands) {
    let store = SettingsStore::new(&store_path());
    let settings = store.load();
    info!(
        "Settings loaded ({:?}); {} bests on file",
        store.path().display(),
        settings.bests.len()
    );
    commands.insert_resource(StoreResource(store));
    commands.insert_resource(GameSettings {
        settings,
        dirty: false,
        controls_shown: false,
        tuning_modified: Default::default(),
        tuning: Default::default(),
    });
}

/// Save at most every 3 seconds when dirty.
fn autosave(
    store: Res<StoreResource>,
    mut settings: ResMut<GameSettings>,
    mut timer: Local<f32>,
    time: Res<Time>,
) {
    if !settings.dirty {
        return;
    }
    *timer += time.delta_secs();
    if *timer >= 3.0 {
        *timer = 0.0;
        save(&store.0, &settings);
        settings.dirty = false;
    }
}

fn save_on_exit(
    store: Res<StoreResource>,
    settings: Res<GameSettings>,
    reader: MessageReader<bevy::app::AppExit>,
) {
    if reader.is_empty() {
        return;
    }
    if settings.dirty {
        save(&store.0, &settings);
    }
}

fn save(store: &SettingsStore, game: &GameSettings) {
    let settings: &Settings = &game.settings;
    match store.save(settings) {
        Ok(()) => debug!("settings saved"),
        Err(e) => warn!("failed to save settings: {e}"),
    }
}
