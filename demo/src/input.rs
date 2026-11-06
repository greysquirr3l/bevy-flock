//! Turning keystrokes into a camera: a decoder that is a pure function of the bytes it is fed.
//!
//! A terminal delivers unmodified bytes, in whatever chunks the kernel felt like handing over: an
//! arrow key is the three bytes `ESC [ A` and there is no promise they arrive together. So the
//! decoder keeps a small buffer, consumes only what it can, and reports the rest as pending. That
//! makes it a state machine with no clock and no I/O, which is why it can be tested by feeding it
//! byte slices — including the one-byte-at-a-time case that a naive parser gets wrong.

use crate::camera_rig::CameraInput;

/// A one-shot instruction, as distinct from a held key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Show view `n`, counting from one as the keyboard labels them.
    View(usize),
    /// Start or stop the scripted playback.
    Playback,
    /// Double the flying speed.
    Faster,
    /// Halve the flying speed.
    Slower,
    /// Widen the lens.
    ZoomOut,
    /// Narrow the lens.
    ZoomIn,
    /// Toggle the help line.
    Help,
    /// Leave.
    Quit,
}

/// Which keys are down, as a bitset over a fixed table.
///
/// A `Vec<Key>` would allocate on every keystroke; eleven booleans in a `u16` do not, and a decoder
/// that runs sixty times a second should not be the reason a loop allocates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held(u16);

impl Held {
    /// Whether `key` is down.
    #[must_use]
    pub fn is_down(self, key: Key) -> bool {
        self.0 & (1 << key as u16) != 0
    }

    fn set(&mut self, key: Key, down: bool) {
        let bit = 1 << key as u16;
        if down {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }

    /// Every key up, which is what a focus loss should do.
    pub fn release_all(&mut self) {
        self.0 = 0;
    }
}

/// The keys the playground understands, in the order of their bit in [`Held`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Key {
    /// Walk forward.
    Forward = 0,
    /// Walk back.
    Back = 1,
    /// Slide left.
    Left = 2,
    /// Slide right.
    Right = 3,
    /// Climb.
    Up = 4,
    /// Descend.
    Down = 5,
    /// Turn left.
    LookLeft = 6,
    /// Turn right.
    LookRight = 7,
    /// Tilt up.
    LookUp = 8,
    /// Tilt down.
    LookDown = 9,
}

impl Key {
    /// The bit this key occupies.
    #[must_use]
    pub fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// How fast the arrow keys turn, radians per second.
pub const LOOK_RATE: f32 = 1.6;

/// A decoder for the bytes a terminal sends.
///
/// Feed it whatever arrived; it holds an incomplete escape sequence until the rest turns up. Commands
/// come out of [`Decoder::feed`], held keys out of [`Decoder::held`] and [`Decoder::camera_input`].
#[derive(Debug, Default)]
pub struct Decoder {
    pending: Vec<u8>,
    held: Held,
}

impl Decoder {
    /// An empty decoder with every key up.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Which keys are down.
    #[must_use]
    pub fn held(&self) -> Held {
        self.held
    }

    /// The bytes still waiting for the rest of a sequence.
    #[must_use]
    pub fn pending(&self) -> &[u8] {
        &self.pending
    }

    /// Consume `bytes`, returning the one-shot commands they completed.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Command> {
        self.pending.extend_from_slice(bytes);
        let mut commands = Vec::new();
        let mut consumed = 0;
        while let Some(byte) = self.pending.get(consumed).copied() {
            match byte {
                0x1b => {
                    // An escape sequence: `ESC [` then one final byte. Anything shorter is either a
                    // lone ESC (quit) or an incomplete sequence that must wait for the next read.
                    let rest = self.pending.len() - consumed;
                    let second = self.pending.get(consumed + 1).copied();
                    if rest == 1 {
                        break;
                    }
                    if second != Some(b'[') {
                        commands.push(Command::Quit);
                        consumed += 1;
                        continue;
                    }
                    if rest < 3 {
                        break;
                    }
                    match self.pending.get(consumed + 2).copied() {
                        Some(b'A') => self.held.set(Key::LookUp, true),
                        Some(b'B') => self.held.set(Key::LookDown, true),
                        Some(b'C') => self.held.set(Key::LookRight, true),
                        Some(b'D') => self.held.set(Key::LookLeft, true),
                        _ => {}
                    }
                    consumed += 3;
                }
                b'w' | b'W' => {
                    self.held.set(Key::Forward, true);
                    consumed += 1;
                }
                b's' | b'S' => {
                    self.held.set(Key::Back, true);
                    consumed += 1;
                }
                b'a' | b'A' => {
                    self.held.set(Key::Left, true);
                    consumed += 1;
                }
                b'd' | b'D' => {
                    self.held.set(Key::Right, true);
                    consumed += 1;
                }
                b'e' | b'E' => {
                    self.held.set(Key::Up, true);
                    consumed += 1;
                }
                b'c' | b'C' => {
                    self.held.set(Key::Down, true);
                    consumed += 1;
                }
                b'1'..=b'9' => {
                    let index = usize::from(byte - b'0');
                    commands.push(Command::View(index));
                    consumed += 1;
                }
                b'p' | b'P' => {
                    commands.push(Command::Playback);
                    consumed += 1;
                }
                b'+' | b'=' => {
                    commands.push(Command::Faster);
                    consumed += 1;
                }
                b'-' | b'_' => {
                    commands.push(Command::Slower);
                    consumed += 1;
                }
                b'[' => {
                    commands.push(Command::ZoomOut);
                    consumed += 1;
                }
                b']' => {
                    commands.push(Command::ZoomIn);
                    consumed += 1;
                }
                b'h' | b'H' | b'?' => {
                    commands.push(Command::Help);
                    consumed += 1;
                }
                // Ctrl-C arrives as ETX; both quit.
                b'q' | b'Q' | 0x03 => {
                    commands.push(Command::Quit);
                    consumed += 1;
                }
                _ => consumed += 1,
            }
        }
        self.pending.drain(..consumed);
        commands
    }

