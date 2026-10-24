//! JP overlay 2, part 3: the step handlers of the JP save screens (load,
//! save, new account, delete, format), one per step of `[0x80112DF8]`.
//!
//! The handlers poll the card status of both ports (800E267C) or of the
//! selected one (+4), run one record transfer per call, drive the screen's
//! dialogs (800DEDF0 open, 800DECA8 text, 800DEE08 set, 800DEE84 state) and
//! set the next state (+0) and step (+1). A confirm press is 0x10 (none of
//! 0x2F held) or, on some screens, 0x40; a cancel press 0x20 (none of 0x5F
//! held). Sound requests: 0x3C confirm, 0x3B cancel, 0x3D move.
use super::ov2_p3::MODULE;
use super::ov2_p3_card as card;
use super::ov2_p3_panels::accounts_e6a6c;
use super::ov2_p3_panels::{SAVE, messages, next_state_e69a8, pressed};
use super::ov2_p3_records as records;
use crate::common::{CONTEXT, loaded_module};
use crate::dispatch::call;
use crate::machine::{Machine, SmallData};

/// Other parts of overlay 2: the dialogs.
const DIALOG_TEXT: u32 = 0x800d_eca8;
const DIALOG_OPEN: u32 = 0x800d_edf0;
const DIALOG_SET: u32 = 0x800d_ee08;
const DIALOG_STATE: u32 = 0x800d_ee84;
/// The sound request (resident 8002DB4C, `script`).
const SOUND: u32 = 0x8002_db4c;
/// The scene block (its selection +0xE0).
use super::ov2_p3::SCENE;

const NONE: u32 = u32::MAX;

fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}

fn save(m: &mut Machine) -> u32 {
    m.lw(SAVE)
}

/// Step +1 += `by` (`lbu`, `addiu`, `sb`).
fn advance(m: &mut Machine, by: u32) {
    let s = save(m);
    let step = u32::from(m.lbu(s.wrapping_add(1)));
    m.sb(s.wrapping_add(1), step.wrapping_add(by) as u8);
}

/// State +0 = `state`, step +1 = 0 (`[SAVE]` read for each store).
fn go(m: &mut Machine, state: u32) {
    let s = save(m);
    m.sb(s, state as u8);
    let s = save(m);
    m.sb(s.wrapping_add(1), 0);
}

/// The screen message (`[[DRAW] + 0x5C] + 1`).
fn message(m: &mut Machine, id: u8) {
    let block = messages(m);
    m.sb(block.wrapping_add(1), id);
}

fn status(m: &mut Machine, port: u32) -> u32 {
    card::status_e267c(m, port)
}

/// The selected port's status.
fn port_status(m: &mut Machine) -> u32 {
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    status(m, port)
}

/// A card status of "no card" (2) or "not formatted" (6).
fn absent(status: u32) -> bool {
    status == 2 || status == 6
}

fn dialog(m: &mut Machine, args: [u32; 4]) {
    call(m, DIALOG_SET, args);
}

fn sound(m: &mut Machine, id: u32) {
    call(m, SOUND, [id, 0, 0, 0]);
}

/// The story choice's next state (800E69A8 of scene +0xE0).
fn story_next(m: &mut Machine) -> u32 {
    let scene = m.lw(SCENE);
    let choice = m.lw(scene.wrapping_add(0xe0));
    next_state_e69a8(m, choice)
}

/// The port marks of the message block (+2 + i, three accounts): the
/// selected account (+5) 3 with context +0x124 bit 0x10, else 0; the
/// others 1.
fn account_marks(m: &mut Machine) {
    for i in 0..3u32 {
        let s = save(m);
        if u32::from(m.lbu(s.wrapping_add(5))) != i {
            let block = messages(m);
            m.sb(block.wrapping_add(i).wrapping_add(2), 1);
        } else {
            let ctx = context(m);
            let mark = if m.lw(ctx.wrapping_add(0x124)) & 0x10 != 0 {
                3
            } else {
                0
            };
            let block = messages(m);
            m.sb(block.wrapping_add(i).wrapping_add(2), mark);
        }
    }
}

/// 800E7498: start the load screen: dialog 1 (0xC01; texts 0xB, 0, 0xA),
/// no choice, the flags, next step.
pub fn load_start_e7498(m: &mut Machine) {
    m.push_frame(0x18);
    call(m, DIALOG_OPEN, [1, 0xc01, 0, 0]);
    call(m, DIALOG_TEXT, [1, 0xb, 0, 0]);
    call(m, DIALOG_TEXT, [1, 0, 0, 0]);
    call(m, DIALOG_TEXT, [1, 0xa, 0, 0]);
    dialog(m, [1, 1, 0, 0x14]);
    let s = save(m);
    m.sb(s.wrapping_add(3), 0);
    let s = save(m);
    m.sb(s.wrapping_add(8), 1);
    let block = messages(m);
    m.sb(block.wrapping_add(8), 0);
    let block = messages(m);
    m.sb(block.wrapping_add(0x1e), 0);
    advance(m, 1);
    m.pop_frame(0x18);
}

/// 800E7554: the load screen's question (dialog 1, message 0x2B): a
/// confirm press asks the dialog to close with choice 1 (sound 0x3C), a
/// cancel press with choice 2 (0x3B); with the dialog closed (0 or 3) the
/// choice acts: 1 -> state 1 (message 0x2F), 2 -> the story's next state.
pub fn load_question_e7554(m: &mut Machine) {
    m.push_frame(0x20);
    let block = messages(m);
    m.sb(block.wrapping_add(1), 0x2b);
    let state = call(m, DIALOG_STATE, [1, 0, 0, 0]);
    if pressed(m, 0x10, 0x2f) || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            let choice = m.lbu(s.wrapping_add(3));
            if choice == 1 {
                message(m, 0x2f);
                dialog(m, [1, 0, 0, 0]);
                let s = save(m);
                m.sb(s, choice);
                let s = save(m);
                m.sb(s.wrapping_add(1), 0);
            }
        } else {
            dialog(m, [1, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 1);
            sound(m, 0x3c);
        }
    }
    if pressed(m, 0x20, 0x5f) || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 2 {
                message(m, 0);
                dialog(m, [1, 0, 0, 0]);
                let next = story_next(m);
                let s = save(m);
                m.sb(s, next as u8);
                let s = save(m);
                m.sb(s.wrapping_add(1), 0);
            }
        } else {
            dialog(m, [1, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 2);
            sound(m, 0x3b);
        }
    }
    m.pop_frame(0x20);
}

/// 800E7758: open dialog 1 (0xC01) and check both ports: no card in
/// either -> state 2; else dialog 5 and the next step.
pub fn check_ports_e7758(m: &mut Machine) {
    m.push_frame(0x18);
    let s = save(m);
    m.sb(s.wrapping_add(8), 1);
    call(m, DIALOG_OPEN, [1, 0xc01, 0, 0]);
    if status(m, 0) != 0 && status(m, 1) != 0 {
        let s = save(m);
        m.sb(s, 2);
    } else {
        dialog(m, [5, 1, 0, 0x14]);
        advance(m, 1);
    }
    m.pop_frame(0x18);
}

/// 800E77EC: wait for a card in both ports (message 0x2F meanwhile); then
/// with dialog 5 closed (0) state 2, else ask it to close (unless 3).
pub fn wait_ports_e77ec(m: &mut Machine) {
    m.push_frame(0x18);
    let state = call(m, DIALOG_STATE, [5, 0, 0, 0]);
    if status(m, 0) == 0 || status(m, 1) == 0 {
        message(m, 0x2f);
    } else if state == 0 {
        dialog(m, [5, 0, 0, 0]);
        go(m, 2);
    } else if state != 3 {
        dialog(m, [5, 3, NONE, NONE]);
    }
    m.pop_frame(0x18);
}

/// 800E78A0: both ports without a card (2 or 6): dialog 4 (message 0x2C)
/// and the next step; else state 3.
pub fn no_cards_e78a0(m: &mut Machine) {
    m.push_frame(0x18);
    let first = status(m, 0);
    let second = status(m, 1);
    if absent(first) && absent(second) {
        let s = save(m);
        let step = u32::from(m.lbu(s.wrapping_add(1)));
        m.sb(s.wrapping_add(1), step.wrapping_add(1) as u8);
        dialog(m, [4, 1, 0, 0x14]);
        message(m, 0x2c);
    } else {
        let s = save(m);
        m.sb(s, 3);
    }
    m.pop_frame(0x18);
}

