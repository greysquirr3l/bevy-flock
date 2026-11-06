//! A picture in a terminal: raw mode on the way in, half blocks on the way out.
//!
//! There is no window here and no widget toolkit. A terminal can show a picture if each cell is
//! split in two by the upper-half block `▀`: the foreground colour paints the top pixel and the
//! background colour paints the bottom one, so an image of `cols` by `rows * 2` pixels fits a grid
//! of `cols` by `rows` cells exactly, with square pixels.
//!
//! Two things make this usable rather than a novelty: colour sequences are only emitted when the
//! colour changes (a solid sky costs one sequence per run, not per cell), and raw mode is put back
//! by a guard's `Drop`, so a panic still leaves the shell working.

use std::io::{self, Write};

use crate::raster::Image;

/// The character that splits a cell into two pixels.
pub const HALF_BLOCK: char = '▀';

/// Escape sequence: move the cursor home and clear to the end of the screen.
const HOME: &str = "\x1b[H\x1b[J";

/// Escape sequence: reset every attribute.
const RESET: &str = "\x1b[0m";

/// A terminal in raw mode for as long as the value lives.
///
/// Raw mode means every keystroke arrives immediately, un-echoed, and `Ctrl-C` arrives as a byte
/// instead of killing the process. Both are what an interactive loop needs; both are undone on drop
/// by running `stty` with the settings that were saved on the way in.
#[derive(Debug)]
pub struct RawTerminal {
    saved: String,
    active: bool,
}

impl RawTerminal {
    /// Put the terminal into raw mode.
    ///
    /// # Errors
    ///
    /// Returns an error when `stty` is missing or refuses, which is the case when stdin is not a
    /// terminal — in a pipe or a CI job, for instance.
    pub fn enter() -> Result<Self, io::Error> {
        let saved = run_stty(&["-g"])?;
        let saved = saved.trim().to_owned();
        if saved.is_empty() {
            return Err(io::Error::other(
                "stty could not read the terminal settings",
            ));
        }
        // `raw` is the combination the loop wants: no line discipline, no signals, no echo. `-echo`
        // is implied but spelled out because it is the one that matters most if `raw` is ever
        // replaced by something narrower.
        run_stty(&["raw", "-echo"])?;
        Ok(Self {
            saved,
            active: true,
        })
    }

    /// Write `text` to stdout and flush, which is what a frame is.
    ///
    /// # Errors
    ///
    /// Returns whatever writing to stdout returns: a closed pipe, most likely.
    pub fn draw(text: &str) -> Result<(), io::Error> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(text.as_bytes())?;
        stdout.flush()
    }

    /// Put the terminal back as it was found. Idempotent, and also run by `Drop`.
    pub fn restore(&mut self) {
        if self.active {
            self.active = false;
            let _ = run_stty(&[self.saved.as_str()]);
        }
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Run `stty` with `arguments`, returning its stdout.
fn run_stty(arguments: &[&str]) -> Result<String, io::Error> {
    let output = std::process::Command::new("stty")
        .args(arguments)
        .stdin(std::process::Stdio::inherit())
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "stty {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The terminal's size in cells, as `(columns, rows)`.
///
/// # Errors
///
/// Returns an error when `stty` cannot run or does not answer with two numbers, which is what
/// happens when the process has no controlling terminal.
pub fn size() -> Result<(u32, u32), io::Error> {
    parse_size(&run_stty(&["size"])?)
}

/// Read `stty size` output, which is `rows columns`.
///
/// # Errors
///
/// Returns an error when either number is missing or zero: a zero-column terminal is not something
/// to render into.
pub fn parse_size(text: &str) -> Result<(u32, u32), io::Error> {
    let mut numbers = text.split_whitespace();
    let rows = numbers
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0);
    let columns = numbers
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0);
    match (rows, columns) {
        (Some(rows), Some(columns)) => Ok((columns, rows)),
        _ => Err(io::Error::other(format!(
            "could not read a terminal size from {text:?}"
        ))),
    }
}

