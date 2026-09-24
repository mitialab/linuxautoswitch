//! Applies a correction to already-typed text using `wtype`.
//!
//! `wtype` drives Wayland's `virtual-keyboard-unstable-v1` protocol, which
//! Hyprland supports natively. That's deliberate: it means the backspaces
//! and retyped characters we synthesize never come back through the
//! physical `/dev/input/eventN` devices this tool reads from, so there's no
//! risk of feeding our own corrections back into the word buffer.

use anyhow::{Context, Result, bail};
use std::process::Command;

/// Deletes `backspace_count` characters before the cursor, then types
/// `replacement`.
pub fn correct_word(backspace_count: usize, replacement: &str) -> Result<()> {
    let mut cmd = Command::new("wtype");
    for _ in 0..backspace_count {
        cmd.arg("-k").arg("BackSpace");
    }
    if !replacement.is_empty() {
        cmd.arg(replacement);
    }
    let status = cmd
        .status()
        .context("failed to run `wtype` - is it installed and on PATH?")?;
    if !status.success() {
        bail!("wtype exited with {status}");
    }
    Ok(())
}