/// 800E7948: the no-card question (dialog 4): a card inserted asks the
/// dialog to close (choice 3, then state 1); confirm -> the story's next
/// state, cancel -> state 0.
pub fn no_cards_question_e7948(m: &mut Machine) {
    m.push_frame(0x20);
    let state = call(m, DIALOG_STATE, [4, 0, 0, 0]);
    let first = status(m, 0);
    let second = status(m, 1);
    if !(absent(first) && absent(second)) {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 3 {
                m.sb(s, 1);
                let s = save(m);
                m.sb(s.wrapping_add(1), 0);
                m.pop_frame(0x20);
                return;
            }
        } else {
            dialog(m, [4, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 3);
        }
    }
    let mut done = false;
    if pressed(m, 0x10, 0x2f) || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 1 {
                dialog(m, [4, 0, 0, 0]);
                let next = story_next(m);
                let s = save(m);
                m.sb(s, next as u8);
                let s = save(m);
                m.sb(s.wrapping_add(1), 0);
                done = true;
            }
        } else {
            dialog(m, [4, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 1);
        }
    }
    if !done && (pressed(m, 0x20, 0x5f) || state == 0) {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 2 {
                dialog(m, [4, 0, 0, 0]);
                go(m, 0);
            }
        } else {
            dialog(m, [4, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 2);
        }
    }
    m.pop_frame(0x20);
}

/// 800E7B8C: the port choice's start: both ports without a card -> state
/// 1 (dialog 1 0xC01); else dialog 1 (0xC04, text 2) and the next step
/// (message 0x2E); a port with status 5 preselects port 1 unless port 0
/// has it.
pub fn port_start_e7b8c(m: &mut Machine) {
    m.push_frame(0x20);
    let s = save(m);
    m.sb(s.wrapping_add(8), 0);
    let s = save(m);
    m.sb(s.wrapping_add(9), 0);
    let first = status(m, 0);
    let second = status(m, 1);
    if absent(first) && absent(second) {
        call(m, DIALOG_OPEN, [1, 0xc01, 0, 0]);
        call(m, DIALOG_TEXT, [1, 0, 0, 0]);
        let s = save(m);
        m.sb(s, 1);
        let s = save(m);
        m.sb(s.wrapping_add(1), 0);
        let s = save(m);
        m.sb(s.wrapping_add(8), 1);
    } else {
        call(m, DIALOG_OPEN, [1, 0xc04, 0, 0]);
        call(m, DIALOG_TEXT, [1, 2, 0, 0]);
        dialog(m, [1, 1, 0, 0x14]);
        advance(m, 1);
        message(m, 0x2e);
    }
    if first != 5 && second == 5 {
        let s = save(m);
        m.sb(s.wrapping_add(4), 1);
    }
    let block = messages(m);
    m.sb(block.wrapping_add(0x1e), 0);
    m.pop_frame(0x20);
}

/// 800E7CDC: the port choice (dialog 1): left/right (context +0x130 bits
/// 0..1) toggle the port (+4), a port without a card is not selectable,
/// the marks show it; confirm -> state 5, cancel -> state 0; both ports
/// gone -> state 1.
pub fn port_choice_e7cdc(m: &mut Machine) {
    m.push_frame(0x28);
    let state = call(m, DIALOG_STATE, [1, 0, 0, 0]);
    let s = save(m);
    let before = m.lbu(s.wrapping_add(4));
    let first = status(m, 0);
    let second = status(m, 1);
    let ctx = context(m);
    if m.lw(ctx.wrapping_add(0x130)) & 3 != 0 {
        let s = save(m);
        let port = m.lbu(s.wrapping_add(4));
        m.sb(s.wrapping_add(4), port ^ 1);
    }
    if absent(first) {
        let s = save(m);
        m.sb(s.wrapping_add(4), 1);
    }
    if absent(second) {
        let s = save(m);
        m.sb(s.wrapping_add(4), 0);
    }
    if absent(first) && absent(second) {
        dialog(m, [1, 0, 0, 0]);
        call(m, DIALOG_OPEN, [1, 0xc01, 0, 0]);
        call(m, DIALOG_TEXT, [1, 0, 0, 0]);
        go(m, 1);
        m.pop_frame(0x28);
        return;
    }
    let mark = if absent(first) {
        2
    } else {
        let s = save(m);
        u8::from(m.lbu(s.wrapping_add(4)) != 0)
    };
    let block = messages(m);
    m.sb(block.wrapping_add(2), mark);
    let mark = if absent(second) {
        2
    } else {
        let s = save(m);
        u8::from(m.lbu(s.wrapping_add(4)) != 1)
    };
    let block = messages(m);
    m.sb(block.wrapping_add(3), mark);
    if pressed(m, 0x10, 0x2f) || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 1 {
                dialog(m, [1, 0, 0, 0]);
                let s = save(m);
                m.sb(s, 5);
                let s = save(m);
                m.sb(s.wrapping_add(1), 0);
                m.pop_frame(0x28);
                return;
            }
        } else {
            dialog(m, [1, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 1);
            sound(m, 0x3c);
        }
    }
    if pressed(m, 0x20, 0x5f) || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 2 {
                dialog(m, [1, 0, 0, 0]);
                go(m, 0);
                m.pop_frame(0x28);
                return;
            }
        } else {
            dialog(m, [1, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 2);
            sound(m, 0x3b);
        }
    }
    let ctx = context(m);
    if m.lw(ctx.wrapping_add(0x130)) & 3 != 0 {
        let s = save(m);
        if before != m.lbu(s.wrapping_add(4)) {
            sound(m, 0x3d);
        }
    }
    m.pop_frame(0x28);
}

/// 800E80B8: the selected port without a card (2 or 6): dialog 4 (message
/// 0x2C) and the next step; else state 6. Then the 0x80 record and the
/// account records reset.
pub fn port_check_e80b8(m: &mut Machine) {
    m.push_frame(0x18);
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    if absent(status(m, port)) {
        let s = save(m);
        let step = u32::from(m.lbu(s.wrapping_add(1)));
        m.sb(s.wrapping_add(1), step.wrapping_add(1) as u8);
        dialog(m, [4, 1, 0, 0x14]);
        call(m, DIALOG_OPEN, [1, 0xc01, 0, 0]);
        call(m, DIALOG_TEXT, [1, 0, 0, 0]);
        message(m, 0x2c);
    } else {
        let s = save(m);
        m.sb(s, 6);
    }
    records::reset_info_e3b5c(m);
    records::reset_slots_e3e14(m);
    m.pop_frame(0x18);
}

/// 800E8174: the selected port's no-card question (dialog 4): a card
/// missing in a port asks the dialog to close (choice 3, then state 5);
/// confirm -> the story's next state, cancel -> state 3.
pub fn port_question_e8174(m: &mut Machine) {
    m.push_frame(0x20);
    let state = call(m, DIALOG_STATE, [4, 0, 0, 0]);
    let first = status(m, 0);
    let second = status(m, 1);
    if first == 0 || second == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 3 {
                m.sb(s, 5);
                let s = save(m);
                m.sb(s.wrapping_add(1), 0);
                m.pop_frame(0x20);
                return;
            }
        } else {
            dialog(m, [4, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 3);
        }
    }
    if pressed(m, 0x10, 0x2f) || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 1 {
                dialog(m, [4, 0, 0, 0]);
                let next = story_next(m);
                let s = save(m);
                m.sb(s, next as u8);
                let s = save(m);
                m.sb(s.wrapping_add(1), 0);
            }
        } else {
            dialog(m, [4, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 1);
        }
    }
    if pressed(m, 0x20, 0x5f) || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 2 {
                dialog(m, [4, 0, 0, 0]);
                go(m, 3);
            }
        } else {
            dialog(m, [4, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 2);
        }
    }
    m.pop_frame(0x20);
}

/// 800E83AC: dialog 2 and the next step (message 0x2F).
pub fn open_dialog2_e83ac(m: &mut Machine) {
    m.push_frame(0x18);
    let s = save(m);
    let step = u32::from(m.lbu(s.wrapping_add(1)));
    m.sb(s.wrapping_add(1), step.wrapping_add(1) as u8);
    dialog(m, [2, 1, 0, 0x14]);
    message(m, 0x2f);
    m.pop_frame(0x18);
}

