//! Bevy UI layer: loading, garage, driving HUD, pause/tuning/controls overlays,
//! telemetry panel, and the results screen.
//!
//! Everything here is plain `bevy_ui`. Each screen owns exactly one root entity;
//! the root is spawned in `OnEnter(state)` and despawned in `OnExit(state)`, so
//! re-entering a state can never accumulate duplicate UI trees (despawn is
//! recursive through `Children` in Bevy 0.17).
//!
//! Integration resources are shared through `game_state`:
//! - [`MinimapTexture`] is filled by the score/course module and read here.
//! - `GameSettings::tuning` is edited by the tuning panel; the vehicle plugin
//!   applies it every frame and owns practice/legitimacy bookkeeping.
//!
//! Units: SI in the data model; UI converts to km/h, degrees, mm:ss.d, and
//! newtons/percent exactly at the display boundary.

use bevy::prelude::*;
use bevy::ui::FocusPolicy;
use core::tuning::{CarId, LiveTuning, ALL_CARS};
use std::fmt::Write as _;

use crate::game_state::{
    AppState, DebugView, GameMode, GameSettings, LoadingProgress, MinimapTexture, PhysicsTiming,
    RunResult, ScoreRuntime, SelectedCar, StepControl, UiSoundSignal, VehicleRuntime,
};

// ---------------------------------------------------------------------------
// Palette and layout constants.
// ---------------------------------------------------------------------------

/// Dark translucent panel fill, used for all read-only HUD panels.
const PANEL_BG: Color = Color::srgba(0.03, 0.04, 0.06, 0.72);
/// More opaque fill for modal panels that must stay readable over gameplay.
const PANEL_SOLID: Color = Color::srgba(0.03, 0.04, 0.06, 0.94);
/// 1px border colour for panels.
const PANEL_BORDER: Color = Color::srgba(0.85, 0.65, 0.35, 0.34);
/// Primary accent (warm amber, motorsport signage).
const ACCENT: Color = Color::srgb(0.85, 0.65, 0.35);
/// Secondary accent for drift/boost values.
const ORANGE: Color = Color::srgb(1.0, 0.55, 0.16);
const RED: Color = Color::srgb(0.95, 0.24, 0.18);
const TEXT: Color = Color::srgb(0.94, 0.95, 0.96);
const TEXT_DIM: Color = Color::srgb(0.60, 0.64, 0.68);
/// Empty portion of a bar.
const TRACK: Color = Color::srgba(0.90, 0.92, 0.95, 0.12);
/// Selected menu row.
const ROW_SELECTED: Color = Color::srgba(0.14, 0.12, 0.07, 0.96);
const BACKDROP: Color = Color::srgba(0.012, 0.016, 0.024, 0.88);

/// Minimap edge length in logical pixels.
const MINIMAP_PX: f32 = 190.0;
/// Signed drift-angle bar half-range in degrees.
const DRIFT_BAR_MAX_DEG: f32 = 65.0;
/// Pause rows 0..6 are actions; 6..10 are volume sliders.
const PAUSE_VOLUME_FIRST_ROW: usize = 6;

// ---------------------------------------------------------------------------
// Public integration resources.
// ---------------------------------------------------------------------------

/// Fonts loaded once at plugin build time.
#[derive(Resource, Debug, Clone)]
pub struct Fonts {
    pub regular: Handle<Font>,
    pub bold: Handle<Font>,
}

// ---------------------------------------------------------------------------
// Plugin.
// ---------------------------------------------------------------------------

/// Installs all screens, overlays, and the font resources.
///
/// Must be added after `DefaultPlugins` (it needs `AssetServer` at build time so
/// the very first `OnEnter(Loading)` — which runs before `Startup` — already has
/// fonts available).
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        let asset_server = app.world().resource::<AssetServer>();
        let fonts = Fonts {
            regular: asset_server.load("fonts/JetBrainsMonoNerdFont-Regular.ttf"),
            bold: asset_server.load("fonts/JetBrainsMonoNerdFont-Bold.ttf"),
        };

        app.insert_resource(fonts)
            .init_resource::<UiRoots>()
            .init_resource::<UiModal>()
            .init_resource::<PauseMenuState>()
            .init_resource::<TuningMenuState>()
            .configure_sets(
                Update,
                (UiSet::Dismiss, UiSet::Input, UiSet::Refresh).chain(),
            )
            // Screens: one root per state, always despawned on exit.
            .add_systems(OnEnter(AppState::Loading), enter_loading)
            .add_systems(OnExit(AppState::Loading), exit_loading)
            .add_systems(OnEnter(AppState::Garage), enter_garage)
            .add_systems(OnExit(AppState::Garage), exit_garage)
            .add_systems(OnEnter(AppState::Driving), enter_driving)
            .add_systems(OnExit(AppState::Driving), exit_driving)
            .add_systems(OnEnter(AppState::Paused), enter_paused)
            .add_systems(OnExit(AppState::Paused), exit_paused)
            .add_systems(OnEnter(AppState::Results), enter_results)
            .add_systems(OnExit(AppState::Results), exit_results)
            .add_systems(
                Update,
                (
                    // Overlay dismissal is evaluated before any game input so a
                    // "press any key" that closes the controls overlay cannot
                    // also pause/select on the same frame.
                    dismiss_controls_overlay.in_set(UiSet::Dismiss),
                    clear_input_consumption.in_set(UiSet::Refresh),
                    update_loading
                        .in_set(UiSet::Refresh)
                        .run_if(in_state(AppState::Loading)),
                    garage_keyboard
                        .in_set(UiSet::Input)
                        .run_if(in_state(AppState::Garage)),
                    garage_refresh
                        .in_set(UiSet::Refresh)
                        .run_if(in_state(AppState::Garage)),
                    driving_keyboard
                        .in_set(UiSet::Input)
                        .run_if(in_state(AppState::Driving)),
                    update_hud
                        .in_set(UiSet::Refresh)
                        .run_if(in_state(AppState::Driving)),
                    debug_toggle
                        .in_set(UiSet::Input)
                        .run_if(in_driving_or_paused),
                    pause_keyboard
                        .in_set(UiSet::Input)
                        .run_if(in_state(AppState::Paused)),
                    pause_hover
                        .in_set(UiSet::Input)
                        .run_if(in_state(AppState::Paused)),
                    pause_refresh
                        .in_set(UiSet::Refresh)
                        .run_if(in_state(AppState::Paused)),
                    update_tuning_display
                        .in_set(UiSet::Refresh)
                        .run_if(in_state(AppState::Paused)),
                    update_telemetry
                        .in_set(UiSet::Refresh)
                        .run_if(in_driving_or_paused),
                    results_keyboard
                        .in_set(UiSet::Input)
                        .run_if(in_state(AppState::Results)),
                    handle_ui_actions.in_set(UiSet::Input),
                    button_hover.in_set(UiSet::Refresh),
                ),
            );
    }
}

/// Update ordering inside the UI module.
///
/// Implemented by hand instead of `#[derive(SystemSet)]`: the derive macro emits
/// unqualified `core::...` paths, which resolve to this workspace's `core`
/// simulation crate rather than the standard `core` crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum UiSet {
    /// Mouse/keyboard dismissal of modal overlays; consumes the input first.
    Dismiss,
    /// Menu and gameplay-adjacent key handling.
    Input,
    /// Visual refresh and per-frame resource polling.
    Refresh,
}

impl SystemSet for UiSet {
    fn dyn_clone(&self) -> Box<dyn SystemSet> {
        Box::new(*self)
    }
}

fn in_driving_or_paused(state: Option<Res<State<AppState>>>) -> bool {
    state.is_some_and(|s| matches!(*s.get(), AppState::Driving | AppState::Paused))
}

// ---------------------------------------------------------------------------
// Private UI state.
// ---------------------------------------------------------------------------

/// Root entities, one slot per screen or overlay. `None` means "not spawned".
#[derive(Resource, Default)]
struct UiRoots {
    loading: Option<Entity>,
    garage: Option<Entity>,
    hud: Option<Entity>,
    pause: Option<Entity>,
    results: Option<Entity>,
    telemetry: Option<Entity>,
    tuning: Option<Entity>,
    controls: Option<Entity>,
}

/// Modal bookkeeping.
#[derive(Resource, Default)]
struct UiModal {
    /// Controls overlay is visible; all other UI input is suspended.
    controls: bool,
    /// Tuning panel is visible; the pause list underneath is blocked.
    tuning: bool,
    /// The controls overlay was spawned on this frame; ignore dismissal input
    /// until the next frame so the opening key/click cannot immediately close it.
    controls_suppress: bool,
    /// The overlay dismissal system consumed this frame's key/click.
    consume_key: bool,
}

#[derive(Resource, Default)]
struct PauseMenuState {
    selected: usize,
}

#[derive(Resource, Default)]
struct TuningMenuState {
    selected: usize,
}

// ---------------------------------------------------------------------------
// Markers / lightweight component payloads.
// ---------------------------------------------------------------------------

#[derive(Component)]
struct HudRoot;

#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum HudText {
    Speed,
    Gear,
    DriftAngle,
    Pending,
    Multiplier,
    Banked,
    Timer,
    BankedTotal,
}

/// Nodes the HUD mutates every frame. All UI nodes carry `BackgroundColor` and
/// `UiTransform` as required components, so one query can address every node
/// without conflicting mutable access.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum HudNode {
    RpmFill,
    DriftFill,
    TimerPanel,
    BestFlash,
    MinimapCar,
}

#[derive(Component)]
struct MinimapImage;

#[derive(Component)]
struct TelemetryRoot;

#[derive(Component, Clone, Copy)]
enum TelemetryValue {
    Speed,
    Drift,
    Yaw,
    RpmGear,
    Grip,
    Physics,
    Flags,
    Wheel(usize),
}

#[derive(Component)]
struct LoadingFill;

#[derive(Component, Clone, Copy)]
enum LoadingText {
    Status,
    ErrorDetail,
}

#[derive(Component)]
struct LoadingErrorPanel;

#[derive(Component, Clone, Copy)]
struct CarCard(usize);

#[derive(Component, Clone, Copy)]
struct CarSelectedBadge(usize);

#[derive(Component, Clone, Copy)]
struct PauseRow(usize);

#[derive(Component, Clone, Copy)]
struct VolumeValue(VolumeChannel);

#[derive(Component, Clone, Copy)]
struct TuningRow(usize);

#[derive(Component, Clone, Copy)]
struct TuningValue(usize);

#[derive(Component, Clone, Copy)]
struct TuningFill(usize);

#[derive(Component)]
struct ControlsOverlayRoot;

/// Interactions that are wired to a mouse click.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum UiAction {
    /// Pause-menu row; pressing selects and activates action rows.
    PauseRow(usize),
    Volume(VolumeChannel, i8),
    Retry,
    Garage,
    TuningReset,
    TuningClose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VolumeChannel {
    Master,
    Engine,
    Tires,
    Effects,
}

impl VolumeChannel {
    fn label(self) -> &'static str {
        match self {
            VolumeChannel::Master => "MASTER VOLUME",
            VolumeChannel::Engine => "ENGINE VOLUME",
            VolumeChannel::Tires => "TIRE VOLUME",
            VolumeChannel::Effects => "EFFECTS VOLUME",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PauseAction {
    Resume,
    Restart,
    Garage,
    Controls,
    Tuning,
    Quit,
}

// ---------------------------------------------------------------------------
// Generic spawn helpers.
// ---------------------------------------------------------------------------

fn root_node() -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: Val::Px(0.0),
        top: Val::Px(0.0),
        width: Val::Percent(100.0),
        height: Val::Percent(100.0),
        ..default()
    }
}

fn column(gap: f32) -> Node {
    Node {
        display: Display::Flex,
        flex_direction: FlexDirection::Column,
        row_gap: Val::Px(gap),
        ..default()
    }
}

fn row(gap: f32) -> Node {
    Node {
        display: Display::Flex,
        flex_direction: FlexDirection::Row,
        column_gap: Val::Px(gap),
        align_items: AlignItems::Center,
        ..default()
    }
}

fn centered_overlay() -> Node {
    Node {
        width: Val::Percent(100.0),
        height: Val::Percent(100.0),
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        ..default()
    }
}

