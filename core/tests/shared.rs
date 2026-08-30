// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

#[test]
fn overlay_gives_every_claimant_contested_cells() {
    // Contention only in even columns catches the tempting `x % claims.len()`
    // implementation: it would choose the same owner for both cells.
    let first = vec![vec![0x01, 0, 0x01, 0]];
    let second = vec![vec![0x40, 0, 0x40, 0]];
    let cells = opscope_core::overlay(
        &[("first".to_string(), first), ("second".to_string(), second)],
        4,
        1,
    );
    let owners: Vec<&str> = cells[0]
        .iter()
        .filter(|(_, dots)| *dots != 0)
        .map(|(owner, _)| owner.as_str())
        .collect();

    assert_eq!(owners.len(), 2);
    assert!(owners.contains(&"first"), "{owners:?}");
    assert!(owners.contains(&"second"), "{owners:?}");
}

#[test]
fn unsupported_names_this_kernel() {
    // The wording every widget uses when it has no source here. The OS
    // name is rustc's, so a macOS build says `macos` and a Linux one
    // says `linux` — not a string that was true on the machine that
    // compiled it.
    let got = opscope_core::unsupported();
    assert!(
        got.starts_with("does not run on "),
        "unsupported() drifted from the agreed wording: {got:?}"
    );
    assert!(
        got.ends_with(std::env::consts::OS),
        "unsupported() must name this target, not a hardcoded one: {got:?}"
    );
}

#[test]
fn shared_glyph_tables_keep_their_cell_geometry() {
    assert_eq!(opscope_core::SPARK.len(), 8);
    assert_eq!(opscope_core::SPINNER.len(), 10);
    assert_eq!(
        opscope_core::BRAILLE
            .iter()
            .flatten()
            .fold(0u8, |all, dot| all | dot),
        u8::MAX
    );
}