/// 800E8404: a selected port with any status -> state 6.
pub fn port_any_e8404(m: &mut Machine) {
    m.push_frame(0x18);
    if port_status(m) != 0 {
        go(m, 6);
    }
    m.pop_frame(0x18);
}

/// 800E8454: a selected port with status 1 opens dialog 1 (0xC01, message
/// 0x31) and the next step; else state 7.
pub fn port_ready_e8454(m: &mut Machine) {
    m.push_frame(0x18);
    if port_status(m) == 1 {
        call(m, DIALOG_OPEN, [1, 0xc01, 0, 0]);
        call(m, DIALOG_TEXT, [1, 0, 0, 0]);
        message(m, 0x31);
        advance(m, 1);
    } else {
        let s = save(m);
        m.sb(s, 7);
    }
    m.pop_frame(0x18);
}

/// 800E84E8: an unusable card in the selected port: no card -> state 1; a
/// confirm press -> the story's next state, a cancel press -> state 3.
pub fn port_error_e84e8(m: &mut Machine) {
    m.push_frame(0x18);
    let status = port_status(m);
    if absent(status) {
        go(m, 1);
    } else if status != 0 {
        if pressed(m, 0x10, 0x2f) {
            let next = story_next(m);
            let s = save(m);
            m.sb(s, next as u8);
            let s = save(m);
            m.sb(s.wrapping_add(1), 0);
        }
        if pressed(m, 0x20, 0x5f) {
            go(m, 3);
        }
    }
    m.pop_frame(0x18);
}

/// 800E8600: a selected port with status 3 opens dialog 1 (0xC02, text 1,
/// message 0x33) and the next step; else state 8.
pub fn port_new_e8600(m: &mut Machine) {
    m.push_frame(0x18);
    if port_status(m) == 3 {
        let s = save(m);
        let step = u32::from(m.lbu(s.wrapping_add(1)));
        m.sb(s.wrapping_add(1), step.wrapping_add(1) as u8);
        call(m, DIALOG_OPEN, [1, 0xc02, 0, 0]);
        call(m, DIALOG_TEXT, [1, 1, 0, 0]);
        message(m, 0x33);
    } else {
        let s = save(m);
        m.sb(s, 8);
    }
    m.pop_frame(0x18);
}

/// 800E868C: with dialog 1 closed (0) dialog 2 and the next step; else
/// ask it to close (unless 3).
pub fn close_dialog1_e868c(m: &mut Machine) {
    m.push_frame(0x18);
    let state = call(m, DIALOG_STATE, [1, 0, 0, 0]);
    if state == 0 {
        dialog(m, [2, 1, 0, 0x14]);
        advance(m, 1);
    } else if state != 3 {
        dialog(m, [1, 3, NONE, NONE]);
    }
    m.pop_frame(0x18);
}

/// 800E8708: the format question (dialog 2): no card -> state 1; confirm
/// (0x40) -> dialog 3 and the next step (message 0x35); cancel -> state 3.
pub fn format_question_e8708(m: &mut Machine) {
    m.push_frame(0x20);
    let state = call(m, DIALOG_STATE, [2, 0, 0, 0]);
    let status = port_status(m);
    if absent(status) {
        go(m, 1);
        m.pop_frame(0x20);
        return;
    }
    if status == 0 {
        m.pop_frame(0x20);
        return;
    }
    let ctx = context(m);
    if m.lw(ctx.wrapping_add(0x12c)) & 0x40 != 0 || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 1 {
                dialog(m, [2, 0, 0, 0]);
                dialog(m, [3, 1, 0, 0x14]);
                advance(m, 1);
                message(m, 0x35);
                m.pop_frame(0x20);
                return;
            }
        } else {
            dialog(m, [2, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 1);
            sound(m, 0x3c);
            m.pop_frame(0x20);
            return;
        }
    }
    if pressed(m, 0x20, 0x5f) || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 2 {
                dialog(m, [2, 0, 0, 0]);
                go(m, 3);
            }
        } else {
            dialog(m, [2, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 2);
            sound(m, 0x3b);
        }
    }
    m.pop_frame(0x20);
}

/// 800E8910: with dialog 3 open (2), format the selected port: done ->
/// dialog 3 closing and the next step (flag +9), failed -> dialog 3
/// closing, dialog 1 (0xC01, message 0x50) and two steps on.
pub fn format_e8910(m: &mut Machine) {
    m.push_frame(0x20);
    if call(m, DIALOG_STATE, [3, 0, 0, 0]) == 2 {
        let s = save(m);
        let port = u32::from(m.lbu(s.wrapping_add(4)));
        let result = card::format_e4760(m, port);
        if result == 1 {
            dialog(m, [3, 3, NONE, NONE]);
            let s = save(m);
            let step = m.lbu(s.wrapping_add(1));
            m.sb(s.wrapping_add(1), step.wrapping_add(1));
            let s = save(m);
            m.sb(s.wrapping_add(9), result as u8);
        } else if result == NONE {
            dialog(m, [3, 3, NONE, NONE]);
            let s = save(m);
            let step = u32::from(m.lbu(s.wrapping_add(1)));
            m.sb(s.wrapping_add(1), step.wrapping_add(2) as u8);
            call(m, DIALOG_OPEN, [1, 0xc01, 0, 0]);
            message(m, 0x50);
        }
    }
    m.pop_frame(0x20);
}

/// 800E89F8: the format failure (message 0x50): no card -> state 1; a
/// confirm press -> the story's next state, a cancel press -> state 0.
pub fn format_failed_e89f8(m: &mut Machine) {
    m.push_frame(0x18);
    let status = port_status(m);
    message(m, 0x50);
    if absent(status) {
        go(m, 1);
    } else if status != 0 {
        if pressed(m, 0x10, 0x2f) {
            let next = story_next(m);
            let s = save(m);
            m.sb(s, next as u8);
            let s = save(m);
            m.sb(s.wrapping_add(1), 0);
        }
        if pressed(m, 0x20, 0x5f) {
            go(m, 0);
        }
    }
    m.pop_frame(0x18);
}

/// 800E8B2C: a selected port with status 4 (no save file) -> state 9;
/// else the next step.
pub fn file_check_e8b2c(m: &mut Machine) {
    m.push_frame(0x18);
    if port_status(m) == 4 {
        go(m, 9);
    } else {
        advance(m, 1);
    }
    m.pop_frame(0x18);
}

/// The result of a record read of 800E8B98 / 800E8C50: failed -> message
/// 0x50, bad record -> 0x51, both to step 6; read -> the next step.
fn read_result(m: &mut Machine, result: u32) {
    if result == NONE {
        message(m, 0x50);
        let s = save(m);
        m.sb(s.wrapping_add(1), 6);
    }
    if result == 2 {
        message(m, 0x51);
        let s = save(m);
        m.sb(s.wrapping_add(1), 6);
    }
    if result == 1 {
        advance(m, 1);
    }
}

/// 800E8B98: read the selected port's 0x80 record.
pub fn read_info_e8b98(m: &mut Machine) {
    m.push_frame(0x18);
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    let result = card::read_info_e4ef0(m, port, 0);
    read_result(m, result);
    m.pop_frame(0x18);
}

/// 800E8C50: read the selected port's account records.
pub fn read_slots_e8c50(m: &mut Machine) {
    m.push_frame(0x18);
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    let result = card::read_slots_e5100(m, port, 1);
    read_result(m, result);
    m.pop_frame(0x18);
}

/// 800E8D08: find the context's account: none -> state 9, else its index
/// (+5) and the next step.
pub fn find_account_e8d08(m: &mut Machine) {
    m.push_frame(0x18);
    let account = records::find_account_e46a0(m);
    if account == NONE {
        go(m, 9);
    } else {
        let s = save(m);
        m.sb(s.wrapping_add(5), account as u8);
        advance(m, 1);
    }
    m.pop_frame(0x18);
}