fn panel(width: Val, padding: f32) -> Node {
    Node {
        width,
        padding: UiRect::all(Val::Px(padding)),
        border: UiRect::all(Val::Px(1.0)),
        row_gap: Val::Px(10.0),
        ..column(10.0)
    }
}

fn text_nowrap(
    value: impl Into<String>,
    font: &Handle<Font>,
    size: f32,
    color: Color,
) -> (Text, TextFont, TextColor, TextLayout) {
    (
        Text::new(value),
        TextFont {
            font: font.clone(),
            font_size: size,
            ..default()
        },
        TextColor(color),
        TextLayout::new_with_no_wrap(),
    )
}

fn text_wrap(
    value: impl Into<String>,
    font: &Handle<Font>,
    size: f32,
    color: Color,
) -> (Text, TextFont, TextColor, TextLayout) {
    (
        Text::new(value),
        TextFont {
            font: font.clone(),
            font_size: size,
            ..default()
        },
        TextColor(color),
        TextLayout::new_with_justify(Justify::Left),
    )
}

fn text_center(
    value: impl Into<String>,
    font: &Handle<Font>,
    size: f32,
    color: Color,
) -> (Text, TextFont, TextColor, TextLayout) {
    (
        Text::new(value),
        TextFont {
            font: font.clone(),
            font_size: size,
            ..default()
        },
        TextColor(color),
        TextLayout::new_with_justify(Justify::Center),
    )
}

fn span(
    value: impl Into<String>,
    font: &Handle<Font>,
    size: f32,
    color: Color,
) -> (TextSpan, TextFont, TextColor) {
    (
        TextSpan::new(value),
        TextFont {
            font: font.clone(),
            font_size: size,
            ..default()
        },
        TextColor(color),
    )
}

/// Replace text only when it actually changed, avoiding pointless relayout.
fn set_text(text: &mut Text, value: &str) {
    if text.0 != value {
        text.0.clear();
        text.0.push_str(value);
    }
}

fn play_ui_sound(sound: &mut Option<ResMut<UiSoundSignal>>) {
    if let Some(sound) = sound.as_mut() {
        sound.seq = sound.seq.wrapping_add(1);
    }
}

fn despawn_slot(commands: &mut Commands, slot: &mut Option<Entity>) {
    if let Some(entity) = slot.take() {
        commands.entity(entity).despawn();
    }
}

fn current_car(settings: Option<&GameSettings>, selected: Option<&SelectedCar>) -> CarId {
    selected
        .and_then(|s| s.car)
        .or_else(|| settings.map(|s| s.settings.selected_car))
        .unwrap_or(CarId::FairladyZ)
}

fn controls_shown(settings: Option<&GameSettings>) -> bool {
    settings.map_or(true, |s| s.controls_shown)
}

/// Snapshot of the live tuning multipliers. Defaults to neutral when settings
/// have not been inserted yet.
fn current_tuning(settings: Option<&GameSettings>) -> LiveTuning {
    settings.map(|s| s.tuning.clone()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Number/string formatting (no allocation in the common unchanged case).
// ---------------------------------------------------------------------------

/// Group an integer point total in threes: `12 345`.
fn push_points(buf: &mut String, value: f32) {
    let n = if value.is_finite() {
        value.max(0.0).round() as u64
    } else {
        0
    };
    if n < 1_000 {
        let _ = write!(buf, "{n}");
    } else if n < 1_000_000 {
        let _ = write!(buf, "{} {:03}", n / 1_000, n % 1_000);
    } else {
        let _ = write!(
            buf,
            "{} {:03} {:03}",
            n / 1_000_000,
            (n / 1_000) % 1_000,
            n % 1_000
        );
    }
}

/// `mm:ss.d` for a remaining run time, clamped at zero.
fn push_time(buf: &mut String, seconds: f32) {
    let tenths = (seconds.max(0.0) * 10.0).round() as u32;
    let minutes = tenths / 600;
    let secs = (tenths / 10) % 60;
    let deci = tenths % 10;
    let _ = write!(buf, "{minutes:02}:{secs:02}.{deci}");
}

fn push_gear(buf: &mut String, gear: i8) {
    match gear {
        -1 => buf.push('R'),
        0 => buf.push('N'),
        g if g > 0 => {
            let _ = write!(buf, "{g}");
        }
        _ => buf.push('R'),
    }
}

/// Cached preset scalars. `CarPreset::preset()` allocates a gear-ratio `Vec`,
/// so it is only evaluated when the selected car changes.
#[derive(Debug, Clone, Copy)]
struct PresetCache {
    car: Option<CarId>,
    front_grip: f32,
    rear_grip: f32,
    redline: f32,
}

impl Default for PresetCache {
    fn default() -> Self {
        PresetCache {
            car: None,
            front_grip: 1.0,
            rear_grip: 1.0,
            redline: 8000.0,
        }
    }
}

impl PresetCache {
    fn refresh(&mut self, car: CarId) {
        if self.car != Some(car) {
            let preset = car.preset();
            self.front_grip = preset.tire_grip_front;
            self.rear_grip = preset.tire_grip_rear;
            self.redline = preset.redline_rpm;
            self.car = Some(car);
        }
    }
}

// ---------------------------------------------------------------------------
// Loading screen.
// ---------------------------------------------------------------------------

fn enter_loading(mut commands: Commands, fonts: Res<Fonts>, mut roots: ResMut<UiRoots>) {
    despawn_slot(&mut commands, &mut roots.loading);

    let entity = commands
        .spawn((
            root_node(),
            BackgroundColor(Color::srgb(0.012, 0.016, 0.024)),
            GlobalZIndex(20),
            FocusPolicy::Pass,
        ))
        .with_children(|root| {
            root.spawn(centered_overlay()).with_children(|center| {
                center
                    .spawn((
                        Node {
                            width: Val::Px(620.0),
                            padding: UiRect::all(Val::Px(34.0)),
                            border: UiRect::all(Val::Px(1.0)),
                            align_items: AlignItems::Center,
                            row_gap: Val::Px(14.0),
                            ..column(14.0)
                        },
                        BackgroundColor(PANEL_SOLID),
                        BorderColor::all(PANEL_BORDER),
                        BorderRadius::all(Val::Px(3.0)),
                    ))
                    .with_children(|panel| {
                        panel.spawn(text_center("COASTAL DRIFT PARK", &fonts.bold, 46.0, TEXT));
                        panel.spawn(text_center(
                            "DRIFT / SCORE / REPEAT",
                            &fonts.regular,
                            13.0,
                            ACCENT,
                        ));

                        // Progress track + fill.
                        panel
                            .spawn((
                                Node {
                                    width: Val::Px(520.0),
                                    height: Val::Px(10.0),
                                    margin: UiRect::vertical(Val::Px(8.0)),
                                    ..default()
                                },
                                BackgroundColor(TRACK),
                                BorderColor::all(PANEL_BORDER),
                                BorderRadius::all(Val::Px(2.0)),
                            ))
                            .with_children(|track| {
                                track.spawn((
                                    Node {
                                        width: Val::Percent(0.0),
                                        height: Val::Percent(100.0),
                                        ..default()
                                    },
                                    BackgroundColor(ACCENT),
                                    BorderRadius::all(Val::Px(2.0)),
                                    LoadingFill,
                                ));
                            });

                        panel.spawn((
                            text_center("PREPARING ASSETS", &fonts.regular, 11.0, TEXT_DIM),
                            LoadingText::Status,
                        ));

                        // Failure block, hidden unless the loader reports one.
                        panel
                            .spawn((
                                Node {
                                    display: Display::None,
                                    width: Val::Percent(100.0),
                                    padding: UiRect::all(Val::Px(12.0)),
                                    border: UiRect::all(Val::Px(1.0)),
                                    row_gap: Val::Px(5.0),
                                    ..column(5.0)
                                },
                                BackgroundColor(Color::srgba(0.10, 0.02, 0.02, 0.88)),
                                BorderColor::all(Color::srgba(0.95, 0.24, 0.18, 0.55)),
                                BorderRadius::all(Val::Px(2.0)),
                                LoadingErrorPanel,
                            ))
                            .with_children(|error| {
                                error.spawn(text_center(
                                    "ASSET LOAD FAILED",
                                    &fonts.bold,
                                    12.0,
                                    RED,
                                ));
                                error.spawn((
                                    text_center("", &fonts.regular, 11.0, RED),
                                    LoadingText::ErrorDetail,
                                ));
                                error.spawn(text_center(
                                    "THE GAME CANNOT START",
                                    &fonts.regular,
                                    10.0,
                                    TEXT_DIM,
                                ));
                            });
                    });
            });
        })
        .id();

    roots.loading = Some(entity);
}

fn exit_loading(mut commands: Commands, mut roots: ResMut<UiRoots>) {
    despawn_slot(&mut commands, &mut roots.loading);
}

fn update_loading(
    progress: Option<Res<LoadingProgress>>,
    mut fill: Query<&mut Node, With<LoadingFill>>,
    mut texts: Query<(&LoadingText, &mut Text, &mut TextColor)>,
    mut error_panel: Query<&mut Node, (With<LoadingErrorPanel>, Without<LoadingFill>)>,
) {
    let (total, loaded, message, failed) = match progress.as_deref() {
        Some(p) => (p.total, p.loaded, p.message.as_str(), p.failed.as_deref()),
        None => (0, 0, "PREPARING ASSETS", None),
    };

    let fraction = if total == 0 {
        0.0
    } else {
        (loaded as f32 / total as f32).clamp(0.0, 1.0)
    };
    if let Ok(mut node) = fill.single_mut() {
        node.width = Val::Percent(fraction * 100.0);
    }

    for (kind, mut text, mut color) in texts.iter_mut() {
        match kind {
            LoadingText::Status => {
                let mut buf = String::new();
                if total == 0 {
                    let _ = write!(
                        buf,
                        "{}",
                        if message.is_empty() {
                            "PREPARING ASSETS"
                        } else {
                            message
                        }
                    );
                } else {
                    let _ = write!(buf, "{loaded:>3} / {total:<3}  {}", message);
                }
                set_text(&mut text, &buf);
                let wanted = if failed.is_some() { RED } else { TEXT_DIM };
                if color.0 != wanted {
                    color.0 = wanted;
                }
            }
            LoadingText::ErrorDetail => {
                if let Some(reason) = failed {
                    set_text(&mut text, reason);
                    if color.0 != RED {
                        color.0 = RED;
                    }
                }
            }
        }
    }

    if let Ok(mut node) = error_panel.single_mut() {
        node.display = if failed.is_some() {
            Display::Flex
        } else {
            Display::None
        };
    }
}

// ---------------------------------------------------------------------------
// Garage.
// ---------------------------------------------------------------------------

fn enter_garage(
    mut commands: Commands,
    fonts: Res<Fonts>,
    settings: Option<Res<GameSettings>>,
    selected: Option<Res<SelectedCar>>,
    mut roots: ResMut<UiRoots>,
    mut modal: ResMut<UiModal>,
) {
    despawn_slot(&mut commands, &mut roots.garage);
    despawn_slot(&mut commands, &mut roots.tuning);
    modal.controls = false;
    modal.tuning = false;

    let car = current_car(settings.as_deref(), selected.as_deref());
    let entity = spawn_garage(&mut commands, &fonts, car);
    roots.garage = Some(entity);

    if !controls_shown(settings.as_deref()) {
        open_controls(&mut commands, &fonts, &mut roots, &mut modal);
    }
}

fn exit_garage(mut commands: Commands, mut roots: ResMut<UiRoots>, mut modal: ResMut<UiModal>) {
    despawn_slot(&mut commands, &mut roots.garage);
    despawn_slot(&mut commands, &mut roots.controls);
    modal.controls = false;
}

fn spawn_garage(commands: &mut Commands, fonts: &Fonts, selected: CarId) -> Entity {
    let selected_index = ALL_CARS.iter().position(|c| *c == selected).unwrap_or(1);

    commands
        .spawn((
            root_node(),
            BackgroundColor(Color::srgb(0.016, 0.020, 0.028)),
            GlobalZIndex(20),
            FocusPolicy::Pass,
        ))
        .with_children(|root| {
            root.spawn((Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(22.0),
                padding: UiRect::all(Val::Px(28.0)),
                ..default()
            },))
                .with_children(|page| {
                    page.spawn((Node {
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        row_gap: Val::Px(4.0),
                        ..default()
                    },))
                        .with_children(|title| {
                            title.spawn(text_center("GARAGE", &fonts.bold, 40.0, TEXT));
                            title.spawn(text_center(
                                "SELECT YOUR CAR",
                                &fonts.regular,
                                12.0,
                                ACCENT,
                            ));
                        });

                    page.spawn((Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Stretch,
                        column_gap: Val::Px(22.0),
                        ..default()
                    },))
                        .with_children(|cards| {
                            for (index, car) in ALL_CARS.iter().copied().enumerate() {
                                spawn_car_card(cards, fonts, car, index, index == selected_index);
                            }
                        });

                    page.spawn(text_center(
                        "A / D  or  LEFT / RIGHT   SELECT      ENTER   DRIVE",
                        &fonts.regular,
                        12.0,
                        TEXT_DIM,
                    ));
                });
        })
        .id()
}

