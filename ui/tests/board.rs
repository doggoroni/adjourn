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

/// A decided game offers nothing to click. Otherwise the board would invite a
/// move the projection will ignore, and the record would be permanent.
///
/// Deliberately ends the game by **resignation**, not checkmate. A mated
/// position already has zero legal moves for the side to move, so a
/// checkmate-based version of this test passes whether or not the
/// game-over gate in `squares` exists -- selecting a piece of the side NOT
/// to move yields no targets either way, for reasons having nothing to do
/// with `status.decision`. Resignation is unconditional and unanchored (see
/// invariant 7): it ends the game while the raw position still has an
/// ordinary set of legal moves available, so this is the version that
/// actually exercises the `_ => Vec::new()` suppression arm in `squares`.
#[test]
fn a_finished_game_marks_no_legal_targets() {
    let (mut state, params) = start();
    let w = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
    let b = ed25519_dalek::SigningKey::from_bytes(&[2u8; 32]);
    for (i, uci) in ["e2e4", "e7e5"].iter().enumerate() {
        let key = if i % 2 == 0 { &w } else { &b };
        let rec = adjourn_core::make_move(&state, &params, key, uci)
            .unwrap_or_else(|| panic!("move {} ({uci}) rejected", i + 1));
        assert!(state.insert_verified(&rec, &params));
    }
    // Black resigns despite an ordinary, wide-open position: the board still
    // has plenty of legal moves, but the game is decided.
    let resign = adjourn_core::Record::sign(&b, &params, adjourn_core::Body::Resign);
    assert!(state.insert_verified(&resign, &params));

    let status = project(&state, &params);
    assert!(status.is_over(), "resignation ends the game");

    // White's g1 knight has real legal moves (Nf3, Nh3) in this position.
    let board = squares(&status, Color::White, Some(('g', 1)));
    assert_eq!(
        board
            .iter()
            .filter(|s| s.marker == Marker::LegalTarget)
            .count(),
        0,
        "a decided game offers no targets"
    );
}

/// The white arm of `is_promotion` is covered above by a pawn reaching rank 8.
/// This covers the black arm: a black pawn reaching rank 1 via legal play,
/// mirroring the white promotion line's shape (color-flipped, ranks mirrored,
/// with one neutral white move inserted so White still moves first).
#[test]
fn is_promotion_also_fires_for_black_reaching_the_first_rank() {
    let (mut state, params) = start();
    let w = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
    let b = ed25519_dalek::SigningKey::from_bytes(&[2u8; 32]);

    // A line that leaves a black pawn on b2 with a promotion available.
    for (i, uci) in [
        "d2d3", "b7b5", "a2a4", "b5a4", "b2b3", "a4b3", "h2h3", "b3b2", "h3h4",
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
        adjourn_ui::board::is_promotion(&status, ('b', 2), ('b', 1)),
        "a black pawn reaching the first rank promotes"
    );
}