/// 800E8D78: select the account (+5) as the current record; for a story
/// choice of 2..4 pack the progress into it -> state 0xC, step 6 (message
/// 0x3B); else the next step.
pub fn select_account_e8d78(m: &mut Machine) {
    m.push_frame(0x18);
    let s = save(m);
    let account = u32::from(m.lbu(s.wrapping_add(5)));
    records::select_slot_e30a4(m, account);
    let s = save(m);
    m.sb(s.wrapping_add(0xa), 0);
    let scene = m.lw(SCENE);
    let choice = m.lw(scene.wrapping_add(0xe0));
    if choice.wrapping_sub(2) < 3 {
        records::pack_slot_e2ab4(m);
        let s = save(m);
        m.sb(s, 0xc);
        let s = save(m);
        m.sb(s.wrapping_add(1), 6);
        message(m, 0x3b);
    } else {
        advance(m, 1);
    }
    m.pop_frame(0x18);
}

/// 800E8E28: read the account's mission records: failed / bad -> step 6
/// (messages 0x50 / 0x51); read -> state 0xD (dialog 2 cleared).
pub fn read_records_e8e28(m: &mut Machine) {
    m.push_frame(0x18);
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    let account = u32::from(m.lbu(s.wrapping_add(5)));
    let result = card::read_records_e53a0(m, port, account, 1);
    if result == NONE {
        let s = save(m);
        m.sb(s.wrapping_add(1), 6);
        message(m, 0x50);
    }
    if result == 2 {
        let s = save(m);
        m.sb(s.wrapping_add(1), 6);
        message(m, 0x51);
    }
    if result == 1 {
        let s = save(m);
        m.sb(s, 0xd);
        let s = save(m);
        m.sb(s.wrapping_add(1), 0);
        dialog(m, [2, 0, 0, 0]);
    }
    m.pop_frame(0x18);
}

/// 800E8EF0: the read failure: dialog 1 (0xC01); a confirm press -> the
/// story's next state (sound 0x3C), a cancel press -> state 0 (0x3B).
pub fn read_failed_e8ef0(m: &mut Machine) {
    m.push_frame(0x18);
    call(m, DIALOG_OPEN, [1, 0xc01, 0, 0]);
    if pressed(m, 0x10, 0x2f) {
        let next = story_next(m);
        let s = save(m);
        m.sb(s, next as u8);
        let s = save(m);
        m.sb(s.wrapping_add(1), 0);
        sound(m, 0x3c);
    }
    if pressed(m, 0x20, 0x5f) {
        let s = save(m);
        m.sb(s, 0);
        sound(m, 0x3b);
    }
    m.pop_frame(0x18);
}

/// 800E8FC4: start the new-file question: dialog 1 (0xC02, text 1), dialog
/// 2 cleared, dialog 3 (message 0x37), flag +9, no choice, the next step.
pub fn new_file_start_e8fc4(m: &mut Machine) {
    m.push_frame(0x18);
    call(m, DIALOG_OPEN, [1, 0xc02, 0, 0]);
    call(m, DIALOG_TEXT, [1, 1, 0, 0]);
    dialog(m, [2, 0, 0, 0]);
    dialog(m, [3, 1, 0, 0x14]);
    message(m, 0x37);
    let s = save(m);
    m.sb(s.wrapping_add(9), 1);
    let s = save(m);
    m.sb(s.wrapping_add(3), 0);
    advance(m, 1);
    m.pop_frame(0x18);
}

/// 800E9068: the new-file question (dialog 3): no card -> state 1;
/// confirm (0x40) -> state 0xA (message 0x2F); cancel -> state 3. Returns
/// 0.
pub fn new_file_question_e9068(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let status = port_status(m);
    let state = call(m, DIALOG_STATE, [3, 0, 0, 0]);
    if absent(status) {
        go(m, 1);
        m.pop_frame(0x20);
        return 0;
    }
    if status == 0 {
        m.pop_frame(0x20);
        return 0;
    }
    let ctx = context(m);
    if m.lw(ctx.wrapping_add(0x12c)) & 0x40 != 0 || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 1 {
                dialog(m, [3, 0, 0, 0]);
                go(m, 0xa);
                message(m, 0x2f);
            }
        } else {
            dialog(m, [3, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 1);
            sound(m, 0x3c);
        }
    }
    if pressed(m, 0x20, 0x5f) || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 2 {
                dialog(m, [3, 0, 0, 0]);
                go(m, 3);
            }
        } else {
            dialog(m, [3, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 2);
            sound(m, 0x3b);
        }
    }
    m.pop_frame(0x20);
    0
}

/// 800E927C: the selected port: a save file (5) -> state 0xB, no card ->
/// state 1, else (not 0) the next step.
pub fn file_present_e927c(m: &mut Machine) {
    m.push_frame(0x18);
    let status = port_status(m);
    if status == 5 {
        go(m, 0xb);
    } else if absent(status) {
        go(m, 1);
    } else if status != 0 {
        advance(m, 1);
    }
    m.pop_frame(0x18);
}

/// 800E9310: list the selected port's files: free blocks below 2 (not
/// enough) -> the next step (dialog 1 0xC01, message 0x38, monitor
/// reset), else two steps on.
pub fn free_blocks_e9310(m: &mut Machine) {
    m.push_frame(0x18);
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    let free = card::directory_e4db0(m, port);
    if free != NONE {
        if (free as i32) < 2 {
            advance(m, 1);
            message(m, 0x38);
            call(m, DIALOG_OPEN, [1, 0xc01, 0, 0]);
            card::reset_ports_e25d4(m);
        } else {
            advance(m, 2);
        }
    }
    m.pop_frame(0x18);
}

/// 800E93B8: not enough space: no card -> state 1 (and the presses still
/// count); a confirm press -> the story's next state, a cancel press ->
/// state 1. Returns 0.
pub fn no_space_e93b8(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let status = port_status(m);
    if absent(status) {
        go(m, 1);
    }
    if status != 0 {
        if pressed(m, 0x10, 0x2f) {
            let next = story_next(m);
            let s = save(m);
            m.sb(s, next as u8);
            let s = save(m);
            m.sb(s.wrapping_add(1), 0);
        }
        if pressed(m, 0x20, 0x5f) {
            go(m, 1);
        }
    }
    m.pop_frame(0x18);
    0
}

/// 800E94E0: whether the save file exists: yes -> the next step, no ->
/// three steps on.
pub fn file_exists_e94e0(m: &mut Machine) {
    m.push_frame(0x18);
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    let result = card::file_exists_e47b0(m, port);
    if result == 1 {
        advance(m, 1);
    }
    if result == NONE {
        advance(m, 3);
    }
    m.pop_frame(0x18);
}

/// 800E955C: write the file head: failed -> two steps on, written -> the
/// next step (0x80 record reset).
pub fn write_head_e955c(m: &mut Machine) {
    m.push_frame(0x18);
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    let result = card::write_head_e5664(m, port, 0);
    if result == NONE {
        advance(m, 2);
    }
    if result == 1 {
        advance(m, 1);
        records::reset_info_e3b5c(m);
    }
    m.pop_frame(0x18);
}

/// 800E95DC: write an empty 0x80 record: failed -> the next step, written
/// -> state 0xB.
pub fn write_empty_info_e95dc(m: &mut Machine) {
    m.push_frame(0x18);
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    let result = card::write_info_e57f0(m, port, NONE, 1);
    if result == NONE {
        advance(m, 1);
    }
    if result == 1 {
        go(m, 0xb);
    }
    m.pop_frame(0x18);
}

/// 800E965C: the write failure (message 0x50): no card -> state 1; a
/// confirm press -> the story's next state (0x3C), a cancel press -> state
/// 0 (0x3B). Returns 1.
pub fn write_failed_e965c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let status = port_status(m);
    let s = save(m);
    m.sb(s.wrapping_add(9), 0);
    message(m, 0x50);
    if absent(status) {
        go(m, 1);
    }
    if status != 0 {
        if pressed(m, 0x10, 0x2f) {
            let next = story_next(m);
            let s = save(m);
            m.sb(s, next as u8);
            let s = save(m);
            m.sb(s.wrapping_add(1), 0);
            sound(m, 0x3c);
        }
        if pressed(m, 0x20, 0x5f) {
            let s = save(m);
            m.sb(s, 0);
            sound(m, 0x3b);
        }
    }
    m.pop_frame(0x18);
    1
}

/// 800E97A4: start the account choice: dialog 1 (0xC10, text 4; message
/// 0x39), flag +9, the next step.
pub fn account_start_e97a4(m: &mut Machine) {
    m.push_frame(0x18);
    call(m, DIALOG_OPEN, [1, 0xc10, 0, 0]);
    call(m, DIALOG_TEXT, [1, 4, 0, 0]);
    dialog(m, [1, 1, 0, 0x3c]);
    message(m, 0x39);
    let s = save(m);
    m.sb(s.wrapping_add(9), 1);
    advance(m, 1);
    m.pop_frame(0x18);
}