fn spawn_car_card(
    parent: &mut ChildSpawnerCommands<'_>,
    fonts: &Fonts,
    car: CarId,
    index: usize,
    selected: bool,
) {
    let bg = if selected {
        Color::srgba(0.075, 0.080, 0.105, 0.96)
    } else {
        PANEL_SOLID
    };
    let border = if selected { ACCENT } else { PANEL_BORDER };

    parent
        .spawn((
            Node {
                width: Val::Px(296.0),
                height: Val::Px(424.0),
                padding: UiRect::all(Val::Px(18.0)),
                border: UiRect::all(Val::Px(1.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                ..default()
            },
            BackgroundColor(bg),
            BorderColor::all(border),
            BorderRadius::all(Val::Px(3.0)),
            CarCard(index),
        ))
        .with_children(|card| {
            card.spawn(text_nowrap(car.short_name(), &fonts.bold, 28.0, TEXT));
            card.spawn(text_nowrap(car.difficulty(), &fonts.regular, 11.0, ACCENT));
            card.spawn((
                text_wrap(car.catalog_name(), &fonts.regular, 12.0, TEXT_DIM),
                Node {
                    width: Val::Percent(100.0),
                    min_height: Val::Px(34.0),
                    ..default()
                },
            ));
            card.spawn(text_nowrap(
                car.source_game(),
                &fonts.regular,
                10.0,
                TEXT_DIM,
            ));

            card.spawn((
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Px(1.0),
                    margin: UiRect::vertical(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(PANEL_BORDER),
            ));
            card.spawn(text_nowrap("STATS", &fonts.regular, 10.0, TEXT_DIM));

            let preset = car.preset();
            let average_grip = (preset.tire_grip_front + preset.tire_grip_rear) * 0.5;
            let stats = [
                (
                    "POWER",
                    normalize(preset.engine_torque, 2200.0, 4400.0),
                    ACCENT,
                ),
                (
                    "ACCEL",
                    1.0 - normalize(preset.mass_kg, 1200.0, 1600.0),
                    ACCENT,
                ),
                ("GRIP", normalize(average_grip, 1.0, 1.25), ACCENT),
                ("DRIFT", preset.handbrake_grip_cut.clamp(0.0, 1.0), ORANGE),
            ];
            for (label, value, color) in stats {
                spawn_stat_row(card, fonts, label, value, color);
            }

            // Push the badge to the bottom of the card.
            card.spawn(Node {
                flex_grow: 1.0,
                ..default()
            });
            card.spawn((
                Node {
                    display: if selected {
                        Display::Flex
                    } else {
                        Display::None
                    },
                    ..row(0.0)
                },
                CarSelectedBadge(index),
            ))
            .with_children(|badge| {
                badge.spawn(text_nowrap("SELECTED", &fonts.bold, 10.0, ORANGE));
            });
        });
}

fn spawn_stat_row(
    parent: &mut ChildSpawnerCommands<'_>,
    fonts: &Fonts,
    label: &'static str,
    value: f32,
    color: Color,
) {
    let value = value.clamp(0.0, 1.0);
    parent
        .spawn(Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(3.0),
            ..default()
        })
        .with_children(|stat| {
            stat.spawn((Node {
                width: Val::Percent(100.0),
                justify_content: JustifyContent::SpaceBetween,
                ..row(0.0)
            },))
                .with_children(|line| {
                    line.spawn(text_nowrap(label, &fonts.regular, 10.0, TEXT_DIM));
                    line.spawn(text_nowrap(
                        format!("{:.0}", value * 100.0),
                        &fonts.regular,
                        10.0,
                        TEXT_DIM,
                    ));
                });
            stat.spawn((
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Px(4.0),
                    ..default()
                },
                BackgroundColor(TRACK),
                BorderRadius::all(Val::Px(1.0)),
            ))
            .with_children(|track| {
                track.spawn((
                    Node {
                        width: Val::Percent(value * 100.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    BackgroundColor(color),
                    BorderRadius::all(Val::Px(1.0)),
                ));
            });
        });
}

fn normalize(value: f32, min: f32, max: f32) -> f32 {
    if (max - min).abs() < f32::EPSILON {
        0.0
    } else {
        ((value - min) / (max - min)).clamp(0.0, 1.0)
    }
}

fn garage_keyboard(
    keys: Res<ButtonInput<KeyCode>>,
    mut settings: Option<ResMut<GameSettings>>,
    mut selected: Option<ResMut<SelectedCar>>,
    mut mode: Option<ResMut<GameMode>>,
    mut next: ResMut<NextState<AppState>>,
    mut sound: Option<ResMut<UiSoundSignal>>,
    modal: Res<UiModal>,
) {
    if modal.controls || modal.consume_key {
        return;
    }

    let car_before = current_car(settings.as_deref(), selected.as_deref());
    let mut car = car_before;
    let index = ALL_CARS.iter().position(|c| *c == car).unwrap_or(1);

    if keys.any_just_pressed([KeyCode::KeyA, KeyCode::ArrowLeft]) {
        car = ALL_CARS[(index + ALL_CARS.len() - 1) % ALL_CARS.len()];
    }
    if keys.any_just_pressed([KeyCode::KeyD, KeyCode::ArrowRight]) {
        car = ALL_CARS[(index + 1) % ALL_CARS.len()];
    }

    if car != car_before {
        if let Some(settings) = settings.as_mut() {
            settings.settings.selected_car = car;
            settings.dirty = true;
        }
        if let Some(selected) = selected.as_mut() {
            selected.car = Some(car);
        }
        play_ui_sound(&mut sound);
    }

    if keys.any_just_pressed([KeyCode::Enter, KeyCode::NumpadEnter]) {
        if let Some(selected) = selected.as_mut() {
            selected.car = Some(car);
        }
        if let Some(settings) = settings.as_mut() {
            settings.settings.selected_car = car;
            settings.dirty = true;
        }
        if let Some(mode) = mode.as_mut() {
            mode.timed = false; // Enter: free practice
        }
        play_ui_sound(&mut sound);
        next.set(AppState::Driving);
    }

    // T: enter a timed run with this car.
    if keys.just_pressed(KeyCode::KeyT) {
        if let Some(selected) = selected.as_mut() {
            selected.car = Some(car);
        }
        if let Some(settings) = settings.as_mut() {
            settings.settings.selected_car = car;
            settings.dirty = true;
        }
        if let Some(mode) = mode.as_mut() {
            mode.timed = true;
        }
        play_ui_sound(&mut sound);
        next.set(AppState::Driving);
    }
}

fn garage_refresh(
    settings: Option<Res<GameSettings>>,
    selected: Option<Res<SelectedCar>>,
    mut cards: Query<(&CarCard, &mut BackgroundColor, &mut BorderColor)>,
    mut badges: Query<(&CarSelectedBadge, &mut Node)>,
) {
    let car = current_car(settings.as_deref(), selected.as_deref());
    let selected_index = ALL_CARS.iter().position(|c| *c == car).unwrap_or(1);

    for (card, mut bg, mut border) in cards.iter_mut() {
        let is_selected = card.0 == selected_index;
        let wanted_bg = if is_selected {
            Color::srgba(0.075, 0.080, 0.105, 0.96)
        } else {
            PANEL_SOLID
        };
        let wanted_border = if is_selected { ACCENT } else { PANEL_BORDER };
        if bg.0 != wanted_bg {
            bg.0 = wanted_bg;
        }
        if border.top != wanted_border {
            *border = BorderColor::all(wanted_border);
        }
    }

    for (badge, mut node) in badges.iter_mut() {
        let wanted = if badge.0 == selected_index {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != wanted {
            node.display = wanted;
        }
    }
}

// ---------------------------------------------------------------------------
// Driving HUD.
// ---------------------------------------------------------------------------

fn enter_driving(
    mut commands: Commands,
    fonts: Res<Fonts>,
    settings: Option<Res<GameSettings>>,
    debug: Option<Res<DebugView>>,
    mut roots: ResMut<UiRoots>,
    mut modal: ResMut<UiModal>,
) {
    despawn_slot(&mut commands, &mut roots.hud);
    despawn_slot(&mut commands, &mut roots.telemetry);
    despawn_slot(&mut commands, &mut roots.tuning);
    modal.tuning = false;
    modal.controls = false;

    roots.hud = Some(spawn_hud(&mut commands, &fonts));
    let show_telemetry = debug.map(|d| d.enabled).unwrap_or(false);
    roots.telemetry = Some(spawn_telemetry(&mut commands, &fonts, show_telemetry));

    if !controls_shown(settings.as_deref()) {
        open_controls(&mut commands, &fonts, &mut roots, &mut modal);
    }
}

fn exit_driving(mut commands: Commands, mut roots: ResMut<UiRoots>, mut modal: ResMut<UiModal>) {
    despawn_slot(&mut commands, &mut roots.hud);
    despawn_slot(&mut commands, &mut roots.telemetry);
    despawn_slot(&mut commands, &mut roots.controls);
    modal.controls = false;
    modal.tuning = false;
}

fn spawn_hud(commands: &mut Commands, fonts: &Fonts) -> Entity {
    commands
        .spawn((root_node(), FocusPolicy::Pass, GlobalZIndex(10), HudRoot))
        .with_children(|hud| {
            // ----- Bottom centre: speed / gear / RPM -----------------------
            hud.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    bottom: Val::Px(18.0),
                    width: Val::Percent(100.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::FlexEnd,
                    ..default()
                },
                FocusPolicy::Pass,
            ))
            .with_children(|wrapper| {
                wrapper
                    .spawn((
                        Node {
                            padding: UiRect::axes(Val::Px(18.0), Val::Px(10.0)),
                            column_gap: Val::Px(16.0),
                            align_items: AlignItems::FlexEnd,
                            border: UiRect::all(Val::Px(1.0)),
                            ..row(0.0)
                        },
                        BackgroundColor(PANEL_BG),
                        BorderColor::all(PANEL_BORDER),
                        BorderRadius::all(Val::Px(2.0)),
                        FocusPolicy::Pass,
                    ))
                    .with_children(|cluster| {
                        // Speed.
                        cluster
                            .spawn(Node {
                                flex_direction: FlexDirection::Column,
                                align_items: AlignItems::FlexEnd,
                                ..default()
                            })
                            .with_children(|speed| {
                                speed
                                    .spawn((
                                        text_nowrap("0", &fonts.bold, 48.0, TEXT),
                                        HudText::Speed,
                                    ))
                                    .with_children(|unit| {
                                        unit.spawn(span(" KM/H", &fonts.regular, 12.0, TEXT_DIM));
                                    });
                            });

                        cluster.spawn((
                            Node {
                                width: Val::Px(1.0),
                                height: Val::Px(46.0),
                                ..default()
                            },
                            BackgroundColor(PANEL_BORDER),
                        ));

                        // Gear.
                        cluster
                            .spawn(Node {
                                flex_direction: FlexDirection::Column,
                                align_items: AlignItems::FlexStart,
                                row_gap: Val::Px(2.0),
                                ..default()
                            })
                            .with_children(|gear| {
                                gear.spawn(text_nowrap("GEAR", &fonts.regular, 10.0, TEXT_DIM));
                                gear.spawn((
                                    text_nowrap("N", &fonts.bold, 24.0, ACCENT),
                                    HudText::Gear,
                                ));
                            });

                        // RPM bar.
                        cluster
                            .spawn(Node {
                                width: Val::Px(220.0),
                                flex_direction: FlexDirection::Column,
                                row_gap: Val::Px(4.0),
                                ..default()
                            })
                            .with_children(|rpm| {
                                rpm.spawn(Node {
                                    width: Val::Percent(100.0),
                                    justify_content: JustifyContent::SpaceBetween,
                                    ..row(0.0)
                                })
                                .with_children(|line| {
                                    line.spawn(text_nowrap("RPM", &fonts.regular, 10.0, TEXT_DIM));
                                    line.spawn(text_nowrap(
                                        "REDLINE",
                                        &fonts.regular,
                                        9.0,
                                        TEXT_DIM,
                                    ));
                                });
                                rpm.spawn((
                                    Node {
                                        width: Val::Percent(100.0),
                                        height: Val::Px(5.0),
                                        ..default()
                                    },
                                    BackgroundColor(TRACK),
                                    BorderRadius::all(Val::Px(1.0)),
                                ))
                                .with_children(|track| {
                                    track.spawn((
                                        Node {
                                            width: Val::Percent(0.0),
                                            height: Val::Percent(100.0),
                                            ..default()
                                        },
                                        BackgroundColor(TEXT),
                                        BorderRadius::all(Val::Px(1.0)),
                                        HudNode::RpmFill,
                                    ));
                                });
                            });
                    });
            });

            // ----- Bottom right: drift + combo -----------------------------
            hud.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    right: Val::Px(16.0),
                    bottom: Val::Px(18.0),
                    width: Val::Px(252.0),
                    padding: UiRect::all(Val::Px(10.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(6.0),
                    ..default()
                },
                BackgroundColor(PANEL_BG),
                BorderColor::all(PANEL_BORDER),
                BorderRadius::all(Val::Px(2.0)),
                FocusPolicy::Pass,
            ))
            .with_children(|panel| {
                panel
                    .spawn(Node {
                        width: Val::Percent(100.0),
                        justify_content: JustifyContent::SpaceBetween,
                        align_items: AlignItems::FlexEnd,
                        ..row(0.0)
                    })
                    .with_children(|line| {
                        line.spawn(text_nowrap("DRIFT ANGLE", &fonts.regular, 10.0, TEXT_DIM));
                        line.spawn((
                            text_nowrap("+0.0", &fonts.bold, 20.0, ORANGE),
                            HudText::DriftAngle,
                        ))
                        .with_children(|degree| {
                            degree.spawn(span("°", &fonts.regular, 12.0, TEXT_DIM));
                        });
                    });

                panel
                    .spawn((
                        Node {
                            width: Val::Percent(100.0),
                            height: Val::Px(6.0),
                            ..default()
                        },
                        BackgroundColor(TRACK),
                    ))
                    .with_children(|track| {
                        track.spawn((
                            Node {
                                position_type: PositionType::Absolute,
                                left: Val::Percent(50.0),
                                top: Val::Px(0.0),
                                width: Val::Px(1.0),
                                height: Val::Percent(100.0),
                                ..default()
                            },
                            BackgroundColor(PANEL_BORDER),
                        ));
                        track.spawn((
                            Node {
                                position_type: PositionType::Absolute,
                                left: Val::Percent(50.0),
                                top: Val::Px(0.0),
                                width: Val::Percent(0.0),
                                height: Val::Percent(100.0),
                                ..default()
                            },
                            BackgroundColor(ORANGE),
                            HudNode::DriftFill,
                        ));
                    });

                spawn_hud_value_row(panel, fonts, "PENDING", "0", TEXT, HudText::Pending, 15.0);
                spawn_hud_value_row(
                    panel,
                    fonts,
                    "COMBO",
                    "x1",
                    ACCENT,
                    HudText::Multiplier,
                    15.0,
                );
                spawn_hud_value_row(panel, fonts, "BANKED", "0", TEXT, HudText::Banked, 13.0);
            });

            // ----- Top centre: timed-run timer, score, NEW BEST ------------
            hud.spawn(Node {
                position_type: PositionType::Absolute,
                top: Val::Px(12.0),
                left: Val::Px(0.0),
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(6.0),
                ..default()
            })
            .with_children(|stack| {
                stack
                    .spawn((
                        Node {
                            display: Display::None,
                            padding: UiRect::axes(Val::Px(16.0), Val::Px(7.0)),
                            column_gap: Val::Px(16.0),
                            align_items: AlignItems::Center,
                            border: UiRect::all(Val::Px(1.0)),
                            ..row(0.0)
                        },
                        BackgroundColor(PANEL_BG),
                        BorderColor::all(PANEL_BORDER),
                        BorderRadius::all(Val::Px(2.0)),
                        HudNode::TimerPanel,
                    ))
                    .with_children(|timer| {
                        timer.spawn(text_nowrap("TIME", &fonts.regular, 10.0, TEXT_DIM));
                        timer.spawn((
                            text_nowrap("02:00.0", &fonts.bold, 22.0, TEXT),
                            HudText::Timer,
                        ));
                        timer.spawn((
                            Node {
                                width: Val::Px(1.0),
                                height: Val::Px(24.0),
                                ..default()
                            },
                            BackgroundColor(PANEL_BORDER),
                        ));
                        timer.spawn(text_nowrap("BANKED", &fonts.regular, 10.0, TEXT_DIM));
                        timer.spawn((
                            text_nowrap("0", &fonts.bold, 16.0, ACCENT),
                            HudText::BankedTotal,
                        ));
                    });

                stack.spawn((
                    Node {
                        display: Display::None,
                        ..default()
                    },
                    text_nowrap("NEW BEST", &fonts.bold, 20.0, ORANGE),
                    HudNode::BestFlash,
                ));
            });

            // ----- Top left: course map ------------------------------------
            hud.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(16.0),
                    top: Val::Px(16.0),
                    width: Val::Px(MINIMAP_PX),
                    height: Val::Px(MINIMAP_PX),
                    border: UiRect::all(Val::Px(1.0)),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(PANEL_SOLID),
                BorderColor::all(PANEL_BORDER),
                BorderRadius::all(Val::Px(2.0)),
                FocusPolicy::Pass,
            ))
            .with_children(|map| {
                map.spawn((
                    Node {
                        width: Val::Percent(100.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    ImageNode::default(),
                    MinimapImage,
                ));
                map.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(MINIMAP_PX * 0.5 - 4.0),
                        top: Val::Px(MINIMAP_PX * 0.5 - 4.0),
                        width: Val::Px(8.0),
                        height: Val::Px(8.0),
                        ..default()
                    },
                    BackgroundColor(ORANGE),
                    BorderRadius::all(Val::Px(4.0)),
                    HudNode::MinimapCar,
                ));
            });

            // ----- Bottom left: contextual controls ------------------------
            hud.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(16.0),
                    bottom: Val::Px(16.0),
                    ..default()
                },
                text_nowrap(
                    "SPACE HANDBRAKE  |  C CAMERA  |  R RESET  |  F3 TELEMETRY  |  ESC PAUSE",
                    &fonts.regular,
                    11.0,
                    TEXT_DIM,
                ),
                FocusPolicy::Pass,
            ));
        })
        .id()
}

