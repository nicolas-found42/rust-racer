# rust-racer — Coastal Drift Park

A 3D drift driving game built with **Bevy 0.17** and **avian3d 0.4** on a
custom rear-wheel-drive tire model. Set in a coastal industrial drift park at
golden hour with three connected driving areas and three playable cars using
real recovered car models.

## Requirements

- macOS (Apple Silicon) or any desktop platform supported by wgpu
- Rust 1.85+ (2021 edition)

## Build & run

```sh
cargo run --release
```

First build takes several minutes (Bevy). The game runs fully offline; it
never touches the asset source repository.

## Controls

| Input | Action |
| --- | --- |
| `W`/`S` or `↑`/`↓` | Throttle / brake & reverse |
| `A`/`D` or `←`/`→` | Steering |
| `Space` | Handbrake |
| `R` | Reset to nearest checkpoint |
| `C` | Cycle camera (chase / hood / orbit) |
| `Esc` | Pause / back |
| `F3` | Under-the-hood telemetry overlay |
| `F4`/`F5` (telemetry open) | Toggle force / contact visualizations |
| `T` (pause) | Tuning panel; `.` steps one physics tick while paused |
| Gamepad | Left stick steer, RT throttle, LT brake, B handbrake |

Menus are keyboard-first (arrows + Enter) and mouse-clickable.

## The park

1. **Skidpad** — wide pad with painted concentric circles and figure-eight
   guides; safe spawn.
2. **Industrial circuit** — linked corners, a hairpin, barriers, warehouses,
   container stacks, curbs and marked clipping zones (teal rings + cones).
3. **Coastal loop** — sweeping circle with elevation changes, guardrails and a
   scenic overlook over the ocean.

Connectors link all three areas; color-coded chevron signs and the HUD
minimap show the way.

## Cars

| Car | Identity (real catalog model) | Source game | Tuning intent |
| --- | --- | --- | --- |
| RX-7 FD | `1995 RX7` (`MCLA_VP_MZD_RX7_95`) | Midnight Club: Los Angeles | Forgiving beginner |
| 350Z | `vp_350z_04` (2004 Fairlady Z) (`MC3_VP_350Z_04`) | Midnight Club 3: DUB Edition Remix | Balanced drift |
| R34 GT-R | `1999 SKYLINE GT-R V-SPEC` (`MCLA_VP_NSN_SKYLINE_99`) | Midnight Club: Los Angeles | Powerful expert |

Models are recovered from the named games and are the property of their
respective rights holders (see `assets/cars/manifest.json` for provenance and
hashes). **All handling values are game-specific tuning created for this
game**; they do not reproduce the original vehicles' performance or the
source games' physics.

## Architecture

- `core/` — pure, testable crate: fixed-step vehicle controller (`sim.rs`),
  telemetry-driven drift scoring (`score.rs`), presets/tuning data
  (`tuning.rs`), robust JSON persistence (`persistence.rs`). No Bevy ECS.
- `src/` — Bevy layer:
  - `vehicle.rs` — runs the controller inside avian's physics schedule
    (120 Hz), applies forces to the chassis rigid body, detects impacts.
  - `world/` — analytic heightfield + road corridors (`geo.rs`) feeding both
    the terrain mesh and its collision trimesh (`build.rs`).
  - `assets.rs` — GLB loading, scene normalization, material corrections,
    wheel-node extraction (rim/tire pairs re-parented to steer/spin pivots).
  - `score_game.rs`, `session.rs`, `camera.rs`, `input.rs`, `fx.rs`,
    `audio.rs` (fully synthesized SFX), `ui.rs`, `persistence.rs`.

Physics runs at an exact **120 Hz fixed step** with bounded catch-up and
render interpolation; the controller and scoring advance only inside the
physics schedule, so pause and single-step debugging are exact.

## Scoring (summary)

Qualifying drift: ≥ 20 km/h, drift angle 10°–65°, grounded wheels. Points per
tick: `100 × clamp(speed/60, 0, 2) × clamp((|angle|−10)/35, 0, 1) × dt`,
times a combo multiplier that rises 1×→5× every 2 s of qualifying drift.
Direction flips get a 0.75 s grace; longer gaps bank the combo. Hard impacts
(beyond ~1.5 m/s Δv), resets, and spins past 90° discard pending points.
Clipping-zone proximity adds up to 1.25×. Timed runs last 120 s; surviving
pending banks at the buzzer. Runs that used debug stepping or altered tuning
are marked practice and never become personal bests.

## Persistence

`saved/settings.json` (project-local): audio volumes, graphics quality,
selected car, and legitimate personal bests. Missing or malformed files fall
back to safe defaults; valid fields of a partially corrupt file are salvaged.

## Demo smoke run

Set `RACER_DEMO=1` to launch straight into free practice with a scripted
drive (launch, handbrake flick, sustained drift, countersteer) and no
first-launch controls overlay. `RACER_DEMO=timed` starts a 120 s scored run
instead. Useful for hands-off smoke testing; normal play ignores it.

```sh
RACER_DEMO=1 cargo run --release
```

Demo runs are muted and open behind other windows. For hands-off runs that
must not interrupt whoever is using the machine, use
`scripts/demo-quiet.sh [logfile]`: it sets `RACER_NO_ACTIVATE=1`, which the
vendored `bevy_winit` (`vendor/bevy_winit`, wired in via `[patch.crates-io]`)
turns into macOS accessory activation policy plus
`activateIgnoringOtherApps(false)`. `RACER_MUTE=1` silences audio outside the
demo, and `RACER_CAM=chase|orbit|hood` pins the demo camera.

## Asset provenance

Cars are byte-identical GLB copies from the `carmodels` recovery corpus
(see `assets/cars/manifest.json` for original paths and SHA-256 hashes).
Adjustments are import-time only (material factors, scene normalization,
wheel re-parenting); geometry and textures are untouched. Fonts are JetBrains
Mono (OFL license). All audio is synthesized at startup — no audio files.
Environment is generated programmatically.

## Known limitations

- Wheels are visual meshes driven by the controller (raycast suspension);
  there are no per-wheel collider joints, so extreme chassis pitching can
  clip wheel meshes slightly into ground geometry.
- The 350Z model ships a fused interior shadow mesh that is hidden at import;
  some showroom-only material variants (KHR_materials_variants liveries) are
  not exposed — each car uses its default factory appearance.
- Engine audio is a synthesized loop (no recordings exist for these models).
- Personal bests are per-car and validated only locally (no anti-tamper).
