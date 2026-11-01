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
/// `replacement`, pausing `delay_before_retype_ms` in between if non-zero.
pub fn correct_word(
    backspace_count: usize,
    replacement: &str,
    delay_before_retype_ms: u64,
) -> Result<()> {
    let status = Command::new("wtype")
        .args(wtype_args(
            backspace_count,
            replacement,
            delay_before_retype_ms,
        ))
        .status()
        .context("failed to run `wtype` - is it installed and on PATH?")?;
    if !status.success() {
        bail!("wtype exited with {status}");
    }
    Ok(())
}

fn wtype_args(
    backspace_count: usize,
    replacement: &str,
    delay_before_retype_ms: u64,
) -> Vec<String> {
    let mut args = Vec::with_capacity(backspace_count * 2 + 3);
    for _ in 0..backspace_count {
        args.extend(["-k", "BackSpace"].map(String::from));
    }
    if !replacement.is_empty() {
        if backspace_count > 0 && delay_before_retype_ms > 0 {
            args.extend(["-s".to_string(), delay_before_retype_ms.to_string()]);
        }
        args.push(replacement.to_string());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletes_then_types() {
        assert_eq!(
            wtype_args(2, "ok ", 0),
            ["-k", "BackSpace", "-k", "BackSpace", "ok "]
        );
    }

    #[test]
    fn empty_replacement_only_deletes() {
        assert_eq!(wtype_args(1, "", 0), ["-k", "BackSpace"]);
        // Nothing follows the Backspaces, so there is nothing to wait for.
        assert_eq!(wtype_args(1, "", 40), ["-k", "BackSpace"]);
    }

    #[test]
    fn delay_sits_between_the_backspaces_and_the_word() {
        assert_eq!(
            wtype_args(1, "ok", 40),
            ["-k", "BackSpace", "-s", "40", "ok"]
        );
    }

    #[test]
    fn no_delay_when_there_is_nothing_to_delete() {
        assert_eq!(wtype_args(0, "ok", 40), ["ok"]);
    }
}