fn spawn_hud_value_row(
    parent: &mut ChildSpawnerCommands<'_>,
    fonts: &Fonts,
    label: &'static str,
    value: &'static str,
    color: Color,
    kind: HudText,
    size: f32,
) {
    parent
        .spawn(Node {
            width: Val::Percent(100.0),
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            ..row(0.0)
        })
        .with_children(|line| {
            line.spawn(text_nowrap(label, &fonts.regular, 10.0, TEXT_DIM));
            line.spawn((text_nowrap(value, &fonts.bold, size, color), kind));
        });
}

fn update_hud(
    runtime: Option<Res<VehicleRuntime>>,
    score: Option<Res<ScoreRuntime>>,
    mode: Option<Res<GameMode>>,
    settings: Option<Res<GameSettings>>,
    selected: Option<Res<SelectedCar>>,
    time: Res<Time>,
    mut texts: Query<(&HudText, &mut Text, &mut TextColor)>,
    mut nodes: Query<(&HudNode, &mut Node, &mut BackgroundColor, &mut UiTransform)>,
    mut minimap_image: Query<&mut ImageNode, With<MinimapImage>>,
    minimap: Option<Res<MinimapTexture>>,
    mut preset: Local<PresetCache>,
    mut buf: Local<String>,
) {
    let runtime_default = VehicleRuntime::default();
    let vr = runtime.as_deref().unwrap_or(&runtime_default);
    let score_default = ScoreRuntime::default();
    let sc = score.as_deref().unwrap_or(&score_default);
    let timed = mode.as_deref().is_some_and(|m| m.timed);

    let car = current_car(settings.as_deref(), selected.as_deref());
    preset.refresh(car);
    let rpm_ratio = if preset.redline > 1.0 {
        (vr.rpm / preset.redline).clamp(0.0, 1.25)
    } else {
        0.0
    };

    let blink = (time.elapsed_secs() * 4.0) % 1.0 < 0.5;
    let urgent = timed && sc.run_active && sc.run_time_left < 10.0;
    let timer_color = if urgent && blink { RED } else { TEXT };

    for (kind, mut text, mut color) in texts.iter_mut() {
        buf.clear();
        match kind {
            HudText::Speed => {
                let _ = write!(buf, "{:.0}", vr.speed_kmh.max(0.0));
            }
            HudText::Gear => push_gear(&mut buf, vr.gear),
            HudText::DriftAngle => {
                let _ = write!(buf, "{:+.1}", vr.drift_angle_deg);
            }
            HudText::Pending => push_points(&mut buf, sc.score.pending),
            HudText::Multiplier => {
                let _ = write!(buf, "x{}", sc.score.multiplier.clamp(1, 5));
            }
            HudText::Banked | HudText::BankedTotal => push_points(&mut buf, sc.score.banked),
            HudText::Timer => push_time(&mut buf, sc.run_time_left),
        }
        set_text(&mut text, &buf);
        if matches!(kind, HudText::Timer) && color.0 != timer_color {
            color.0 = timer_color;
        }
    }

    for (kind, mut node, mut bg, mut transform) in nodes.iter_mut() {
        match kind {
            HudNode::RpmFill => {
                node.width = Val::Percent(rpm_ratio.clamp(0.0, 1.0) * 100.0);
                let wanted = if rpm_ratio >= 0.92 {
                    RED
                } else if rpm_ratio >= 0.78 {
                    ORANGE
                } else {
                    TEXT
                };
                if bg.0 != wanted {
                    bg.0 = wanted;
                }
            }
            HudNode::DriftFill => {
                let magnitude = (vr.drift_angle_deg.abs() / DRIFT_BAR_MAX_DEG).clamp(0.0, 1.0);
                let width = 50.0 * magnitude;
                node.width = Val::Percent(width);
                node.left = if vr.drift_angle_deg >= 0.0 {
                    Val::Percent(50.0)
                } else {
                    Val::Percent(50.0 - width)
                };
                let wanted = if vr.drift_angle_deg.abs() >= 30.0 {
                    ORANGE
                } else {
                    ACCENT
                };
                if bg.0 != wanted {
                    bg.0 = wanted;
                }
            }
            HudNode::TimerPanel => {
                let wanted = if timed { Display::Flex } else { Display::None };
                if node.display != wanted {
                    node.display = wanted;
                }
            }
            HudNode::BestFlash => {
                let wanted = if sc.new_best && blink {
                    Display::Flex
                } else {
                    Display::None
                };
                if node.display != wanted {
                    node.display = wanted;
                }
            }
            HudNode::MinimapCar => {
                if let Some(map) = minimap.as_deref() {
                    let uv = map.car_uv.clamp(Vec2::ZERO, Vec2::ONE);
                    node.left = Val::Px(uv.x * MINIMAP_PX - 4.0);
                    node.top = Val::Px(uv.y * MINIMAP_PX - 4.0);
                    let angle = if map.car_forward.length_squared() > 1e-6 {
                        map.car_forward.y.atan2(map.car_forward.x)
                    } else {
                        0.0
                    };
                    transform.rotation = Rot2::radians(angle);
                }
            }
        }
    }

    if let Some(map) = minimap.as_deref() {
        if let Some(handle) = map.image.as_ref() {
            if let Ok(mut image) = minimap_image.single_mut() {
                if image.image != *handle {
                    image.image = handle.clone();
                }
            }
        }
    }
}

