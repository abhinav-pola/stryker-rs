use stryker_core::Position;

/// Byte-offset → 1-based line/column lookup for one source file.
///
/// Columns count UTF-16 code units to match the JS-ecosystem convention used
/// by the mutation-testing report schema consumers.
pub struct LineIndex {
    /// Byte offset of the start of each line (line_starts[0] == 0).
    line_starts: Vec<u32>,
    source_len: u32,
}

impl LineIndex {
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0u32];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        Self {
            line_starts,
            source_len: source.len() as u32,
        }
    }

    /// 1-based line and column for a byte offset. `source` must be the same
    /// string the index was built from.
    pub fn position(&self, source: &str, offset: u32) -> Position {
        let offset = offset.min(self.source_len);
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let line_start = self.line_starts[line_idx] as usize;
        let col_utf16: usize = source[line_start..offset as usize]
            .chars()
            .map(|c| c.len_utf16())
            .sum();
        Position {
            line: line_idx as u32 + 1,
            column: col_utf16 as u32 + 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_char_is_line1_col1() {
        let src = "abc\ndef";
        let idx = LineIndex::new(src);
        assert_eq!(idx.position(src, 0), Position { line: 1, column: 1 });
        assert_eq!(idx.position(src, 4), Position { line: 2, column: 1 });
        assert_eq!(idx.position(src, 6), Position { line: 2, column: 3 });
    }

    #[test]
    fn utf16_columns() {
        // '😀' is 4 bytes UTF-8, 2 UTF-16 code units.
        let src = "a😀b";
        let idx = LineIndex::new(src);
        assert_eq!(idx.position(src, 1), Position { line: 1, column: 2 });
        assert_eq!(idx.position(src, 5), Position { line: 1, column: 4 });
    }
}
