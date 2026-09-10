use std::io::{BufReader, Cursor};

use crate::subprocess::output::read_piece;

#[test]
fn long_unterminated_output_is_bounded_and_preserves_following_bytes() {
    let mut reader = BufReader::with_capacity(3, Cursor::new(b"123456789\nnext\nlast"));
    assert_eq!(read_piece(&mut reader, 5).unwrap(), b"12345");
    assert_eq!(read_piece(&mut reader, 5).unwrap(), b"6789\n");
    assert_eq!(read_piece(&mut reader, 5).unwrap(), b"next\n");
    assert_eq!(read_piece(&mut reader, 5).unwrap(), b"last");
    assert!(read_piece(&mut reader, 5).unwrap().is_empty());
}