fn driving_keyboard(
    keys: Res<ButtonInput<KeyCode>>,
    mut next: ResMut<NextState<AppState>>,
    modal: Res<UiModal>,
    mut sound: Option<ResMut<UiSoundSignal>>,
) {
    if modal.controls || modal.consume_key {
        return;
    }
    if keys.just_pressed(KeyCode::Escape) {
        play_ui_sound(&mut sound);
        next.set(AppState::Paused);
    }
}

fn debug_toggle(
    keys: Res<ButtonInput<KeyCode>>,
    mut debug: Option<ResMut<DebugView>>,
    modal: Res<UiModal>,
) {
    if modal.controls || modal.tuning || modal.consume_key {
        return;
    }
    let Some(debug) = debug.as_mut() else {
        return;
    };
    if keys.just_pressed(KeyCode::F3) {
        debug.enabled = !debug.enabled;
    }
    if keys.just_pressed(KeyCode::F4) {
        debug.show_forces = !debug.show_forces;
    }
    if keys.just_pressed(KeyCode::F5) {
        debug.show_contacts = !debug.show_contacts;
    }
}

// ---------------------------------------------------------------------------
// Telemetry / under-the-hood overlay.
// ---------------------------------------------------------------------------

fn spawn_telemetry(commands: &mut Commands, fonts: &Fonts, visible: bool) -> Entity {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(16.0),
                top: Val::Px(16.0),
                width: Val::Px(432.0),
                display: if visible {
                    Display::Flex
                } else {
                    Display::None
                },
                padding: UiRect::all(Val::Px(10.0)),
                border: UiRect::all(Val::Px(1.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(3.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.02, 0.03, 0.04, 0.84)),
            BorderColor::all(PANEL_BORDER),
            BorderRadius::all(Val::Px(2.0)),
            FocusPolicy::Pass,
            GlobalZIndex(30),
            TelemetryRoot,
        ))
        .with_children(|panel| {
            panel
                .spawn(Node {
                    width: Val::Percent(100.0),
                    justify_content: JustifyContent::SpaceBetween,
                    margin: UiRect::bottom(Val::Px(3.0)),
                    ..row(0.0)
                })
                .with_children(|header| {
                    header.spawn(text_nowrap("TELEMETRY", &fonts.bold, 12.0, ACCENT));
                    header.spawn(text_nowrap(
                        "F3 CLOSE  F4 FORCES  F5 CONTACTS",
                        &fonts.regular,
                        9.0,
                        TEXT_DIM,
                    ));
                });

            spawn_telemetry_line(panel, fonts, TelemetryValue::Speed);
            spawn_telemetry_line(panel, fonts, TelemetryValue::Drift);
            spawn_telemetry_line(panel, fonts, TelemetryValue::Yaw);
            spawn_telemetry_line(panel, fonts, TelemetryValue::RpmGear);
            spawn_telemetry_line(panel, fonts, TelemetryValue::Grip);
            spawn_telemetry_line(panel, fonts, TelemetryValue::Physics);
            spawn_telemetry_line(panel, fonts, TelemetryValue::Flags);

            panel.spawn((
                text_nowrap(
                    "WHEEL LOAD N  SUSP m  SLIP R  ANGLE  SKID GND",
                    &fonts.regular,
                    9.5,
                    TEXT_DIM,
                ),
                Node {
                    margin: UiRect::top(Val::Px(4.0)),
                    ..default()
                },
            ));
            for wheel in 0..4 {
                spawn_telemetry_line(panel, fonts, TelemetryValue::Wheel(wheel));
            }
        })
        .id()
}

fn spawn_telemetry_line(
    parent: &mut ChildSpawnerCommands<'_>,
    fonts: &Fonts,
    kind: TelemetryValue,
) {
    parent.spawn((text_nowrap("", &fonts.regular, 11.0, TEXT), kind));
}

fn update_telemetry(
    runtime: Option<Res<VehicleRuntime>>,
    mut timing: Option<ResMut<PhysicsTiming>>,
    debug: Option<Res<DebugView>>,
    settings: Option<Res<GameSettings>>,
    selected: Option<Res<SelectedCar>>,
    mut values: Query<(&TelemetryValue, &mut Text)>,
    mut root: Query<&mut Node, With<TelemetryRoot>>,
    mut preset: Local<PresetCache>,
    mut ticks: Local<u64>,
    mut buf: Local<String>,
) {
    let enabled = debug.as_deref().is_some_and(|d| d.enabled);
    if let Ok(mut node) = root.single_mut() {
        let wanted = if enabled {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != wanted {
            node.display = wanted;
        }
    }
    if !enabled {
        return;
    }

    let runtime_default = VehicleRuntime::default();
    let vr = runtime.as_deref().unwrap_or(&runtime_default);
    let mut timing_default = PhysicsTiming::default();
    let timing = timing.as_deref_mut().unwrap_or(&mut timing_default);
    let live = current_tuning(settings.as_deref());

    let car = current_car(settings.as_deref(), selected.as_deref());
    preset.refresh(car);

    *ticks = (*ticks).saturating_add(timing.ticks_pending as u64);
    timing.ticks_last_frame = timing.ticks_pending;
    timing.ticks_pending = 0;

    for (kind, mut text) in values.iter_mut() {
        buf.clear();
        match kind {
            TelemetryValue::Speed => {
                let _ = write!(buf, "SPEED     {:>7.1} km/h", vr.speed_kmh.max(0.0));
            }
            TelemetryValue::Drift => {
                let _ = write!(buf, "DRIFT     {:>+7.1} deg", vr.drift_angle_deg);
            }
            TelemetryValue::Yaw => {
                let _ = write!(buf, "YAW RATE  {:>+7.2} rad/s", vr.yaw_rate);
            }
            TelemetryValue::RpmGear => {
                let _ = write!(buf, "RPM {:>5.0}   GEAR ", vr.rpm);
                push_gear(&mut buf, vr.gear);
            }
            TelemetryValue::Grip => {
                let rear = preset.rear_grip * live.rear_grip;
                let _ = write!(
                    buf,
                    "GRIP  F {:.2}  R {:.2}   ASSIST {:.2}",
                    preset.front_grip, rear, live.assist
                );
            }
            TelemetryValue::Physics => {
                let _ = write!(
                    buf,
                    "PHYS {:.2} ms   TICKS/FRAME {}   TOTAL {}",
                    timing.controller_ms, timing.ticks_last_frame, *ticks
                );
            }
            TelemetryValue::Flags => {
                let forces = debug.as_deref().is_some_and(|d| d.show_forces);
                let contacts = debug.as_deref().is_some_and(|d| d.show_contacts);
                let _ = write!(
                    buf,
                    "FORCES {}   CONTACTS {}",
                    if forces { "ON" } else { "OFF" },
                    if contacts { "ON" } else { "OFF" }
                );
            }
            TelemetryValue::Wheel(index) => {
                let wheel = &vr.wheels[*index];
                let name = ["FL", "FR", "RL", "RR"][*index];
                let _ = write!(
                    buf,
                    "{name:<3} {:>7.0} {:>7.3} {:>7.3} {:>6.1} {:>5.2} {:>3}",
                    wheel.load,
                    wheel.suspension_length,
                    wheel.slip_ratio,
                    wheel.slip_angle.to_degrees(),
                    wheel.skid.clamp(0.0, 1.0),
                    if wheel.grounded { "Y" } else { "N" }
                );
            }
        }
        set_text(&mut text, &buf);
    }
}

// ---------------------------------------------------------------------------
// Pause menu.
// ---------------------------------------------------------------------------

fn enter_paused(
    mut commands: Commands,
    fonts: Res<Fonts>,
    mode: Option<Res<GameMode>>,
    settings: Option<Res<GameSettings>>,
    debug: Option<Res<DebugView>>,
    mut roots: ResMut<UiRoots>,
    mut modal: ResMut<UiModal>,
    mut pause_state: ResMut<PauseMenuState>,
) {
    despawn_slot(&mut commands, &mut roots.pause);
    despawn_slot(&mut commands, &mut roots.telemetry);
    modal.tuning = false;
    modal.controls = false;
    pause_state.selected = 0;

    let timed = mode.as_deref().is_some_and(|m| m.timed);
    roots.pause = Some(spawn_pause_menu(
        &mut commands,
        &fonts,
        timed,
        settings.as_deref(),
    ));
    let show_telemetry = debug.map(|d| d.enabled).unwrap_or(false);
    roots.telemetry = Some(spawn_telemetry(&mut commands, &fonts, show_telemetry));

    if !controls_shown(settings.as_deref()) {
        open_controls(&mut commands, &fonts, &mut roots, &mut modal);
    }
}

fn exit_paused(mut commands: Commands, mut roots: ResMut<UiRoots>, mut modal: ResMut<UiModal>) {
    despawn_slot(&mut commands, &mut roots.pause);
    despawn_slot(&mut commands, &mut roots.telemetry);
    despawn_slot(&mut commands, &mut roots.tuning);
    despawn_slot(&mut commands, &mut roots.controls);
    modal.tuning = false;
    modal.controls = false;
}

fn spawn_pause_menu(
    commands: &mut Commands,
    fonts: &Fonts,
    timed: bool,
    settings: Option<&GameSettings>,
) -> Entity {
    commands
        .spawn((
            root_node(),
            BackgroundColor(BACKDROP),
            GlobalZIndex(50),
            Interaction::default(),
            FocusPolicy::Block,
        ))
        .with_children(|root| {
            root.spawn(centered_overlay()).with_children(|center| {
                center
                    .spawn((
                        panel(Val::Px(500.0), 24.0),
                        BackgroundColor(PANEL_SOLID),
                        BorderColor::all(PANEL_BORDER),
                        BorderRadius::all(Val::Px(3.0)),
                    ))
                    .with_children(|menu| {
                        menu.spawn(Node {
                            width: Val::Percent(100.0),
                            justify_content: JustifyContent::SpaceBetween,
                            align_items: AlignItems::FlexEnd,
                            ..row(0.0)
                        })
                        .with_children(|title| {
                            title.spawn(text_nowrap("PAUSED", &fonts.bold, 36.0, TEXT));
                            title.spawn(text_nowrap(
                                if timed { "TIMED RUN" } else { "PRACTICE" },
                                &fonts.regular,
                                11.0,
                                ACCENT,
                            ));
                        });
                        menu.spawn(text_nowrap(
                            "COASTAL DRIFT PARK",
                            &fonts.regular,
                            11.0,
                            TEXT_DIM,
                        ));
                        menu.spawn((
                            Node {
                                width: Val::Percent(100.0),
                                height: Val::Px(1.0),
                                margin: UiRect::vertical(Val::Px(4.0)),
                                ..default()
                            },
                            BackgroundColor(PANEL_BORDER),
                        ));

                        let action_rows = [
                            ("RESUME", "ENTER / ESC"),
                            ("RESTART RUN", "R"),
                            ("GARAGE", "G"),
                            ("CONTROLS", "K"),
                            ("TUNING", "T"),
                            ("QUIT TO DESKTOP", "Q"),
                        ];
                        for (index, (label, key)) in action_rows.iter().enumerate() {
                            spawn_pause_action_row(menu, fonts, index, label, key);
                        }

                        let volumes = [
                            (
                                VolumeChannel::Master,
                                settings.map(|s| s.settings.volume_master),
                            ),
                            (
                                VolumeChannel::Engine,
                                settings.map(|s| s.settings.volume_engine),
                            ),
                            (
                                VolumeChannel::Tires,
                                settings.map(|s| s.settings.volume_tires),
                            ),
                            (
                                VolumeChannel::Effects,
                                settings.map(|s| s.settings.volume_effects),
                            ),
                        ];
                        for (offset, (channel, value)) in volumes.into_iter().enumerate() {
                            spawn_volume_row(
                                menu,
                                fonts,
                                PAUSE_VOLUME_FIRST_ROW + offset,
                                channel,
                                value.unwrap_or(0.0),
                            );
                        }

                        menu.spawn(text_nowrap(
                            "UP / DOWN SELECT   ENTER ACTIVATE",
                            &fonts.regular,
                            10.0,
                            TEXT_DIM,
                        ));
                        menu.spawn(text_nowrap(
                            "LEFT / RIGHT  or  - / =  ADJUST VOLUME",
                            &fonts.regular,
                            10.0,
                            TEXT_DIM,
                        ));
                    });
            });
        })
        .id()
}

fn spawn_pause_action_row(
    parent: &mut ChildSpawnerCommands<'_>,
    fonts: &Fonts,
    index: usize,
    label: &'static str,
    key: &'static str,
) {
    parent
        .spawn((
            Button,
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(38.0),
                padding: UiRect::horizontal(Val::Px(12.0)),
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(1.0)),
                ..row(0.0)
            },
            BackgroundColor(PANEL_BG),
            BorderColor::all(Color::NONE),
            BorderRadius::all(Val::Px(2.0)),
            PauseRow(index),
            UiAction::PauseRow(index),
        ))
        .with_children(|row| {
            row.spawn(text_nowrap(label, &fonts.bold, 14.0, TEXT));
            row.spawn(text_nowrap(key, &fonts.regular, 11.0, TEXT_DIM));
        });
}

