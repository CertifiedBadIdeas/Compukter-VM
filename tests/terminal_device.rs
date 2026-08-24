/*
 * The Compukters Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

use compukter_vm::{
    TerminalCell, TerminalChange, TerminalCommit, TerminalConfig, TerminalDevice,
    TerminalInputEvent, TerminalInputLimits, TerminalKey, TerminalKeyAction, TerminalKeyEvent,
    TerminalModifiers, TerminalPosition, TerminalRectangle, TerminalUpdate, TERMINAL_HEIGHT,
    TERMINAL_PALETTE_SIZE, TERMINAL_WIDTH,
};

#[test]
fn terminal_starts_as_a_blank_fixed_grid() {
    let terminal = TerminalDevice::default();

    assert_eq!((51, 19), terminal.dimensions());
    assert_eq!(51, TERMINAL_WIDTH);
    assert_eq!(19, TERMINAL_HEIGHT);
    assert_eq!(16, TERMINAL_PALETTE_SIZE);
    assert_eq!(
        TerminalPosition::new(0, 0).unwrap(),
        terminal.cursor_position()
    );
    assert!(terminal.cursor_visible());
    for y in 0..TERMINAL_HEIGHT {
        for x in 0..TERMINAL_WIDTH {
            assert_eq!(TerminalCell::default(), terminal.cell(x, y).unwrap());
        }
    }
}

#[test]
fn public_values_validate_bounds_unicode_scalars_and_palette_indices() {
    assert!(TerminalPosition::new(50, 18).is_ok());
    assert!(TerminalPosition::new(51, 0).is_err());
    assert!(TerminalPosition::new(0, 19).is_err());
    assert!(TerminalRectangle::new(0, 0, 51, 19).is_ok());
    assert!(TerminalRectangle::new(50, 18, 2, 1).is_err());

    assert!(TerminalCell::new('A' as u32, 15, 0).is_ok());
    assert!(TerminalCell::new(0x10ffff, 0, 15).is_ok());
    assert!(TerminalCell::new(0xd800, 15, 0).is_err());
    assert!(TerminalCell::new(0x11_0000, 15, 0).is_err());
    assert!(TerminalCell::new('A' as u32, 16, 0).is_err());
    assert!(TerminalCell::new('A' as u32, 0, 16).is_err());
}

#[test]
fn positional_patch_and_fill_do_not_move_the_stream_cursor() {
    let mut terminal = TerminalDevice::default();
    let cursor = TerminalPosition::new(7, 8).unwrap();
    terminal.set_cursor(cursor);
    let red = TerminalCell::new('R' as u32, 14, 0).unwrap();
    let green = TerminalCell::new('G' as u32, 13, 0).unwrap();

    terminal
        .fill(TerminalRectangle::new(2, 3, 2, 2).unwrap(), red)
        .unwrap();
    terminal
        .patch(TerminalPosition::new(50, 3).unwrap(), &[green, red])
        .unwrap();

    assert_eq!(red, terminal.cell(2, 3).unwrap());
    assert_eq!(red, terminal.cell(3, 4).unwrap());
    assert_eq!(green, terminal.cell(50, 3).unwrap());
    assert_eq!(red, terminal.cell(0, 4).unwrap());
    assert_eq!(cursor, terminal.cursor_position());
    assert!(terminal
        .patch(TerminalPosition::new(50, 18).unwrap(), &[red, green])
        .is_err());
}

#[test]
fn stream_writes_wrap_newline_and_scroll_the_ring_rows() {
    let mut terminal = TerminalDevice::default();
    terminal.set_cursor(TerminalPosition::new(50, 18).unwrap());

    terminal.write_utf16(&['A' as u16, 'B' as u16]).unwrap();

    assert_eq!('A' as u32, terminal.cell(50, 17).unwrap().code_point());
    assert_eq!('B' as u32, terminal.cell(0, 18).unwrap().code_point());
    assert_eq!(
        TerminalPosition::new(1, 18).unwrap(),
        terminal.cursor_position()
    );

    terminal.write_utf16(&['\n' as u16, 'C' as u16]).unwrap();
    assert_eq!('B' as u32, terminal.cell(0, 17).unwrap().code_point());
    assert_eq!('C' as u32, terminal.cell(0, 18).unwrap().code_point());
}

#[test]
fn stream_writes_replace_each_malformed_utf16_sequence() {
    let mut terminal = TerminalDevice::default();
    terminal
        .write_utf16(&[0xd83d, 0xde00, 0xd800, 'A' as u16, 0xdc00])
        .unwrap();

    assert_eq!(0x1f600, terminal.cell(0, 0).unwrap().code_point());
    assert_eq!(0xfffd, terminal.cell(1, 0).unwrap().code_point());
    assert_eq!('A' as u32, terminal.cell(2, 0).unwrap().code_point());
    assert_eq!(0xfffd, terminal.cell(3, 0).unwrap().code_point());
}

#[test]
fn erase_previous_blanks_one_logical_cell_across_wrapped_rows() {
    let mut terminal = TerminalDevice::default();
    terminal.set_cursor(TerminalPosition::new(50, 0).unwrap());
    terminal.write_utf16(&['A' as u16, 0xd83d, 0xde00]).unwrap();
    terminal.erase_previous();

    assert_eq!(TerminalCell::default(), terminal.cell(0, 1).unwrap());
    assert_eq!(
        TerminalPosition::new(0, 1).unwrap(),
        terminal.cursor_position()
    );

    terminal.erase_previous();
    assert_eq!(TerminalCell::default(), terminal.cell(50, 0).unwrap());
    assert_eq!(
        TerminalPosition::new(50, 0).unwrap(),
        terminal.cursor_position()
    );
}

#[test]
fn clear_resets_cells_cursor_and_replication_as_one_authoritative_change() {
    let mut terminal = TerminalDevice::default();
    terminal.write_utf16(&['A' as u16]).unwrap();
    terminal.commit();

    terminal.clear();

    assert_eq!(
        TerminalPosition::new(0, 0).unwrap(),
        terminal.cursor_position()
    );
    assert_eq!(TerminalCell::default(), terminal.cell(0, 0).unwrap());
    let TerminalCommit::Committed(delta) = terminal.commit() else {
        panic!("clear must commit");
    };
    assert_eq!([TerminalChange::Reset], delta.changes());
}

#[test]
fn explicit_scroll_keeps_logical_coordinates_and_clears_reclaimed_rows() {
    let mut terminal = TerminalDevice::default();
    let marked = TerminalCell::new('X' as u32, 1, 2).unwrap();
    terminal
        .fill(TerminalRectangle::new(0, 0, 51, 2).unwrap(), marked)
        .unwrap();

    terminal.scroll(1).unwrap();

    assert_eq!(marked, terminal.cell(0, 0).unwrap());
    assert_eq!(TerminalCell::default(), terminal.cell(0, 1).unwrap());
    assert_eq!(TerminalCell::default(), terminal.cell(0, 18).unwrap());
    assert!(terminal.scroll(20).is_err());
}

#[test]
fn borrowed_logical_cells_follow_visible_row_order_after_scroll() {
    let mut terminal = TerminalDevice::default();
    let first = TerminalCell::new('A' as u32, 1, 2).unwrap();
    let second = TerminalCell::new('B' as u32, 3, 4).unwrap();
    terminal
        .fill(
            TerminalRectangle::new(0, 0, TERMINAL_WIDTH, 1).unwrap(),
            first,
        )
        .unwrap();
    terminal
        .fill(
            TerminalRectangle::new(0, 1, TERMINAL_WIDTH, 1).unwrap(),
            second,
        )
        .unwrap();

    terminal.scroll(1).unwrap();

    let cells = terminal.logical_cells().collect::<Vec<_>>();
    assert_eq!(
        TERMINAL_WIDTH as usize * TERMINAL_HEIGHT as usize,
        cells.len()
    );
    assert!(cells[..TERMINAL_WIDTH as usize]
        .iter()
        .all(|cell| *cell == second));
    assert!(cells[cells.len() - TERMINAL_WIDTH as usize..]
        .iter()
        .all(|cell| *cell == TerminalCell::default()));
}

#[test]
fn stable_key_and_atomic_text_events_merge_in_fifo_order() {
    assert_eq!(13, TerminalKey::Enter.code());
    assert_eq!(TerminalKey::Enter, TerminalKey::try_from(13).unwrap());
    assert!(TerminalKey::try_from(u16::MAX).is_err());
    let mut terminal = TerminalDevice::with_config(TerminalConfig {
        input: TerminalInputLimits::new(3, 4).unwrap(),
        journal_revisions: 4,
    })
    .unwrap();
    let key = TerminalKeyEvent::new(
        TerminalKey::Enter,
        TerminalKeyAction::Press,
        TerminalModifiers::new(TerminalModifiers::SHIFT).unwrap(),
    );

    terminal.push_key(key).unwrap();
    terminal.push_text("😀ab").unwrap();
    terminal
        .push_key(TerminalKeyEvent::new(
            TerminalKey::Left,
            TerminalKeyAction::Repeat,
            TerminalModifiers::default(),
        ))
        .unwrap();

    assert_eq!(Some(TerminalInputEvent::Key(key)), terminal.poll_input());
    assert_eq!(
        Some(TerminalInputEvent::Text("😀ab".into())),
        terminal.poll_input()
    );
    assert!(matches!(
        terminal.poll_input(),
        Some(TerminalInputEvent::Key(event)) if event.action() == TerminalKeyAction::Repeat
    ));
    assert_eq!(None, terminal.poll_input());
}

#[test]
fn input_limits_reject_whole_events_without_partial_queue_mutation() {
    let mut terminal = TerminalDevice::with_config(TerminalConfig {
        input: TerminalInputLimits::new(1, 2).unwrap(),
        journal_revisions: 1,
    })
    .unwrap();

    assert!(terminal.push_text("abc").is_err());
    terminal.push_text("ab").unwrap();
    assert!(terminal
        .push_key(TerminalKeyEvent::new(
            TerminalKey::Escape,
            TerminalKeyAction::Press,
            TerminalModifiers::default(),
        ))
        .is_err());
    assert_eq!(
        Some(TerminalInputEvent::Text("ab".into())),
        terminal.poll_input()
    );
    assert_eq!(None, terminal.poll_input());
}

#[test]
fn commit_coalesces_adjacent_patches_and_advances_one_revision_per_batch() {
    let mut terminal = TerminalDevice::default();
    let a = TerminalCell::new('A' as u32, 15, 0).unwrap();
    let b = TerminalCell::new('B' as u32, 15, 0).unwrap();
    assert_eq!(TerminalCommit::Unchanged { revision: 0 }, terminal.commit());

    terminal
        .patch(TerminalPosition::new(0, 0).unwrap(), &[a])
        .unwrap();
    terminal
        .patch(TerminalPosition::new(1, 0).unwrap(), &[b])
        .unwrap();
    let TerminalCommit::Committed(delta) = terminal.commit() else {
        panic!("mutations must commit");
    };

    assert_eq!(0, delta.base_revision());
    assert_eq!(1, delta.target_revision());
    assert_eq!(1, delta.changes().len());
    assert!(matches!(
        &delta.changes()[0],
        TerminalChange::Patch { start: 0, cells } if cells.as_ref() == [a, b]
    ));
    assert_eq!(TerminalCommit::Unchanged { revision: 1 }, terminal.commit());
    assert_eq!(
        TerminalUpdate::Unchanged { revision: 1 },
        terminal.changes_since(1)
    );
}

#[test]
fn replication_preserves_scroll_order_and_resyncs_after_journal_eviction() {
    let mut terminal = TerminalDevice::with_config(TerminalConfig {
        input: TerminalInputLimits::default(),
        journal_revisions: 1,
    })
    .unwrap();
    terminal.set_cursor(TerminalPosition::new(50, 18).unwrap());
    terminal.commit();

    terminal.write_utf16(&['A' as u16, 'B' as u16]).unwrap();
    let TerminalCommit::Committed(second) = terminal.commit() else {
        panic!("stream write must commit");
    };
    assert!(matches!(second.changes()[0], TerminalChange::Patch { .. }));
    assert!(matches!(
        second.changes()[1],
        TerminalChange::Scroll { rows: 1, .. }
    ));
    assert!(matches!(second.changes()[2], TerminalChange::Patch { .. }));
    assert!(matches!(
        terminal.changes_since(1),
        TerminalUpdate::Delta(delta) if delta.target_revision() == 2
    ));

    let TerminalUpdate::Full(snapshot) = terminal.changes_since(0) else {
        panic!("evicted base revision must receive a full state");
    };
    assert_eq!(2, snapshot.revision());
    assert_eq!(
        TERMINAL_WIDTH as usize * TERMINAL_HEIGHT as usize,
        snapshot.cells().len()
    );
    assert_eq!(
        'A' as u32,
        snapshot.cells()[17 * TERMINAL_WIDTH as usize + 50].code_point()
    );
    assert_eq!(
        'B' as u32,
        snapshot.cells()[18 * TERMINAL_WIDTH as usize].code_point()
    );
}

#[test]
fn oversized_pending_journal_compacts_to_one_bounded_full_replacement() {
    let mut terminal = TerminalDevice::default();
    let cell = TerminalCell::new('Z' as u32, 3, 4).unwrap();
    for _ in 0..1_024 {
        terminal
            .patch(TerminalPosition::new(0, 0).unwrap(), &[cell])
            .unwrap();
    }

    let TerminalCommit::Committed(delta) = terminal.commit() else {
        panic!("mutations must commit");
    };

    assert_eq!(3, delta.changes().len());
    assert!(matches!(delta.changes()[0], TerminalChange::Reset));
    assert!(matches!(
        &delta.changes()[1],
        TerminalChange::Patch { start: 0, cells }
            if cells.len() == TERMINAL_WIDTH as usize * TERMINAL_HEIGHT as usize
                && cells[0] == cell
    ));
    assert!(matches!(delta.changes()[2], TerminalChange::Cursor { .. }));
}

#[test]
fn oversized_accumulated_journal_resyncs_with_a_bounded_full_state() {
    let mut terminal = TerminalDevice::default();
    let cell = TerminalCell::new('Z' as u32, 3, 4).unwrap();
    for _ in 0..17 {
        for _ in 0..256 {
            terminal
                .patch(TerminalPosition::new(0, 0).unwrap(), &[cell])
                .unwrap();
        }
        terminal.commit();
    }

    assert!(matches!(
        terminal.changes_since(0),
        TerminalUpdate::Full(snapshot) if snapshot.revision() == 17
    ));

    let mut cell_heavy = TerminalDevice::default();
    let full_grid = vec![cell; 51 * 19];
    for _ in 0..9 {
        cell_heavy
            .patch(TerminalPosition::new(0, 0).unwrap(), &full_grid)
            .unwrap();
        cell_heavy.commit();
    }
    assert!(matches!(
        cell_heavy.changes_since(0),
        TerminalUpdate::Full(snapshot) if snapshot.revision() == 9
    ));
}
