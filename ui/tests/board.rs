use adjourn_core::{project, GameParams, GameState};
use adjourn_ui::board::{squares, Marker, Shade};
use shakmaty::{Color, Role};

fn start() -> (GameState, GameParams) {
    let w = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
    let b = ed25519_dalek::SigningKey::from_bytes(&[2u8; 32]);
    let params = GameParams {
        white: w.verifying_key().to_bytes(),
        black: b.verifying_key().to_bytes(),
        nonce: [7u8; 16],
    };
    (GameState::empty(), params)
}

#[test]
fn the_opening_position_is_laid_out_correctly() {
    let (state, params) = start();
    let status = project(&state, &params);
    let board = squares(&status, Color::White, None);

    assert_eq!(board.len(), 64, "a board has 64 squares");

    // White's view reads a8 first and h1 last.
    assert_eq!((board[0].file, board[0].rank), ('a', 8));
    assert_eq!((board[63].file, board[63].rank), ('h', 1));

    assert_eq!(
        board[0].piece,
        Some((Color::Black, Role::Rook)),
        "a8 is a black rook"
    );
    assert_eq!(
        board[63].piece,
        Some((Color::White, Role::Rook)),
        "h1 is a white rook"
    );

    // a8 is a light square; h1 is light too.
    assert_eq!(board[0].shade, Shade::Light);
    assert_eq!(board[63].shade, Shade::Light);
}

#[test]
fn black_sees_the_board_from_the_other_side() {
    let (state, params) = start();
    let status = project(&state, &params);
    let board = squares(&status, Color::Black, None);

    assert_eq!(
        (board[0].file, board[0].rank),
        ('h', 1),
        "black reads h1 first"
    );
    assert_eq!((board[63].file, board[63].rank), ('a', 8));

    // Orientation must not recolour a square: a8 is light from either side.
    let a8 = board
        .iter()
        .find(|s| (s.file, s.rank) == ('a', 8))
        .expect("a8");
    assert_eq!(
        a8.shade,
        Shade::Light,
        "shade is a property of the square, not the viewer"
    );
    assert_eq!(a8.piece, Some((Color::Black, Role::Rook)));
}

#[test]
fn selecting_a_piece_marks_its_legal_targets() {
    let (state, params) = start();
    let status = project(&state, &params);
    let board = squares(&status, Color::White, Some(('e', 2)));

    let selected: Vec<_> = board
        .iter()
        .filter(|s| s.marker == Marker::Selected)
        .collect();
    assert_eq!(selected.len(), 1, "exactly the selected square is marked");
    assert_eq!((selected[0].file, selected[0].rank), ('e', 2));

    let targets: Vec<(char, u8)> = board
        .iter()
        .filter(|s| s.marker == Marker::LegalTarget)
        .map(|s| (s.file, s.rank))
        .collect();
    assert_eq!(targets.len(), 2, "the e2 pawn has exactly two legal moves");
    assert!(targets.contains(&('e', 3)));
    assert!(targets.contains(&('e', 4)));
}

#[test]
fn selecting_a_square_with_no_legal_moves_marks_no_targets() {
    let (state, params) = start();
    let status = project(&state, &params);
    // a1 holds a rook that is completely blocked in the opening position.
    let board = squares(&status, Color::White, Some(('a', 1)));

    assert_eq!(
        board
            .iter()
            .filter(|s| s.marker == Marker::LegalTarget)
            .count(),
        0,
        "a blocked rook offers nothing"
    );
}

#[test]
fn nothing_is_marked_when_nothing_is_selected() {
    let (state, params) = start();
    let status = project(&state, &params);
    let board = squares(&status, Color::White, None);
    assert!(board.iter().all(|s| s.marker == Marker::None));
}

/// A UI that only ever queens cannot play some legal games, so the picker has
/// to know when to appear.
#[test]
fn promotion_is_detected_only_for_a_pawn_reaching_the_last_rank() {
    let (mut state, params) = start();
    let w = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
    let b = ed25519_dalek::SigningKey::from_bytes(&[2u8; 32]);

    // A line that leaves a white pawn on b7 with a promotion available.
    for (i, uci) in [
        "b2b4", "a7a5", "b4a5", "b7b6", "a5b6", "h7h6", "b6b7", "h6h5",
    ]
    .iter()
    .enumerate()
    {
        let key = if i % 2 == 0 { &w } else { &b };
        let rec = adjourn_core::make_move(&state, &params, key, uci)
            .unwrap_or_else(|| panic!("move {} ({uci}) rejected", i + 1));
        assert!(state.insert_verified(&rec, &params));
    }
    let status = project(&state, &params);

    assert!(
        adjourn_ui::board::is_promotion(&status, ('b', 7), ('b', 8)),
        "a pawn reaching the last rank promotes"
    );
    assert!(
        !adjourn_ui::board::is_promotion(&status, ('h', 2), ('h', 3)),
        "an ordinary pawn push does not"
    );
}