/// 800E9824: the account choice: the marks, the panel (800E6A6C); no card
/// -> state 1 (message 0x2F); confirm -> state 0xC (0x3C), cancel -> state
/// 0 (0x3B); up / down (context +0x130 bits 1 / 0) move the account
/// (0..2, wrapping; 0x3D). Returns 0.
pub fn account_choice_e9824(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let status = port_status(m);
    message(m, if status == 0 { 0x2f } else { 0x39 });
    let s = save(m);
    m.sb(s.wrapping_add(0xa), 0);
    account_marks(m);
    if absent(status) {
        go(m, 1);
    }
    let s = save(m);
    let column = u32::from(m.lbu(s.wrapping_add(0xa)));
    accounts_e6a6c(m, column, 0);
    if status != 0 {
        if pressed(m, 0x10, 0x2f) {
            go(m, 0xc);
            sound(m, 0x3c);
        } else if pressed(m, 0x20, 0x5f) {
            go(m, 0);
            sound(m, 0x3b);
        } else {
            let ctx = context(m);
            let moves = m.lw(ctx.wrapping_add(0x130));
            if moves & 2 != 0 {
                let s = save(m);
                let account = m.lbu(s.wrapping_add(5));
                m.sb(s.wrapping_add(5), if account < 2 { account + 1 } else { 0 });
                sound(m, 0x3d);
            } else if moves & 1 != 0 {
                let s = save(m);
                let account = m.lbu(s.wrapping_add(5));
                m.sb(
                    s.wrapping_add(5),
                    if account != 0 { account - 1 } else { 2 },
                );
                sound(m, 0x3d);
            }
        }
    }
    m.pop_frame(0x20);
    0
}

/// 800E9AB4: the chosen account: present -> the overwrite question
/// (column 1; dialog 1 0xC20, text 5; message 0x3A) and the next step;
/// absent -> its record reset and two steps on. Then the panel.
pub fn account_chosen_e9ab4(m: &mut Machine) {
    m.push_frame(0x18);
    let s = save(m);
    let account = u32::from(m.lbu(s.wrapping_add(5)));
    if records::account_present_e4324(m, account) != 0 {
        advance(m, 1);
        let s = save(m);
        m.sh(s.wrapping_add(0x1a), 1);
        account_marks(m);
        call(m, DIALOG_OPEN, [1, 0xc20, 0, 0]);
        call(m, DIALOG_TEXT, [1, 5, 0, 0]);
        dialog(m, [1, 1, 0, 0x3c]);
        message(m, 0x3a);
    } else {
        advance(m, 2);
        let s = save(m);
        let account = u32::from(m.lbu(s.wrapping_add(5)));
        m.sh(s.wrapping_add(0x1a), 0);
        records::reset_account_e3658(m, account);
    }
    let s = save(m);
    let column = u32::from(m.lbu(s.wrapping_add(0xa)));
    let mode = m.lh(s.wrapping_add(0x1a)) as i32 as u32;
    accounts_e6a6c(m, column, mode);
    m.pop_frame(0x18);
}

/// 800E9C40: the overwrite question: no card -> state 3; confirm (0x40,
/// 0x3C) -> the next step with the account's record reset; cancel ->
/// state 0xB (0x3B). Returns 0.
pub fn overwrite_question_e9c40(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let status = port_status(m);
    let s = save(m);
    let column = u32::from(m.lbu(s.wrapping_add(0xa)));
    accounts_e6a6c(m, column, 1);
    account_marks(m);
    if absent(status) {
        let s = save(m);
        m.sb(s, 3);
        let s = save(m);
        m.sb(s.wrapping_add(1), 0);
        let s = save(m);
        m.sh(s.wrapping_add(0x1a), 0);
    }
    if status != 0 {
        let ctx = context(m);
        if m.lw(ctx.wrapping_add(0x12c)) & 0x40 != 0 {
            let s = save(m);
            let step = u32::from(m.lbu(s.wrapping_add(1)));
            m.sb(s.wrapping_add(1), step.wrapping_add(1) as u8);
            sound(m, 0x3c);
            let s = save(m);
            let account = u32::from(m.lbu(s.wrapping_add(5)));
            records::reset_account_e3658(m, account);
        }
        if pressed(m, 0x20, 0x5f) {
            let s = save(m);
            m.sb(s, 0xb);
            let s = save(m);
            m.sb(s.wrapping_add(1), 0);
            let s = save(m);
            m.sh(s.wrapping_add(0x1a), 0);
            sound(m, 0x3b);
        }
    }
    m.pop_frame(0x20);
    0
}

/// The account panel of the write steps (column +0xA, mode +0x1A).
fn panel(m: &mut Machine) {
    let s = save(m);
    let column = u32::from(m.lbu(s.wrapping_add(0xa)));
    let mode = m.lh(s.wrapping_add(0x1a)) as i32 as u32;
    accounts_e6a6c(m, column, mode);
}

/// 800E9E20: the panel; a selected port without a save file (4) -> the
/// next step (message 0x3B), else three steps on.
pub fn overwrite_check_e9e20(m: &mut Machine) {
    m.push_frame(0x20);
    let status = port_status(m);
    panel(m);
    if status == 4 {
        advance(m, 1);
        message(m, 0x3b);
    } else {
        advance(m, 3);
    }
    m.pop_frame(0x20);
}

/// 800E9EC4: the panel; whether the save file exists: yes -> the next
/// step, no -> four steps on.
pub fn write_exists_e9ec4(m: &mut Machine) {
    m.push_frame(0x18);
    panel(m);
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    let result = card::file_exists_e47b0(m, port);
    if result != 0 {
        if result == NONE {
            advance(m, 4);
        }
        if result == 1 {
            advance(m, 1);
        }
    }
    m.pop_frame(0x18);
}

/// 800E9F64: the panel; write the file head: failed -> three steps on,
/// written -> the next step (0x80 record reset).
pub fn write_head_e9f64(m: &mut Machine) {
    m.push_frame(0x18);
    panel(m);
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    let result = card::write_head_e5664(m, port, 0);
    if result != 0 {
        if result == NONE {
            advance(m, 3);
        }
        if result == 1 {
            advance(m, 1);
            records::reset_info_e3b5c(m);
        }
    }
    m.pop_frame(0x18);
}

/// 800EA004: the panel; write the 0x80 record marking the account: failed
/// -> two steps on; written -> pack the progress, name it, reset the
/// account's and the current record's marks, the next step.
pub fn write_info_ea004(m: &mut Machine) {
    m.push_frame(0x18);
    panel(m);
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    let account = u32::from(m.lbu(s.wrapping_add(5)));
    let result = card::write_info_e57f0(m, port, account, 0);
    if result == NONE {
        advance(m, 2);
    }
    if result == 1 {
        records::pack_slot_e2ab4(m);
        card::name_from_context_e2a7c(m);
        let s = save(m);
        let account = u32::from(m.lbu(s.wrapping_add(5)));
        records::reset_account_e3658(m, account);
        records::reset_account_e3658(m, NONE);
        advance(m, 1);
    }
    m.pop_frame(0x18);
}

/// 800EA0C8: (the panel for a story choice below 2) write the current
/// account record: failed -> the next step; written -> state 0x10 for a
/// story choice of 2..4, else 0xD.
pub fn write_slot_ea0c8(m: &mut Machine) {
    m.push_frame(0x18);
    let scene = m.lw(SCENE);
    if m.lw(scene.wrapping_add(0xe0)) < 2 {
        panel(m);
    }
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    let account = u32::from(m.lbu(s.wrapping_add(5)));
    let result = card::write_slot_e5a28(m, port, account, NONE, 1);
    if result == NONE {
        advance(m, 1);
    }
    if result == 1 {
        let scene = m.lw(SCENE);
        let choice = m.lw(scene.wrapping_add(0xe0));
        let state = if choice.wrapping_sub(2) < 3 {
            0x10
        } else {
            0xd
        };
        let s = save(m);
        m.sb(s, state);
        let s = save(m);
        m.sb(s.wrapping_add(1), 0);
    }
    m.pop_frame(0x18);
}