fn spawn_volume_row(
    parent: &mut ChildSpawnerCommands<'_>,
    fonts: &Fonts,
    index: usize,
    channel: VolumeChannel,
    value: f32,
) {
    parent
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(34.0),
                padding: UiRect::horizontal(Val::Px(12.0)),
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(1.0)),
                ..row(0.0)
            },
            BackgroundColor(PANEL_BG),
            BorderColor::all(Color::NONE),
            BorderRadius::all(Val::Px(2.0)),
            PauseRow(index),
        ))
        .with_children(|entry| {
            entry.spawn(text_nowrap(channel.label(), &fonts.regular, 11.0, TEXT_DIM));
            entry
                .spawn(Node {
                    column_gap: Val::Px(6.0),
                    align_items: AlignItems::Center,
                    ..row(0.0)
                })
                .with_children(|controls| {
                    spawn_volume_button(controls, fonts, channel, -1, "-");
                    controls.spawn((
                        text_nowrap(
                            format!("{:>3}%", (value.clamp(0.0, 1.0) * 100.0).round() as u32),
                            &fonts.regular,
                            11.0,
                            TEXT,
                        ),
                        Node {
                            width: Val::Px(40.0),
                            justify_content: JustifyContent::Center,
                            ..default()
                        },
                        VolumeValue(channel),
                    ));
                    spawn_volume_button(controls, fonts, channel, 1, "+");
                });
        });
}