    /// Note that a key went up. Terminals do not report releases, so the caller decides: the
    /// interactive loop calls this with the key's bit when an arrow key's sequence is re-sent, and
    /// with everything on a focus loss.
    pub fn release(&mut self, key: Key) {
        self.held.set(key, false);
    }

    /// Decide what an incomplete sequence was, after a timeout.
    ///
    /// `ESC [ A` arrives in order or not at all, but a bare `ESC` — which is how every terminal
    /// spells the Escape key — is indistinguishable from the first byte of an arrow key until the
    /// next byte fails to arrive. The interactive loop calls this when its read returned nothing for
    /// a couple of frames, and a single pending escape then reads as what it almost certainly is.
    pub fn flush(&mut self) -> Vec<Command> {
        let mut commands = Vec::new();
        if self.pending == [0x1b] {
            commands.push(Command::Quit);
        }
        self.pending.clear();
        commands
    }

    /// Release every key, which is what leaving the window means.
    pub fn release_all(&mut self) {
        self.held.release_all();
    }

    /// The axes the held keys describe.
    #[must_use]
    pub fn camera_input(&self) -> CameraInput {
        fn axis(positive: bool, negative: bool) -> f32 {
            f32::from(i8::from(positive)) - f32::from(i8::from(negative))
        }
        CameraInput {
            forward: axis(
                self.held.is_down(Key::Forward),
                self.held.is_down(Key::Back),
            ),
            strafe: axis(self.held.is_down(Key::Right), self.held.is_down(Key::Left)),
            rise: axis(self.held.is_down(Key::Up), self.held.is_down(Key::Down)),
            look_yaw: axis(
                self.held.is_down(Key::LookRight),
                self.held.is_down(Key::LookLeft),
            ),
            look_pitch: axis(
                self.held.is_down(Key::LookUp),
                self.held.is_down(Key::LookDown),
            ),
            zoom: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_arrow_key_split_across_reads_still_arrives() {
        let mut decoder = Decoder::new();
        assert!(decoder.feed(b"\x1b").is_empty());
        assert!(decoder.feed(b"[").is_empty());
        assert_eq!(decoder.pending().len(), 2);
        assert!(decoder.feed(b"A").is_empty());
        assert!(decoder.held().is_down(Key::LookUp));
        assert!(decoder.pending().is_empty());
    }

    #[test]
    fn movement_keys_set_axes() {
        let mut decoder = Decoder::new();
        decoder.feed(b"w");
        assert!((decoder.camera_input().forward - 1.0).abs() < f32::EPSILON);
        decoder.feed(b"s");
        assert!(decoder.camera_input().forward.abs() < f32::EPSILON);
        decoder.feed(b"d");
        assert!((decoder.camera_input().strafe - 1.0).abs() < f32::EPSILON);
        decoder.release_all();
        assert!(decoder.camera_input().is_idle());
    }

    #[test]
    fn view_keys_count_from_one() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(b"3"), vec![Command::View(3)]);
        assert_eq!(decoder.feed(b"1"), vec![Command::View(1)]);
    }

    #[test]
    fn q_and_ctrl_c_and_a_lone_escape_all_quit() {
        for bytes in [b"q".as_slice(), b"\x03".as_slice(), b"\x1b".as_slice()] {
            let mut decoder = Decoder::new();
            let commands = decoder.feed(bytes);
            // A lone escape is ambiguous until the next read, so it takes a second byte that cannot
            // continue a sequence.
            if bytes == b"\x1b" {
                assert!(commands.is_empty());
                assert_eq!(decoder.feed(b"x"), vec![Command::Quit]);
            } else {
                assert_eq!(commands, vec![Command::Quit], "{bytes:?}");
            }
        }
    }

    #[test]
    fn view_commands_do_not_stick_as_held_keys() {
        let mut decoder = Decoder::new();
        decoder.feed(b"p+-[]");
        assert!(decoder.camera_input().is_idle());
        assert!(!decoder.held().is_down(Key::Forward));
    }

    #[test]
    fn an_unknown_byte_is_ignored() {
        let mut decoder = Decoder::new();
        assert!(decoder.feed(b"z\t\x7f").is_empty());
        assert!(decoder.pending().is_empty());
    }
}
