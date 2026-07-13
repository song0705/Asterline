//! Terminal notifications: BEL + OSC 9 so terminals (iTerm2, kitty, WezTerm)
//! can surface attention-needed moments while Asterline is unfocused.

/// Write BEL + an OSC 9 notification. Pure: the caller decides enabled-ness.
pub fn emit(out: &mut impl std::io::Write, title: &str) -> std::io::Result<()> {
    let title = sanitize_title(title);
    out.write_all(b"\x07")?;
    out.write_all(b"\x1b]9;")?;
    out.write_all(title.as_bytes())?;
    out.write_all(b"\x07")?;
    Ok(())
}

/// `ASTERLINE_NO_BELL` unset or empty → true; any value → false.
pub fn enabled_from_env() -> bool {
    match std::env::var("ASTERLINE_NO_BELL") {
        Ok(value) => value.is_empty(),
        Err(_) => true,
    }
}

fn sanitize_title(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect();
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_writes_bel_and_osc9() {
        let mut buf = Vec::new();
        emit(&mut buf, "Asterline: approval needed").unwrap();
        assert_eq!(buf, b"\x07\x1b]9;Asterline: approval needed\x07".as_slice());
    }

    #[test]
    fn sanitize_strips_controls_and_clamps_length() {
        let mut buf = Vec::new();
        emit(&mut buf, "hi\x00there\x1b!").unwrap();
        assert_eq!(buf, b"\x07\x1b]9;hithere!\x07".as_slice());

        let long = "x".repeat(200);
        let mut buf = Vec::new();
        emit(&mut buf, &long).unwrap();
        // BEL + OSC open + 120 chars + BEL terminator
        assert_eq!(buf.len(), 1 + 4 + 120 + 1);
        assert!(buf.starts_with(b"\x07\x1b]9;"));
        assert!(buf.ends_with(b"\x07"));
    }
}