fn spawn_volume_button(
    parent: &mut ChildSpawnerCommands<'_>,
    fonts: &Fonts,
    channel: VolumeChannel,
    delta: i8,
    glyph: &'static str,
) {
    parent
        .spawn((
            Button,
            Node {
                width: Val::Px(22.0),
                height: Val::Px(22.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            BorderColor::all(PANEL_BORDER),
            BorderRadius::all(Val::Px(2.0)),
            UiAction::Volume(channel, delta),
        ))
        .with_children(|button| {
            button.spawn(text_nowrap(glyph, &fonts.bold, 12.0, TEXT));
        });
}

fn pause_keyboard(
    keys: Res<ButtonInput<KeyCode>>,
    mut settings: Option<ResMut<GameSettings>>,
    mut tuning_menu: ResMut<TuningMenuState>,
    mut modal: ResMut<UiModal>,
    mut roots: ResMut<UiRoots>,
    mut pause_state: ResMut<PauseMenuState>,
    mut next: ResMut<NextState<AppState>>,
    mut sound: Option<ResMut<UiSoundSignal>>,
    fonts: Res<Fonts>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
    mut resets: ResMut<crate::vehicle::ResetRequest>,
    mut step: ResMut<StepControl>,
) {
    if modal.controls || modal.consume_key {
        return;
    }

    if modal.tuning {
        if keys.any_just_pressed([KeyCode::ArrowUp, KeyCode::KeyW]) {
            tuning_menu.selected = (tuning_menu.selected + 2) % 3;
            play_ui_sound(&mut sound);
        }
        if keys.any_just_pressed([KeyCode::ArrowDown, KeyCode::KeyS]) {
            tuning_menu.selected = (tuning_menu.selected + 1) % 3;
            play_ui_sound(&mut sound);
        }

        let mut adjust = 0.0;
        if keys.any_just_pressed([KeyCode::ArrowLeft, KeyCode::Minus]) {
            adjust = -1.0;
        }
        if keys.any_just_pressed([KeyCode::ArrowRight, KeyCode::Equal]) {
            adjust = 1.0;
        }
        if adjust != 0.0 {
            if let Some(settings) = settings.as_mut() {
                adjust_tuning(&mut settings.tuning, tuning_menu.selected, adjust);
                settings.dirty = true;
            }
            play_ui_sound(&mut sound);
        }

        if keys.just_pressed(KeyCode::KeyR) {
            if let Some(settings) = settings.as_mut() {
                settings.tuning = LiveTuning::default();
                settings.dirty = true;
            }
            play_ui_sound(&mut sound);
        }

        if keys.any_just_pressed([KeyCode::Escape, KeyCode::KeyT]) {
            close_tuning(&mut commands, &mut roots, &mut modal);
            play_ui_sound(&mut sound);
        }
        return;
    }

    // Pause list.
    if keys.any_just_pressed([KeyCode::Escape, KeyCode::Enter, KeyCode::NumpadEnter]) {
        if keys.just_pressed(KeyCode::Escape) {
            play_ui_sound(&mut sound);
            next.set(AppState::Driving);
            return;
        }
        if let Some(action) = pause_action_for_row(pause_state.selected) {
            play_ui_sound(&mut sound);
            let tuning_now = current_tuning(settings.as_deref());
            let quit = run_pause_action(
                action,
                &mut next,
                &mut modal,
                &mut roots,
                &fonts,
                &mut commands,
                &tuning_now,
                &mut tuning_menu,
            );
            if action == PauseAction::Restart {
                resets.seq += 1;
                resets.full = true;
            }
            if quit {
                exit.write(AppExit::Success);
            }
        }
        return;
    }

    if keys.any_just_pressed([KeyCode::ArrowUp, KeyCode::KeyW]) {
        pause_state.selected = (pause_state.selected + 9) % 10;
        play_ui_sound(&mut sound);
    }
    if keys.any_just_pressed([KeyCode::ArrowDown, KeyCode::KeyS]) {
        pause_state.selected = (pause_state.selected + 1) % 10;
        play_ui_sound(&mut sound);
    }

    let volume_row = pause_state.selected.checked_sub(PAUSE_VOLUME_FIRST_ROW);
    if let Some(offset) = volume_row {
        if let Some(channel) = volume_channel_for_row(offset) {
            let mut delta = 0;
            if keys.any_just_pressed([KeyCode::ArrowLeft, KeyCode::Minus]) {
                delta = -1;
            }
            if keys.any_just_pressed([KeyCode::ArrowRight, KeyCode::Equal]) {
                delta = 1;
            }
            if delta != 0 {
                if let Some(settings) = settings.as_mut() {
                    adjust_volume(settings, channel, delta);
                }
                play_ui_sound(&mut sound);
            }
        }
    }

    // Direct hotkeys are always available from the pause list.
    if keys.just_pressed(KeyCode::Period) {
        // Advance exactly one fixed physics tick (debug stepping).
        step.request_step();
        play_ui_sound(&mut sound);
    } else if keys.just_pressed(KeyCode::KeyR) {
        play_ui_sound(&mut sound);
        resets.seq += 1;
        resets.full = true;
        next.set(AppState::Driving);
    } else if keys.just_pressed(KeyCode::KeyG) {
        play_ui_sound(&mut sound);
        next.set(AppState::Garage);
    } else if keys.just_pressed(KeyCode::KeyK) {
        play_ui_sound(&mut sound);
        open_controls(&mut commands, &fonts, &mut roots, &mut modal);
    } else if keys.just_pressed(KeyCode::KeyT) {
        play_ui_sound(&mut sound);
        let tuning_now = current_tuning(settings.as_deref());
        open_tuning(
            &mut commands,
            &fonts,
            &mut roots,
            &mut modal,
            &tuning_now,
            &mut tuning_menu,
        );
    } else if keys.just_pressed(KeyCode::KeyQ) {
        play_ui_sound(&mut sound);
        exit.write(AppExit::Success);
    }
}

fn pause_refresh(
    settings: Option<Res<GameSettings>>,
    pause_state: Res<PauseMenuState>,
    mut rows: Query<(&PauseRow, &mut BackgroundColor, &mut BorderColor)>,
    mut volumes: Query<(&VolumeValue, &mut Text)>,
) {
    for (row, mut bg, mut border) in rows.iter_mut() {
        let selected = row.0 == pause_state.selected;
        let wanted_bg = if selected { ROW_SELECTED } else { PANEL_BG };
        let wanted_border = if selected { ACCENT } else { Color::NONE };
        if bg.0 != wanted_bg {
            bg.0 = wanted_bg;
        }
        if border.top != wanted_border {
            *border = BorderColor::all(wanted_border);
        }
    }

    if let Some(settings) = settings.as_deref() {
        for (value, mut text) in volumes.iter_mut() {
            let level = match value.0 {
                VolumeChannel::Master => settings.settings.volume_master,
                VolumeChannel::Engine => settings.settings.volume_engine,
                VolumeChannel::Tires => settings.settings.volume_tires,
                VolumeChannel::Effects => settings.settings.volume_effects,
            };
            let wanted = format!("{:>3}%", (level.clamp(0.0, 1.0) * 100.0).round() as u32);
            set_text(&mut text, &wanted);
        }
    }
}

fn pause_hover(
    rows: Query<(&Interaction, &PauseRow), Changed<Interaction>>,
    mut pause_state: ResMut<PauseMenuState>,
    modal: Res<UiModal>,
) {
    if modal.controls || modal.tuning {
        return;
    }
    for (interaction, row) in rows.iter() {
        if *interaction == Interaction::Hovered && pause_state.selected != row.0 {
            pause_state.selected = row.0;
        }
    }
}

fn volume_channel_for_row(offset: usize) -> Option<VolumeChannel> {
    match offset {
        0 => Some(VolumeChannel::Master),
        1 => Some(VolumeChannel::Engine),
        2 => Some(VolumeChannel::Tires),
        3 => Some(VolumeChannel::Effects),
        _ => None,
    }
}

fn adjust_volume(settings: &mut GameSettings, channel: VolumeChannel, delta: i8) {
    let step = 0.05 * delta as f32;
    let value = match channel {
        VolumeChannel::Master => &mut settings.settings.volume_master,
        VolumeChannel::Engine => &mut settings.settings.volume_engine,
        VolumeChannel::Tires => &mut settings.settings.volume_tires,
        VolumeChannel::Effects => &mut settings.settings.volume_effects,
    };
    *value = (*value + step).clamp(0.0, 1.0);
    settings.dirty = true;
}

fn pause_action_for_row(index: usize) -> Option<PauseAction> {
    match index {
        0 => Some(PauseAction::Resume),
        1 => Some(PauseAction::Restart),
        2 => Some(PauseAction::Garage),
        3 => Some(PauseAction::Controls),
        4 => Some(PauseAction::Tuning),
        5 => Some(PauseAction::Quit),
        _ => None,
    }
}

fn run_pause_action(
    action: PauseAction,
    next: &mut NextState<AppState>,
    modal: &mut UiModal,
    roots: &mut UiRoots,
    fonts: &Fonts,
    commands: &mut Commands,
    tuning: &LiveTuning,
    tuning_menu: &mut TuningMenuState,
) -> bool {
    match action {
        PauseAction::Resume => {
            next.set(AppState::Driving);
            false
        }
        PauseAction::Restart => {
            // Full retry: restarts score/timer for timed runs, resets pose.
            next.set(AppState::Driving);
            true
        }
        PauseAction::Garage => {
            next.set(AppState::Garage);
            false
        }
        PauseAction::Controls => {
            open_controls(commands, fonts, roots, modal);
            false
        }
        PauseAction::Tuning => {
            open_tuning(commands, fonts, roots, modal, tuning, tuning_menu);
            false
        }
        PauseAction::Quit => true,
    }
}

// ---------------------------------------------------------------------------
// Tuning panel.
// ---------------------------------------------------------------------------

const TUNING_ROWS: [(&str, (f32, f32)); 3] = [
    ("ASSIST", LiveTuning::ASSIST_RANGE),
    ("REAR GRIP", LiveTuning::REAR_GRIP_RANGE),
    ("ENGINE TORQUE", LiveTuning::TORQUE_RANGE),
];

fn tuning_value(tuning: &LiveTuning, index: usize) -> f32 {
    match index {
        0 => tuning.assist,
        1 => tuning.rear_grip,
        _ => tuning.engine_torque,
    }
}

fn tuning_range(index: usize) -> (f32, f32) {
    TUNING_ROWS
        .get(index)
        .map(|(_, range)| *range)
        .unwrap_or((0.0, 1.0))
}

fn adjust_tuning(tuning: &mut LiveTuning, index: usize, direction: f32) {
    let (min, max) = tuning_range(index);
    let step = (max - min) / 40.0;
    let next = (tuning_value(tuning, index) + step * direction).clamp(min, max);
    match index {
        0 => tuning.assist = next,
        1 => tuning.rear_grip = next,
        _ => tuning.engine_torque = next,
    }
    tuning.clamp_all();
}

/// Tuning edits are practice-flagged by the vehicle plugin when it applies
/// `GameSettings::tuning`; the UI deliberately does not touch
/// `ScoreRuntime::is_practice` or the per-car modification flags.
fn open_tuning(
    commands: &mut Commands,
    fonts: &Fonts,
    roots: &mut UiRoots,
    modal: &mut UiModal,
    tuning: &LiveTuning,
    tuning_menu: &mut TuningMenuState,
) {
    if modal.controls {
        return;
    }
    if roots.tuning.is_none() {
        tuning_menu.selected = 0;
        roots.tuning = Some(spawn_tuning_panel(
            commands,
            fonts,
            tuning,
            tuning_menu.selected,
        ));
    }
    modal.tuning = true;
}

fn close_tuning(commands: &mut Commands, roots: &mut UiRoots, modal: &mut UiModal) {
    despawn_slot(commands, &mut roots.tuning);
    modal.tuning = false;
}

fn spawn_tuning_panel(
    commands: &mut Commands,
    fonts: &Fonts,
    tuning: &LiveTuning,
    selected: usize,
) -> Entity {
    commands
        .spawn((
            root_node(),
            BackgroundColor(Color::srgba(0.012, 0.016, 0.024, 0.62)),
            GlobalZIndex(60),
            Interaction::default(),
            FocusPolicy::Block,
        ))
        .with_children(|root| {
            root.spawn(centered_overlay()).with_children(|center| {
                center
                    .spawn((
                        panel(Val::Px(540.0), 24.0),
                        BackgroundColor(PANEL_SOLID),
                        BorderColor::all(PANEL_BORDER),
                        BorderRadius::all(Val::Px(3.0)),
                    ))
                    .with_children(|menu| {
                        menu.spawn(Node {
                            width: Val::Percent(100.0),
                            justify_content: JustifyContent::SpaceBetween,
                            align_items: AlignItems::FlexEnd,
                            ..row(0.0)
                        })
                        .with_children(|title| {
                            title.spawn(text_nowrap("LIVE TUNING", &fonts.bold, 30.0, TEXT));
                            title.spawn(text_nowrap("PRACTICE ONLY", &fonts.regular, 10.0, ORANGE));
                        });
                        menu.spawn(text_nowrap(
                            "BALANCE ASSIST, REAR GRIP AND ENGINE TORQUE",
                            &fonts.regular,
                            10.0,
                            TEXT_DIM,
                        ));

                        for (index, (label, range)) in TUNING_ROWS.iter().enumerate() {
                            let value = tuning_value(tuning, index);
                            let normalized = normalize(value, range.0, range.1);
                            let step = (range.1 - range.0) / 40.0;
                            menu.spawn((
                                Node {
                                    width: Val::Percent(100.0),
                                    padding: UiRect::all(Val::Px(8.0)),
                                    border: UiRect::all(Val::Px(1.0)),
                                    flex_direction: FlexDirection::Column,
                                    row_gap: Val::Px(4.0),
                                    ..default()
                                },
                                BackgroundColor(if index == selected {
                                    ROW_SELECTED
                                } else {
                                    PANEL_BG
                                }),
                                BorderColor::all(if index == selected {
                                    ACCENT
                                } else {
                                    Color::NONE
                                }),
                                BorderRadius::all(Val::Px(2.0)),
                                TuningRow(index),
                            ))
                            .with_children(|entry| {
                                entry
                                    .spawn(Node {
                                        width: Val::Percent(100.0),
                                        justify_content: JustifyContent::SpaceBetween,
                                        ..row(0.0)
                                    })
                                    .with_children(|line| {
                                        line.spawn(text_nowrap(*label, &fonts.bold, 13.0, TEXT));
                                        line.spawn((
                                            text_nowrap(
                                                format!("{value:.2}"),
                                                &fonts.bold,
                                                13.0,
                                                ACCENT,
                                            ),
                                            TuningValue(index),
                                        ));
                                    });
                                entry.spawn(text_nowrap(
                                    format!(
                                        "RANGE {:.2} - {:.2}   STEP {:.3}",
                                        range.0, range.1, step
                                    ),
                                    &fonts.regular,
                                    9.5,
                                    TEXT_DIM,
                                ));
                                entry
                                    .spawn((
                                        Node {
                                            width: Val::Percent(100.0),
                                            height: Val::Px(5.0),
                                            ..default()
                                        },
                                        BackgroundColor(TRACK),
                                        BorderRadius::all(Val::Px(1.0)),
                                    ))
                                    .with_children(|track| {
                                        track.spawn((
                                            Node {
                                                width: Val::Percent(normalized * 100.0),
                                                height: Val::Percent(100.0),
                                                ..default()
                                            },
                                            BackgroundColor(ACCENT),
                                            BorderRadius::all(Val::Px(1.0)),
                                            TuningFill(index),
                                        ));
                                    });
                            });
                        }

                        menu.spawn(text_wrap(
                            "RESTORING DEFAULTS DOES NOT CLEAR PRACTICE STATUS FOR THIS RUN.",
                            &fonts.regular,
                            10.0,
                            ORANGE,
                        ));

                        menu.spawn(Node {
                            width: Val::Percent(100.0),
                            column_gap: Val::Px(10.0),
                            ..row(0.0)
                        })
                        .with_children(|buttons| {
                            spawn_panel_button(
                                buttons,
                                fonts,
                                "RESTORE DEFAULTS  [R]",
                                UiAction::TuningReset,
                            );
                            spawn_panel_button(
                                buttons,
                                fonts,
                                "CLOSE  [ESC / T]",
                                UiAction::TuningClose,
                            );
                        });

                        menu.spawn(text_nowrap(
                            "UP / DOWN SELECT   LEFT / RIGHT ADJUST",
                            &fonts.regular,
                            10.0,
                            TEXT_DIM,
                        ));
                    });
            });
        })
        .id()
}

fn spawn_panel_button(
    parent: &mut ChildSpawnerCommands<'_>,
    fonts: &Fonts,
    label: &'static str,
    action: UiAction,
) {
    parent
        .spawn((
            Button,
            Node {
                flex_grow: 1.0,
                height: Val::Px(40.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            BorderColor::all(PANEL_BORDER),
            BorderRadius::all(Val::Px(2.0)),
            action,
        ))
        .with_children(|button| {
            button.spawn(text_nowrap(label, &fonts.bold, 12.0, TEXT));
        });
}

fn update_tuning_display(
    settings: Option<Res<GameSettings>>,
    tuning_menu: Res<TuningMenuState>,
    mut values: Query<(&TuningValue, &mut Text)>,
    mut fills: Query<(&TuningFill, &mut Node)>,
    mut rows: Query<(&TuningRow, &mut BackgroundColor, &mut BorderColor)>,
) {
    let live = current_tuning(settings.as_deref());
    for (value, mut text) in values.iter_mut() {
        let current = tuning_value(&live, value.0);
        let wanted = format!("{current:.2}");
        set_text(&mut text, &wanted);
    }

    for (fill, mut node) in fills.iter_mut() {
        let current = tuning_value(&live, fill.0);
        let (min, max) = tuning_range(fill.0);
        node.width = Val::Percent(normalize(current, min, max) * 100.0);
    }

    for (row, mut bg, mut border) in rows.iter_mut() {
        let selected = row.0 == tuning_menu.selected;
        let wanted_bg = if selected { ROW_SELECTED } else { PANEL_BG };
        let wanted_border = if selected { ACCENT } else { Color::NONE };
        if bg.0 != wanted_bg {
            bg.0 = wanted_bg;
        }
        if border.top != wanted_border {
            *border = BorderColor::all(wanted_border);
        }
    }
}

// ---------------------------------------------------------------------------
// Controls overlay.
// ---------------------------------------------------------------------------

fn open_controls(commands: &mut Commands, fonts: &Fonts, roots: &mut UiRoots, modal: &mut UiModal) {
    if modal.tuning {
        return;
    }
    if roots.controls.is_none() {
        roots.controls = Some(spawn_controls_overlay(commands, fonts));
    }
    modal.controls = true;
    modal.controls_suppress = true;
}

fn spawn_controls_overlay(commands: &mut Commands, fonts: &Fonts) -> Entity {
    commands
        .spawn((
            root_node(),
            BackgroundColor(Color::srgba(0.008, 0.012, 0.020, 0.82)),
            GlobalZIndex(100),
            Interaction::default(),
            FocusPolicy::Block,
            ControlsOverlayRoot,
        ))
        .with_children(|root| {
            root.spawn(centered_overlay()).with_children(|center| {
                center
                    .spawn((
                        panel(Val::Px(690.0), 26.0),
                        BackgroundColor(PANEL_SOLID),
                        BorderColor::all(PANEL_BORDER),
                        BorderRadius::all(Val::Px(3.0)),
                    ))
                    .with_children(|panel_root| {
                        panel_root
                            .spawn(Node {
                                width: Val::Percent(100.0),
                                justify_content: JustifyContent::SpaceBetween,
                                align_items: AlignItems::FlexEnd,
                                ..row(0.0)
                            })
                            .with_children(|title| {
                                title.spawn(text_nowrap("CONTROLS", &fonts.bold, 32.0, TEXT));
                                title.spawn(text_nowrap(
                                    "KEYBOARD / GAMEPAD",
                                    &fonts.regular,
                                    10.0,
                                    ACCENT,
                                ));
                            });

                        panel_root
                            .spawn(Node {
                                width: Val::Percent(100.0),
                                column_gap: Val::Px(28.0),
                                align_items: AlignItems::FlexStart,
                                ..row(0.0)
                            })
                            .with_children(|columns| {
                                columns
                                    .spawn(Node {
                                        width: Val::Percent(52.0),
                                        flex_direction: FlexDirection::Column,
                                        row_gap: Val::Px(5.0),
                                        ..default()
                                    })
                                    .with_children(|keyboard| {
                                        keyboard.spawn(text_nowrap(
                                            "KEYBOARD",
                                            &fonts.bold,
                                            11.0,
                                            ACCENT,
                                        ));
                                        controls_line(
                                            keyboard,
                                            fonts,
                                            "W / S  or  UP / DOWN",
                                            "THROTTLE / BRAKE-REVERSE",
                                        );
                                        controls_line(
                                            keyboard,
                                            fonts,
                                            "A / D  or  LEFT / RIGHT",
                                            "STEER",
                                        );
                                        controls_line(keyboard, fonts, "SPACE", "HANDBRAKE");
                                        controls_line(keyboard, fonts, "R", "RESET TO TRACK");
                                        controls_line(keyboard, fonts, "C", "CAMERA");
                                        controls_line(keyboard, fonts, "F3", "TELEMETRY");
                                        controls_line(keyboard, fonts, "T", "TUNING (IN PAUSE)");
                                        controls_line(keyboard, fonts, "ESC", "PAUSE");
                                    });

                                columns
                                    .spawn(Node {
                                        width: Val::Percent(48.0),
                                        flex_direction: FlexDirection::Column,
                                        row_gap: Val::Px(5.0),
                                        ..default()
                                    })
                                    .with_children(|gamepad| {
                                        gamepad.spawn(text_nowrap(
                                            "GAMEPAD",
                                            &fonts.bold,
                                            11.0,
                                            ACCENT,
                                        ));
                                        controls_line(gamepad, fonts, "LEFT STICK", "STEER");
                                        controls_line(gamepad, fonts, "RT", "THROTTLE");
                                        controls_line(gamepad, fonts, "LT", "BRAKE");
                                        controls_line(gamepad, fonts, "B", "HANDBRAKE");
                                    });
                            });

                        panel_root.spawn(text_center(
                            "PRESS ANY KEY OR CLICK TO CONTINUE",
                            &fonts.bold,
                            12.0,
                            ORANGE,
                        ));
                    });
            });
        })
        .id()
}

fn controls_line(
    parent: &mut ChildSpawnerCommands<'_>,
    fonts: &Fonts,
    key: &'static str,
    action: &'static str,
) {
    parent
        .spawn(Node {
            width: Val::Percent(100.0),
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            ..row(0.0)
        })
        .with_children(|line| {
            line.spawn(text_nowrap(key, &fonts.bold, 11.0, TEXT));
            line.spawn(text_nowrap(action, &fonts.regular, 10.0, TEXT_DIM));
        });
}

fn dismiss_controls_overlay(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut modal: ResMut<UiModal>,
    mut roots: ResMut<UiRoots>,
    mut settings: Option<ResMut<GameSettings>>,
    overlay: Query<&Interaction, With<ControlsOverlayRoot>>,
    mut commands: Commands,
) {
    if !modal.controls {
        return;
    }
    if modal.controls_suppress {
        modal.controls_suppress = false;
        return;
    }

    let clicked =
        mouse.just_pressed(MouseButton::Left) || overlay.iter().any(|i| *i == Interaction::Pressed);
    let keyed = keys.get_just_pressed().next().is_some();
    if !clicked && !keyed {
        return;
    }

    despawn_slot(&mut commands, &mut roots.controls);
    modal.controls = false;
    // Consume the opening/closing key so it cannot also pause, select a car, or
    // toggle the debug overlay in the same frame.
    modal.consume_key = true;

    if let Some(settings) = settings.as_mut() {
        if !settings.controls_shown {
            settings.controls_shown = true;
            settings.dirty = true;
        }
    }
}

fn clear_input_consumption(mut modal: ResMut<UiModal>) {
    if modal.consume_key {
        modal.consume_key = false;
    }
    if modal.controls_suppress {
        modal.controls_suppress = false;
    }
}

// ---------------------------------------------------------------------------
// Results screen.
// ---------------------------------------------------------------------------

fn enter_results(
    mut commands: Commands,
    fonts: Res<Fonts>,
    score: Option<Res<ScoreRuntime>>,
    settings: Option<Res<GameSettings>>,
    selected: Option<Res<SelectedCar>>,
    mut roots: ResMut<UiRoots>,
) {
    despawn_slot(&mut commands, &mut roots.results);

    let car = current_car(settings.as_deref(), selected.as_deref());
    let result = score.as_deref().and_then(|s| s.run_result.clone());
    let best = match settings.as_deref() {
        Some(settings) => settings.settings.best_for(car).map(|best| best.score),
        None => None,
    };
    roots.results = Some(spawn_results(
        &mut commands,
        &fonts,
        result.as_ref(),
        best,
        car,
    ));
}

fn exit_results(mut commands: Commands, mut roots: ResMut<UiRoots>) {
    despawn_slot(&mut commands, &mut roots.results);
}

fn spawn_results(
    commands: &mut Commands,
    fonts: &Fonts,
    result: Option<&RunResult>,
    best: Option<f32>,
    car: CarId,
) -> Entity {
    let has_result = result.is_some();
    let banked = result.map_or(0.0, |r| r.banked);
    let pending = result.map_or(0.0, |r| r.pending);
    let total = banked + pending;
    let is_practice = result.is_some_and(|r| r.is_practice);
    let new_best = result.is_some_and(|r| r.new_best);
    let practice_reason = result
        .and_then(|r| r.practice_reason.clone())
        .unwrap_or_else(|| "RUN INELIGIBLE FOR RECORDS".to_string());

    commands
        .spawn((
            root_node(),
            BackgroundColor(Color::srgba(0.010, 0.014, 0.021, 0.94)),
            GlobalZIndex(40),
            Interaction::default(),
            FocusPolicy::Block,
        ))
        .with_children(|root| {
            root.spawn(centered_overlay()).with_children(|center| {
                center
                    .spawn((
                        panel(Val::Px(560.0), 28.0),
                        BackgroundColor(PANEL_SOLID),
                        BorderColor::all(PANEL_BORDER),
                        BorderRadius::all(Val::Px(3.0)),
                    ))
                    .with_children(|card| {
                        card.spawn(Node {
                            width: Val::Percent(100.0),
                            justify_content: JustifyContent::SpaceBetween,
                            align_items: AlignItems::FlexEnd,
                            ..row(0.0)
                        })
                        .with_children(|title| {
                            title.spawn(text_nowrap("FINAL SCORE", &fonts.bold, 34.0, TEXT));
                            title.spawn(text_nowrap(
                                car.short_name(),
                                &fonts.regular,
                                11.0,
                                ACCENT,
                            ));
                        });

                        if has_result {
                            let mut score_buf = String::new();
                            push_points(&mut score_buf, total);
                            card.spawn(text_center(&score_buf, &fonts.bold, 56.0, TEXT));
                        } else {
                            card.spawn(text_center(
                                "NO RESULT RECORDED",
                                &fonts.bold,
                                24.0,
                                TEXT_DIM,
                            ));
                        }

                        let mut breakdown = String::new();
                        breakdown.push_str("BANKED ");
                        push_points(&mut breakdown, banked);
                        breakdown.push_str("     PENDING ");
                        push_points(&mut breakdown, pending);
                        card.spawn(text_center(&breakdown, &fonts.regular, 12.0, TEXT_DIM));

                        card.spawn((
                            Node {
                                width: Val::Percent(100.0),
                                height: Val::Px(1.0),
                                margin: UiRect::vertical(Val::Px(4.0)),
                                ..default()
                            },
                            BackgroundColor(PANEL_BORDER),
                        ));

                        card.spawn(Node {
                            width: Val::Percent(100.0),
                            justify_content: JustifyContent::SpaceBetween,
                            align_items: AlignItems::Center,
                            ..row(0.0)
                        })
                        .with_children(|line| {
                            line.spawn(text_nowrap(
                                "PERSONAL BEST",
                                &fonts.regular,
                                11.0,
                                TEXT_DIM,
                            ));
                            let best_text = match best {
                                Some(value) => {
                                    let mut buf = String::new();
                                    push_points(&mut buf, value);
                                    buf
                                }
                                None => "-".to_string(),
                            };
                            line.spawn(text_nowrap(best_text, &fonts.bold, 16.0, ACCENT));
                        });

                        if new_best {
                            card.spawn(text_center("NEW PERSONAL BEST", &fonts.bold, 18.0, ORANGE));
                        }

                        if is_practice {
                            card.spawn(text_center(
                                format!("PRACTICE RUN - {practice_reason}"),
                                &fonts.regular,
                                11.0,
                                ORANGE,
                            ));
                        }

                        card.spawn(Node {
                            width: Val::Percent(100.0),
                            column_gap: Val::Px(12.0),
                            margin: UiRect::top(Val::Px(8.0)),
                            ..row(0.0)
                        })
                        .with_children(|buttons| {
                            spawn_panel_button(buttons, fonts, "RETRY  [ENTER]", UiAction::Retry);
                            spawn_panel_button(buttons, fonts, "GARAGE  [G]", UiAction::Garage);
                        });
                    });
            });
        })
        .id()
}

fn results_keyboard(
    keys: Res<ButtonInput<KeyCode>>,
    mut next: ResMut<NextState<AppState>>,
    mut sound: Option<ResMut<UiSoundSignal>>,
    mut resets: ResMut<crate::vehicle::ResetRequest>,
) {
    if keys.any_just_pressed([KeyCode::Enter, KeyCode::NumpadEnter]) {
        play_ui_sound(&mut sound);
        resets.seq += 1;
        resets.full = true;
        next.set(AppState::Driving);
    } else if keys.just_pressed(KeyCode::KeyG) {
        play_ui_sound(&mut sound);
        next.set(AppState::Garage);
    }
}

// ---------------------------------------------------------------------------
// Generic mouse action handling and hover styling.
// ---------------------------------------------------------------------------

fn handle_ui_actions(
    actions: Query<(&Interaction, &UiAction), Changed<Interaction>>,
    mut next: ResMut<NextState<AppState>>,
    mut settings: Option<ResMut<GameSettings>>,
    mut pause_state: ResMut<PauseMenuState>,
    mut modal: ResMut<UiModal>,
    mut roots: ResMut<UiRoots>,
    fonts: Res<Fonts>,
    mut commands: Commands,
    mut sound: Option<ResMut<UiSoundSignal>>,
    mut exit: MessageWriter<AppExit>,
    mut tuning_menu: ResMut<TuningMenuState>,
    mut resets: ResMut<crate::vehicle::ResetRequest>,
) {
    if modal.consume_key {
        return;
    }

    for (interaction, action) in actions.iter() {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match action {
            UiAction::PauseRow(index) => {
                pause_state.selected = *index;
                if let Some(pause_action) = pause_action_for_row(*index) {
                    play_ui_sound(&mut sound);
                    let tuning_now = current_tuning(settings.as_deref());
                    let quit = run_pause_action(
                        pause_action,
                        &mut next,
                        &mut modal,
                        &mut roots,
                        &fonts,
                        &mut commands,
                        &tuning_now,
                        &mut tuning_menu,
                    );
                    if pause_action == PauseAction::Restart {
                        resets.seq += 1;
                        resets.full = true;
                    }
                    if quit {
                        exit.write(AppExit::Success);
                    }
                } else {
                    play_ui_sound(&mut sound);
                }
            }
            UiAction::Volume(channel, delta) => {
                if let Some(settings) = settings.as_mut() {
                    adjust_volume(settings, *channel, *delta);
                }
                play_ui_sound(&mut sound);
            }
            UiAction::Retry => {
                play_ui_sound(&mut sound);
                resets.seq += 1;
                resets.full = true;
                next.set(AppState::Driving);
            }
            UiAction::Garage => {
                play_ui_sound(&mut sound);
                next.set(AppState::Garage);
            }
            UiAction::TuningReset => {
                if let Some(settings) = settings.as_mut() {
                    settings.tuning = LiveTuning::default();
                    settings.dirty = true;
                }
                play_ui_sound(&mut sound);
            }
            UiAction::TuningClose => {
                close_tuning(&mut commands, &mut roots, &mut modal);
                play_ui_sound(&mut sound);
            }
        }
    }
}

fn button_hover(
    mut buttons: Query<
        (&Interaction, &mut BackgroundColor),
        (Changed<Interaction>, With<Button>, Without<PauseRow>),
    >,
) {
    for (interaction, mut bg) in buttons.iter_mut() {
        let wanted = match interaction {
            Interaction::Pressed => Color::srgba(0.20, 0.16, 0.08, 0.98),
            Interaction::Hovered => Color::srgba(0.16, 0.14, 0.09, 0.96),
            Interaction::None => PANEL_BG,
        };
        if bg.0 != wanted {
            bg.0 = wanted;
        }
    }
}
