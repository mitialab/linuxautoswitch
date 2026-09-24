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
    let status = Command::new("wtype")
        .args(wtype_args(backspace_count, replacement))
        .status()
        .context("failed to run `wtype` - is it installed and on PATH?")?;
    if !status.success() {
        bail!("wtype exited with {status}");
    }
    Ok(())
}

fn wtype_args(backspace_count: usize, replacement: &str) -> Vec<&str> {
    let mut args = Vec::with_capacity(backspace_count * 2 + 1);
    for _ in 0..backspace_count {
        args.extend(["-k", "BackSpace"]);
    }
    if !replacement.is_empty() {
        args.push(replacement);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletes_then_types() {
        assert_eq!(
            wtype_args(2, "ok "),
            ["-k", "BackSpace", "-k", "BackSpace", "ok "]
        );
    }

    #[test]
    fn empty_replacement_only_deletes() {
        assert_eq!(wtype_args(1, ""), ["-k", "BackSpace"]);
    }
}