/// Paint `image` as half blocks, with `status` on a reverse-video line beneath it.
///
/// The image's height is halved: two pixels share a cell. An odd height leaves the last row's bottom
/// pixel unpainted, which the renderer never asks for because the loop renders `2 * rows`.
#[must_use]
pub fn paint(image: &Image, status: &str) -> String {
    let mut out = String::with_capacity(image.width as usize * image.height as usize * 8);
    out.push_str(HOME);
    let mut last_foreground: Option<[u8; 3]> = None;
    let mut last_background: Option<[u8; 3]> = None;
    for row in 0..image.height.div_ceil(2) {
        for column in 0..image.width {
            let top = image.pixel(column, row * 2);
            let bottom = if row * 2 + 1 < image.height {
                image.pixel(column, row * 2 + 1)
            } else {
                [0, 0, 0]
            };
            if last_foreground != Some(top) {
                push_colour(&mut out, top, true);
                last_foreground = Some(top);
            }
            if last_background != Some(bottom) {
                push_colour(&mut out, bottom, false);
                last_background = Some(bottom);
            }
            out.push(HALF_BLOCK);
        }
        out.push_str(RESET);
        last_foreground = None;
        last_background = None;
        out.push_str("\r\n");
    }
    out.push_str(RESET);
    out.push_str("\x1b[7m");
    out.push_str(status);
    out.push_str(RESET);
    out
}

/// Append the escape sequence that selects `colour` as the foreground or the background.
fn push_colour(out: &mut String, colour: [u8; 3], foreground: bool) {
    use std::fmt::Write as _;
    let layer = if foreground { 38 } else { 48 };
    let _ = write!(
        out,
        "\x1b[{layer};2;{};{};{}m",
        colour.first().copied().unwrap_or(0),
        colour.get(1).copied().unwrap_or(0),
        colour.get(2).copied().unwrap_or(0)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn striped(width: u32, height: u32) -> Image {
        let mut image = Image::new(width, height);
        image.sky([0.9, 0.1, 0.1], [0.1, 0.1, 0.9]);
        image
    }

    #[test]
    fn two_pixels_share_a_cell() {
        let image = striped(4, 6);
        let painted = paint(&image, "status");
        // Three cell rows for six pixel rows, plus the status line the caller adds.
        let rows: Vec<&str> = painted.split("\r\n").collect();
        assert_eq!(rows.len(), 4, "{rows:?}");
        // The last entry is whatever followed the final newline, not a painted row.
        for row in rows.iter().take(3) {
            assert_eq!(row.matches(HALF_BLOCK).count(), 4);
        }
    }

    #[test]
    fn the_top_pixel_is_the_foreground() {
        let mut image = Image::new(1, 2);
        image.sky([1.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
        let top = image.pixel(0, 0);
        let bottom = image.pixel(0, 1);
        let painted = paint(&image, "");
        let foreground = format!("\x1b[38;2;{};{};{}m", top[0], top[1], top[2]);
        let background = format!("\x1b[48;2;{};{};{}m", bottom[0], bottom[1], bottom[2]);
        let foreground_at = painted.find(&foreground);
        let background_at = painted.find(&background);
        assert!(foreground_at.is_some(), "no foreground sequence");
        assert!(background_at.is_some(), "no background sequence");
        assert!(foreground_at.unwrap_or(0) < background_at.unwrap_or(0));
    }

    #[test]
    fn a_flat_region_costs_one_colour_sequence() {
        let image = striped(1, 200);
        // One column, a hundred cells, a gradient: the sequences must track colour changes rather
        // than cells, so a uniform image is cheap and a gradient is bounded by its rows.
        let painted = paint(&image, "");
        let foregrounds = painted.matches("\x1b[38;2;").count();
        let backgrounds = painted.matches("\x1b[48;2;").count();
        assert!(foregrounds <= 100, "{foregrounds} foreground sequences");
        assert!(backgrounds <= 100, "{backgrounds} background sequences");
        let flat = Image::new(8, 8);
        let painted = paint(&flat, "");
        assert_eq!(painted.matches("\x1b[38;2;").count(), 4);
    }

    #[test]
    fn the_status_line_is_reverse_video_and_last() {
        let painted = paint(&striped(2, 2), "district 12 ms");
        assert!(painted.contains("\x1b[7mdistrict 12 ms"));
        assert!(painted.ends_with(RESET));
    }

    #[test]
    fn sizes_are_read_as_columns_and_rows() {
        assert_eq!(parse_size("40 120\n").ok(), Some((120, 40)));
        assert_eq!(parse_size("  24   80 ").ok(), Some((80, 24)));
        assert!(parse_size("0 80").is_err());
        assert!(parse_size("").is_err());
        assert!(parse_size("80").is_err());
    }

    #[test]
    fn an_odd_height_does_not_panic() {
        let painted = paint(&striped(3, 3), "");
        assert_eq!(painted.matches(HALF_BLOCK).count(), 6);
    }
}