/// 800EA1C4: the write failure (dialog 1 0xC80, text 7; message 0x3D;
/// flag +9): no card -> state 1; any press -> state 0 (dialog 1 cleared,
/// sound 0x3B). Returns 1.
pub fn account_failed_ea1c4(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let status = port_status(m);
    let s = save(m);
    m.sb(s.wrapping_add(9), 1);
    dialog(m, [1, 2, 0x14, 0x14]);
    call(m, DIALOG_OPEN, [1, 0xc80, 0, 0]);
    call(m, DIALOG_TEXT, [1, 7, 0, 0]);
    message(m, 0x3d);
    if absent(status) {
        go(m, 1);
    }
    if status != 0 {
        let ctx = context(m);
        if m.lw(ctx.wrapping_add(0x12c)) != 0 {
            let s = save(m);
            m.sb(s.wrapping_add(9), 0);
            dialog(m, [1, 0, 0, 0]);
            let s = save(m);
            m.sb(s, 0);
            let s = save(m);
            m.sb(s.wrapping_add(1), 0);
            sound(m, 0x3b);
        }
    }
    m.pop_frame(0x20);
    1
}

/// The six record marks: 0 for the selected record (+6), else 1.
fn record_marks(m: &mut Machine) {
    for i in 0..6u32 {
        let s = save(m);
        let mark = u8::from(u32::from(m.lbu(s.wrapping_add(6))) != i);
        let block = messages(m);
        m.sb(block.wrapping_add(i).wrapping_add(2), mark);
    }
}

/// 800EA2E8: start the record choice: dialog 1 (0xD00, texts 8 and 0xB),
/// dialogs 1 and 0xF, message 0x3E, flag +9, the six record marks, the
/// next step.
pub fn record_start_ea2e8(m: &mut Machine) {
    m.push_frame(0x18);
    call(m, DIALOG_OPEN, [1, 0xd00, 0, 0]);
    call(m, DIALOG_TEXT, [1, 8, 0, 0]);
    call(m, DIALOG_TEXT, [1, 0xb, 0, 0]);
    dialog(m, [1, 1, 0, 0x14]);
    dialog(m, [0xf, 1, 0, 0x14]);
    message(m, 0x3e);
    let s = save(m);
    m.sb(s.wrapping_add(9), 1);
    record_marks(m);
    advance(m, 1);
    m.pop_frame(0x18);
}

/// Marks with a highlighted selection: for `count` entries, the selected
/// one (field `field` of the save block) 3 with context +0x124 bit 0x10
/// set, else 0; the others 1.
fn selected_marks(m: &mut Machine, field: u32, count: u32) {
    for i in 0..count {
        let s = save(m);
        if u32::from(m.lbu(s.wrapping_add(field))) != i {
            let block = messages(m);
            m.sb(block.wrapping_add(i).wrapping_add(2), 1);
        } else {
            let ctx = context(m);
            let mark = if m.lw(ctx.wrapping_add(0x124)) & 0x10 != 0 {
                3
            } else {
                0
            };
            let block = messages(m);
            m.sb(block.wrapping_add(i).wrapping_add(2), mark);
        }
    }
}

/// The record panel of the selected record (+6) of the account (+5),
/// column +0xA.
fn record_panel(m: &mut Machine) {
    let s = save(m);
    let slot = u32::from(m.lbu(s.wrapping_add(6)));
    let account = u32::from(m.lbu(s.wrapping_add(5)));
    let column = u32::from(m.lbu(s.wrapping_add(0xa)));
    super::ov2_p3_panels::record_e6c4c(m, slot, account, column);
}

/// The selected record (+6) shown in the message block (+9, +0x1E).
fn record_number(m: &mut Machine) {
    for offset in [9, 0x1e] {
        let s = save(m);
        let slot = m.lbu(s.wrapping_add(6));
        let block = messages(m);
        m.sb(block.wrapping_add(offset), slot.wrapping_add(1));
    }
}

/// 800EA3F4: the record choice (dialog 1): the marks and the panel; no
/// card -> state 3; with the dialog shown (2) right / left (context +0x130
/// bits 3 / 2) move the record (0..5, wrapping; 0x3D); confirm -> state
/// 0xE, cancel -> state 0. Returns 0.
pub fn record_choice_ea3f4(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let state = call(m, DIALOG_STATE, [1, 0, 0, 0]);
    let status = port_status(m);
    record_marks(m);
    record_panel(m);
    let block = messages(m);
    m.sb(block.wrapping_add(0x1e), 0);
    if absent(status) {
        go(m, 3);
    }
    if status == 0 {
        m.pop_frame(0x20);
        return 0;
    }
    if state == 2 {
        let ctx = context(m);
        if m.lw(ctx.wrapping_add(0x130)) & 8 != 0 {
            let s = save(m);
            let slot = m.lbu(s.wrapping_add(6));
            m.sb(s.wrapping_add(6), if slot < 5 { slot + 1 } else { 0 });
            sound(m, 0x3d);
        }
        let ctx = context(m);
        if m.lw(ctx.wrapping_add(0x130)) & 4 != 0 {
            let s = save(m);
            let slot = m.lbu(s.wrapping_add(6));
            m.sb(s.wrapping_add(6), if slot != 0 { slot - 1 } else { 5 });
            sound(m, 0x3d);
        }
    }
    if pressed(m, 0x10, 0x2f) || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 1 {
                dialog(m, [1, 0, 0, 0]);
                go(m, 0xe);
            }
        } else {
            dialog(m, [1, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 1);
            sound(m, 0x3c);
        }
    }
    if pressed(m, 0x20, 0x5f) || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 2 {
                dialog(m, [1, 0, 0, 0]);
                go(m, 0);
            }
        } else {
            dialog(m, [1, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 2);
            sound(m, 0x3b);
        }
    }
    m.pop_frame(0x20);
    0
}

/// 800EA744: the chosen record: the panel, no choice; a present record
/// (of the current account for column 1, of the account +5 for column 0)
/// two steps on, else one; dialogs 2 and 4; the marks.
pub fn record_chosen_ea744(m: &mut Machine) {
    m.push_frame(0x18);
    record_panel(m);
    let s = save(m);
    m.sb(s.wrapping_add(3), 0);
    let s = save(m);
    let column = m.lbu(s.wrapping_add(0xa));
    let present = match column {
        0 => {
            let account = u32::from(m.lbu(s.wrapping_add(5)));
            let slot = u32::from(m.lbu(s.wrapping_add(6)));
            records::present_e42ac(m, account, slot)
        }
        1 => {
            let slot = u32::from(m.lbu(s.wrapping_add(6)));
            records::present_e42ac(m, NONE, slot)
        }
        _ => 0,
    };
    match present {
        1 => {
            let s = save(m);
            let step = u32::from(m.lbu(s.wrapping_add(1)));
            m.sb(s.wrapping_add(1), step.wrapping_add(2) as u8);
            dialog(m, [2, 1, 0, 0x14]);
        }
        0 => {
            let s = save(m);
            let step = u32::from(m.lbu(s.wrapping_add(1)));
            m.sb(s.wrapping_add(1), step.wrapping_add(1) as u8);
            dialog(m, [2, 1, 0, 0x14]);
        }
        _ => {}
    }
    dialog(m, [4, 1, 0, 0x14]);
    record_marks(m);
    m.pop_frame(0x18);
}

/// 800EA8AC: the save question for an empty record (dialogs 2 and 4,
/// message 0x3F): no card -> state 3; confirm (0x40) -> state 0xF, cancel
/// -> state 0xD. Returns 0.
pub fn record_question_ea8ac(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let status = port_status(m);
    record_panel(m);
    let state = call(m, DIALOG_STATE, [2, 0, 0, 0]);
    message(m, 0x3f);
    selected_marks(m, 6, 6);
    record_number(m);
    if absent(status) {
        go(m, 3);
    }
    if status != 0 {
        record_confirm(m, state, 0xf, 0xd);
    }
    m.pop_frame(0x20);
    0
}

