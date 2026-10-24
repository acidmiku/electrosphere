//! The game's stick dead zone and the host's compensation (`[controls]
//! compensate_deadzone`, `gamepad::compensate_sticks`) on the Japanese
//! release (`--ignored`: the JP disc and a DualShock flight state).
//!
//! The state, [`state`], is mission 1 in flight with a DualShock in analog
//! mode on port 1: a cold boot of disc 1 with `--pad-type dualshock` through
//! `~/ac3-jp/runs/pad-boot-story-flight.txt`, saved at VBlank 48,961 (the
//! stick research, 2026-10-22). From it, each stick position is held for a
//! few passes and the game's own calibrated inputs are read: 80028C24 maps
//! left Y to pitch (context +0x13C) and left X to roll (+0x140), full
//! deflection 4096.
//!
//! - [`the_games_dead_zone_and_the_compensation`]: without compensation the
//!   game drops the smaller axis of a diagonal and every half deflection
//!   (the 1999 code); with it, the game reads every angle and amount within
//!   one of its steps.
//! - [`centred_sticks_change_nothing`]: with the sticks centred, a run with
//!   compensation and one without keep identical RAM.
use ac3_graphics::Vram;
use ac3_port::assets::GameData;
use ac3_port::gamepad::stick_bytes;
use ac3_port::machine_card::{CardDevice, CardSource};
use ac3_port::machine_player::{self, Host, PadType, Player};
use ac3_port::savestate::{self, GameId};
use std::fs::File;
use std::path::{Path, PathBuf};

fn home(path: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(path)
}

fn disc() -> PathBuf {
    std::env::var_os("AC3_JP_DISC").map_or_else(
        || home("ac3-jp/discs/Ace Combat 3 - Electrosphere (Japan, Asia) (Disc 1) (Rev 1).cue"),
        PathBuf::from,
    )
}

/// The DualShock flight state (see the module documentation).
fn state() -> PathBuf {
    std::env::var_os("AC3_JP_DS_FLIGHT_STATE").map_or_else(
        || home("ac3-jp/runs/present-work/ds-flight.ac3state"),
        PathBuf::from,
    )
}

fn player(data: &GameData, compensate: bool) -> Player {
    let game = machine_player::jp();
    let (images, booted) = machine_player::disc_images(game, &disc(), None).unwrap();
    let discs = images
        .iter()
        .map(|path| {
            let image = ac3_port::version::image_path(path).unwrap_or_else(|_| path.clone());
            File::open(image).unwrap()
        })
        .collect();
    let mut host = Host::with_discs(game, Vram::new(), discs, booted);
    host.card = CardDevice::from_sources(&[CardSource::Blank, CardSource::Blank])
        .unwrap()
        .with_clock(game.layout().vblank_counter);
    host.pad = PadType::DualShock.ports();
    host.compensate_deadzone = compensate;
    let mut player = Player::new(ac3_flight::Machine::default(), host);
    game.boot(&mut player.machine, &data.executable.payload);
    player.machine.take_fault().unwrap();
    load(&mut player, data, &state());
    player
}

fn load(player: &mut Player, data: &GameData, path: &Path) {
    savestate::load(player, GameId::of(&data.executable.payload), path).unwrap();
}

/// The game's calibrated pitch and roll inputs (full deflection 4096).
fn inputs(player: &mut Player) -> [i32; 2] {
    let context = player.machine.lw(machine_player::jp().layout().context);
    [0x13c, 0x140].map(|offset| player.machine.lw(context + offset) as i32)
}

/// Hold the left stick at `degrees` from the roll axis (towards pull up,
/// stick up) at `amount` of full deflection on a round gate, then read the
/// game's inputs.
fn read(player: &mut Player, degrees: f32, amount: f32) -> [i32; 2] {
    let angle = degrees.to_radians();
    let [x, y] = stick_bytes([amount * angle.cos(), amount * angle.sin()], 0.0);
    player.set_sticks([0x80, 0x80, x, y]);
    for _ in 0..6 {
        player.iteration().unwrap();
    }
    inputs(player)
}

const ANGLES: [f32; 9] = [0.0, 10.0, 22.5, 30.0, 45.0, 60.0, 67.5, 80.0, 90.0];

#[test]
#[ignore = "needs the JP disc and the DualShock flight state"]
fn the_games_dead_zone_and_the_compensation() {
    let data = GameData::load_jp_disc(&disc()).unwrap();
    let mut table = Vec::new();
    for compensate in [false, true] {
        let mut player = player(&data, compensate);
        for amount in [0.5f32, 1.0] {
            for degrees in ANGLES {
                let [pitch, roll] = read(&mut player, degrees, amount);
                // Stick up is pitch's negative side.
                let (up, right) = (-pitch as f32 / 4096.0, roll as f32 / 4096.0);
                let read = up.atan2(right).to_degrees();
                table.push(format!(
                    "compensate {compensate:5} amount {amount:.1} {degrees:5.1} deg: \
                     pitch {pitch:6} roll {roll:6} -> {read:6.1} deg"
                ));
                let angle = degrees.to_radians();
                let wanted = [amount * angle.sin(), amount * angle.cos()];
                if compensate {
                    for (got, wanted) in [(up, wanted[0]), (right, wanted[1])] {
                        assert!(
                            (got - wanted).abs() <= 1.0 / 51.0,
                            "{degrees} deg at {amount}: {got} for {wanted}"
                        );
                    }
                    assert!(
                        (read - degrees).abs() <= 1.5,
                        "{degrees} deg at {amount}: the game reads {read}"
                    );
                } else if amount < 0.6 {
                    // Half deflection is inside the game's dead zone.
                    assert_eq!([pitch, roll], [0, 0], "{degrees} deg at {amount}");
                } else if degrees == 22.5 {
                    // A full push at 22.5 degrees loses its pitch.
                    assert_eq!(pitch, 0, "{degrees} deg");
                    assert!(roll > 3000, "{roll}");
                }
            }
        }
        // Centred again: nothing.
        player.set_sticks([0x80; 4]);
        for _ in 0..6 {
            player.iteration().unwrap();
        }
        assert_eq!(inputs(&mut player), [0, 0]);
    }
    eprintln!("{}", table.join("\n"));
}

#[test]
#[ignore = "needs the JP disc and the DualShock flight state"]
fn centred_sticks_change_nothing() {
    let data = GameData::load_jp_disc(&disc()).unwrap();
    let mut plain = player(&data, false);
    let mut compensated = player(&data, true);
    for pass in 0..300 {
        plain.iteration().unwrap();
        compensated.iteration().unwrap();
        assert!(
            plain.machine.ram() == compensated.machine.ram(),
            "pass {pass}: RAM differs"
        );
    }
}