/// The confirm (0x40) / cancel tail of 800EA8AC and 800EAC04 (dialogs 2
/// and 4): -> state `confirmed` or `cancelled`.
fn record_confirm(m: &mut Machine, state: u32, confirmed: u8, cancelled: u8) {
    let ctx = context(m);
    if m.lw(ctx.wrapping_add(0x12c)) & 0x40 != 0 || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 1 {
                dialog(m, [2, 0, 0, 0]);
                dialog(m, [4, 0, 0, 0]);
                go(m, u32::from(confirmed));
            }
        } else {
            dialog(m, [2, 3, NONE, NONE]);
            dialog(m, [4, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 1);
            sound(m, 0x3c);
        }
    }
    if pressed(m, 0x20, 0x5f) || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 2 {
                dialog(m, [2, 0, 0, 0]);
                dialog(m, [4, 0, 0, 0]);
                go(m, u32::from(cancelled));
            }
        } else {
            dialog(m, [2, 3, NONE, NONE]);
            dialog(m, [4, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 2);
            sound(m, 0x3b);
        }
    }
}

/// 800EAC04: the overwrite question for a present record (message 0x40):
/// no card -> state 3; confirm (0x40) -> state 0xF, cancel -> state 0xD.
pub fn record_overwrite_eac04(m: &mut Machine) {
    m.push_frame(0x20);
    let state = call(m, DIALOG_STATE, [2, 0, 0, 0]);
    let status = port_status(m);
    record_panel(m);
    message(m, 0x40);
    selected_marks(m, 6, 6);
    record_number(m);
    if absent(status) {
        go(m, 3);
    } else if status != 0 {
        record_confirm(m, state, 0xf, 0xd);
    }
    m.pop_frame(0x20);
}

/// 800EAF44: pack the progress into the current account record (message
/// 0x41) and the next step; the marks; column +0xA 0 for a present record
/// (of the current account), else 1.
pub fn record_pack_eaf44(m: &mut Machine) {
    m.push_frame(0x20);
    let present = records::records_e458c(m);
    record_panel(m);
    advance(m, 1);
    message(m, 0x41);
    let block = messages(m);
    m.sb(block.wrapping_add(0x1e), 0);
    records::pack_slot_e2ab4(m);
    record_marks(m);
    let s = save(m);
    let slot = u32::from(m.lbu(s.wrapping_add(6)));
    if ((present as i32) >> (slot & 31)) & 1 != 0 {
        m.sb(s.wrapping_add(0xa), 0);
    } else {
        m.sb(s.wrapping_add(0xa), 1);
    }
    m.pop_frame(0x20);
}

/// 800EB064: write the account record with the selected record marked:
/// failed -> two steps on; written -> pack the mission record (800E2F44)
/// and the next step. The marks.
pub fn record_write_slot_eb064(m: &mut Machine) {
    m.push_frame(0x18);
    record_panel(m);
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    let account = u32::from(m.lbu(s.wrapping_add(5)));
    let slot = u32::from(m.lbu(s.wrapping_add(6)));
    let result = card::write_slot_e5a28(m, port, account, slot, 1);
    if result == NONE {
        advance(m, 2);
    }
    if result == 1 {
        advance(m, 1);
        records::pack_record_e2f44(m);
    }
    record_marks(m);
    m.pop_frame(0x18);
}

/// 800EB174: write the mission record: failed -> the next step; written
/// -> state 0x10. The marks.
pub fn record_write_eb174(m: &mut Machine) {
    m.push_frame(0x18);
    record_panel(m);
    let s = save(m);
    let port = u32::from(m.lbu(s.wrapping_add(4)));
    let account = u32::from(m.lbu(s.wrapping_add(5)));
    let slot = u32::from(m.lbu(s.wrapping_add(6)));
    let result = card::write_record_e5ca4(m, port, account, slot, 1);
    if result == NONE {
        advance(m, 1);
    }
    if result == 1 {
        go(m, 0x10);
    }
    record_marks(m);
    m.pop_frame(0x18);
}

/// 800EB280: the record write failure (dialog 1 0xC80, text 7): no card ->
/// state 1; message 0x43; any press -> state 0 (dialog 1 cleared, sound
/// 0x3B). Returns 1.
pub fn record_failed_eb280(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let status = port_status(m);
    dialog(m, [1, 2, 0x14, 0x14]);
    call(m, DIALOG_OPEN, [1, 0xc80, 0, 0]);
    call(m, DIALOG_TEXT, [1, 7, 0, 0]);
    if absent(status) {
        go(m, 1);
    }
    if status != 0 {
        message(m, 0x43);
        let ctx = context(m);
        if m.lw(ctx.wrapping_add(0x12c)) != 0 {
            dialog(m, [1, 0, 0, 0]);
            let s = save(m);
            m.sb(s, 0);
            let s = save(m);
            m.sb(s.wrapping_add(1), 0);
            sound(m, 0x3b);
        }
    }
    m.pop_frame(0x20);
    1
}

/// 800EB388: the save is done: dialog 1 (0xE00, text 9), the next step,
/// the timer (+7) and flag +9 cleared, message 0x44; the record (+6) as
/// the context's progress index (+0x95D).
pub fn done_start_eb388(m: &mut Machine) {
    m.push_frame(0x18);
    call(m, DIALOG_OPEN, [1, 0xe00, 0, 0]);
    call(m, DIALOG_TEXT, [1, 9, 0, 0]);
    advance(m, 1);
    let s = save(m);
    m.sb(s.wrapping_add(7), 0);
    let s = save(m);
    m.sb(s.wrapping_add(9), 0);
    message(m, 0x44);
    let block = messages(m);
    m.sb(block.wrapping_add(0x1e), 0);
    let s = save(m);
    let ctx = context(m);
    let slot = m.lbu(s.wrapping_add(6));
    m.sb(ctx.wrapping_add(0x95d), slot);
    m.pop_frame(0x18);
}

/// 800EB42C: the done screen: the current account's name; after 0x79
/// frames (+7) or any press, the story's next state.
pub fn done_wait_eb42c(m: &mut Machine) {
    m.push_frame(0x18);
    call(m, 0x800f_33a8, [0x4a, 0xd6, 0, 0]);
    let name = records::account_name_e425c(m, 0);
    call(m, 0x800f_4ae0, [name, 0, 0, 0]);
    let s = save(m);
    let leave = if m.lbu(s.wrapping_add(7)) < 0x79 {
        let ctx = context(m);
        m.lw(ctx.wrapping_add(0x12c)) != 0
    } else {
        true
    };
    if leave {
        let next = story_next(m);
        let s = save(m);
        m.sb(s, next as u8);
        let s = save(m);
        m.sb(s.wrapping_add(1), 0);
        let s = save(m);
        m.sb(s.wrapping_add(7), 0);
    }
    let s = save(m);
    let timer = m.lbu(s.wrapping_add(7));
    m.sb(s.wrapping_add(7), timer.wrapping_add(1));
    m.pop_frame(0x18);
}

/// 800EB4F4: start the quit question: flag +8, dialog 1 (0xC01), dialog 2,
/// the next step, message 0x45.
pub fn quit_start_eb4f4(m: &mut Machine) {
    m.push_frame(0x18);
    let s = save(m);
    m.sb(s.wrapping_add(8), 1);
    call(m, DIALOG_OPEN, [1, 0xc01, 0, 0]);
    call(m, DIALOG_TEXT, [1, 0, 0, 0]);
    dialog(m, [2, 1, 0, 0x14]);
    advance(m, 1);
    message(m, 0x45);
    m.pop_frame(0x18);
}

/// 800EB57C: the quit question (dialog 2): confirm -> state 0x12; cancel
/// -> flag +2 = 2 and story flag 2 (context +0x96C).
pub fn quit_question_eb57c(m: &mut Machine) {
    m.push_frame(0x20);
    let state = call(m, DIALOG_STATE, [2, 0, 0, 0]);
    if pressed(m, 0x10, 0x2f) || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            if m.lbu(s.wrapping_add(3)) == 1 {
                dialog(m, [2, 0, 0, 0]);
                go(m, 0x12);
            }
        } else {
            dialog(m, [2, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 1);
            sound(m, 0x3c);
        }
    }
    if pressed(m, 0x20, 0x5f) || state == 0 {
        if state == 0 || state == 3 {
            let s = save(m);
            let choice = m.lbu(s.wrapping_add(3));
            if choice == 2 {
                dialog(m, [2, 0, 0, 0]);
                let s = save(m);
                m.sb(s.wrapping_add(2), choice);
                let ctx = context(m);
                let flags = m.lw(ctx.wrapping_add(0x96c));
                m.sw(ctx.wrapping_add(0x96c), flags | 2);
            }
        } else {
            dialog(m, [2, 3, NONE, NONE]);
            let s = save(m);
            m.sb(s.wrapping_add(3), 2);
            sound(m, 0x3b);
        }
    }
    m.pop_frame(0x20);
}

/// The disc a mission needs (0x80112750, by context +0x38C).
const MISSION_DISC: u32 = 0x8011_2750;
/// Resident routines: the disc number (8002B504, `replay`; -1 for none)
/// and the lid check (8002B428, `mainloop`).
const DISC_NUMBER: u32 = 0x8002_b504;
const LID_OPENED: u32 = 0x8002_b428;

/// The "insert disc" step: message base (+0x18) + `offset`, the lid
/// check, then step +1 with the message base 0x1F.
fn ask_disc(m: &mut Machine, offset: u8) {
    let s = save(m);
    let base = m.lbu(s.wrapping_add(0x18));
    let block = messages(m);
    m.sb(block.wrapping_add(1), base.wrapping_add(offset));
    call(m, LID_OPENED, [0, 0, 0, 0]);
    next_disc_step(m);
}

fn next_disc_step(m: &mut Machine) {
    let s = save(m);
    let step = m.lbu(s.wrapping_add(1));
    m.sh(s.wrapping_add(0x18), 0x1f);
    m.sb(s.wrapping_add(1), step.wrapping_add(1));
}

fn mission_disc(m: &mut Machine) -> u8 {
    let ctx = context(m);
    let mission = m.lh(ctx.wrapping_add(0x38c)) as i32 as u32;
    m.lbu(mission.wrapping_add(MISSION_DISC))
}

/// 800EB73C: the disc check before a mission: with the monitor stopped,
/// the inserted disc (8002B504; none: message 0x49) against the mission's
/// (0x80112750): the right one (or a mission for either, 3) sets flag +2;
/// else the "insert disc 1/2" message and the lid check.
pub fn disc_check_eb73c(m: &mut Machine) {
    m.push_frame(0x20);
    let ctx = context(m);
    m.sb(ctx.wrapping_add(0x95e), 0);
    if card::stop_monitor_e264c(m) == 0 {
        m.pop_frame(0x20);
        return;
    }
    let disc = call(m, DISC_NUMBER, [0, 0, 0, 0]);
    if disc == 0 {
        message(m, 0x49);
        m.pop_frame(0x20);
        return;
    }
    let needed = mission_disc(m);
    if needed == 3 || disc == 3 {
        let s = save(m);
        m.sb(s.wrapping_add(2), 1);
        m.pop_frame(0x20);
        return;
    }
    if disc == 4 || disc == NONE {
        if needed == 1 {
            let s = save(m);
            let base = m.lbu(s.wrapping_add(0x18));
            let block = messages(m);
            m.sb(block.wrapping_add(1), base.wrapping_add(0x46));
        }
        if mission_disc(m) == 2 {
            let s = save(m);
            let base = m.lbu(s.wrapping_add(0x18));
            let block = messages(m);
            m.sb(block.wrapping_add(1), base.wrapping_add(0x47));
        }
        call(m, LID_OPENED, [0, 0, 0, 0]);
        next_disc_step(m);
        m.pop_frame(0x20);
        return;
    }
    if disc == 1 {
        if u32::from(needed) == disc {
            let s = save(m);
            m.sb(s.wrapping_add(2), 1);
        } else {
            ask_disc(m, 0x47);
        }
        m.pop_frame(0x20);
        return;
    }
    if disc == 2 {
        if u32::from(mission_disc(m)) != disc {
            ask_disc(m, 0x46);
        } else {
            let s = save(m);
            m.sb(s.wrapping_add(2), 1);
        }
    }
    m.pop_frame(0x20);
}

/// The routines of this file, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let _ = args;
    Some(match target {
        0x800e_7498 => {
            load_start_e7498(m);
            0
        }
        0x800e_7554 => {
            load_question_e7554(m);
            0
        }
        0x800e_7758 => {
            check_ports_e7758(m);
            0
        }
        0x800e_77ec => {
            wait_ports_e77ec(m);
            0
        }
        0x800e_78a0 => {
            no_cards_e78a0(m);
            0
        }
        0x800e_7948 => {
            no_cards_question_e7948(m);
            0
        }
        0x800e_7b8c => {
            port_start_e7b8c(m);
            0
        }
        0x800e_7cdc => {
            port_choice_e7cdc(m);
            0
        }
        0x800e_80b8 => {
            port_check_e80b8(m);
            0
        }
        0x800e_8174 => {
            port_question_e8174(m);
            0
        }
        0x800e_83ac => {
            open_dialog2_e83ac(m);
            0
        }
        0x800e_8404 => {
            port_any_e8404(m);
            0
        }
        0x800e_8454 => {
            port_ready_e8454(m);
            0
        }
        0x800e_84e8 => {
            port_error_e84e8(m);
            0
        }
        0x800e_8600 => {
            port_new_e8600(m);
            0
        }
        0x800e_868c => {
            close_dialog1_e868c(m);
            0
        }
        0x800e_8708 => {
            format_question_e8708(m);
            0
        }
        0x800e_8910 => {
            format_e8910(m);
            0
        }
        0x800e_89f8 => {
            format_failed_e89f8(m);
            0
        }
        0x800e_8b2c => {
            file_check_e8b2c(m);
            0
        }
        0x800e_8b98 => {
            read_info_e8b98(m);
            0
        }
        0x800e_8c50 => {
            read_slots_e8c50(m);
            0
        }
        0x800e_8d08 => {
            find_account_e8d08(m);
            0
        }
        0x800e_8d78 => {
            select_account_e8d78(m);
            0
        }
        0x800e_8e28 => {
            read_records_e8e28(m);
            0
        }
        0x800e_8ef0 => {
            read_failed_e8ef0(m);
            0
        }
        0x800e_8fc4 => {
            new_file_start_e8fc4(m);
            0
        }
        0x800e_9068 => new_file_question_e9068(m),
        0x800e_927c => {
            file_present_e927c(m);
            0
        }
        0x800e_9310 => {
            free_blocks_e9310(m);
            0
        }
        0x800e_93b8 => no_space_e93b8(m),
        0x800e_94e0 => {
            file_exists_e94e0(m);
            0
        }
        0x800e_955c => {
            write_head_e955c(m);
            0
        }
        0x800e_95dc => {
            write_empty_info_e95dc(m);
            0
        }
        0x800e_965c => write_failed_e965c(m),
        0x800e_97a4 => {
            account_start_e97a4(m);
            0
        }
        0x800e_9824 => account_choice_e9824(m),
        0x800e_9ab4 => {
            account_chosen_e9ab4(m);
            0
        }
        0x800e_9c40 => overwrite_question_e9c40(m),
        0x800e_9e20 => {
            overwrite_check_e9e20(m);
            0
        }
        0x800e_9ec4 => {
            write_exists_e9ec4(m);
            0
        }
        0x800e_9f64 => {
            write_head_e9f64(m);
            0
        }
        0x800e_a004 => {
            write_info_ea004(m);
            0
        }
        0x800e_a0c8 => {
            write_slot_ea0c8(m);
            0
        }
        0x800e_a1c4 => account_failed_ea1c4(m),
        0x800e_a2e8 => {
            record_start_ea2e8(m);
            0
        }
        0x800e_a3f4 => record_choice_ea3f4(m),
        0x800e_a744 => {
            record_chosen_ea744(m);
            0
        }
        0x800e_a8ac => record_question_ea8ac(m),
        0x800e_ac04 => {
            record_overwrite_eac04(m);
            0
        }
        0x800e_af44 => {
            record_pack_eaf44(m);
            0
        }
        0x800e_b064 => {
            record_write_slot_eb064(m);
            0
        }
        0x800e_b174 => {
            record_write_eb174(m);
            0
        }
        0x800e_b280 => record_failed_eb280(m),
        0x800e_b388 => {
            done_start_eb388(m);
            0
        }
        0x800e_b42c => {
            done_wait_eb42c(m);
            0
        }
        0x800e_b4f4 => {
            quit_start_eb4f4(m);
            0
        }
        0x800e_b57c => {
            quit_question_eb57c(m);
            0
        }
        0x800e_b73c => {
            disc_check_eb73c(m);
            0
        }
        _ => return None,
    })
}
