//! 行列 ↔ 字节偏移（F-61 错误定位的地基）。
//!
//! 驱动返回的错误位置（PostgreSQL `position` / SQL Server `LineNumber`）是
//! **1-based 行列**，而编辑器与 `sql_lang` 用的是**字节偏移**。中间的换算必须
//! 覆盖三种会把「行号列号」算错的情况：
//! * CRLF（`\r` 不是列）；
//! * Tab（列按字符算，不展开）；
//! * 多字节字符（一个汉字是 3 字节、1 列）。

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIndex {
    /// 每行起始字节偏移，`starts[0] == 0`。
    starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(sql: &str) -> Self {
        let mut starts = vec![0usize];
        for (i, b) in sql.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self { starts }
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// 1-based 行号 → 行起始字节偏移。
    pub fn line_start(&self, line: usize) -> Option<usize> {
        if line == 0 {
            return None;
        }
        self.starts.get(line - 1).copied()
    }

    /// 字节偏移 → (1-based 行, 1-based 列；列按字符数，CR 不计入列)。
    pub fn line_col(&self, sql: &str, offset: usize) -> (usize, usize) {
        let offset = offset.min(sql.len());
        let line = match self.starts.binary_search(&offset) {
            Ok(idx) => idx + 1,
            Err(idx) => idx, // idx = 该偏移落在的行下标（0-based）→ 行号 = idx
        };
        let start = self.starts[line - 1];
        let text = sql[start..offset].trim_end_matches('\r');
        (line, text.chars().count() + 1)
    }

    /// (1-based 行, 1-based 列) → 字节偏移；越界返回 `None`。
    pub fn offset_of(&self, sql: &str, line: usize, col: usize) -> Option<usize> {
        let start = self.line_start(line)?;
        if col == 0 {
            return None;
        }
        let rest = &sql[start..];
        let mut chars = rest.char_indices();
        let mut taken = 0usize;
        for _ in 1..col {
            match chars.next() {
                Some((_, c)) => {
                    if c == '\n' {
                        // 列超出行尾：夹到行尾（不含换行）。
                        return Some(start + taken);
                    }
                    taken += c.len_utf8();
                }
                None => return Some(start + taken),
            }
        }
        // 允许指向行尾之后一个位置（光标停在行末）。
        if let Some((i, c)) = chars.next() {
            if c == '\n' {
                return Some(start + i);
            }
        }
        Some(start + taken)
    }

    /// 取一行文本（不含 `\r\n`）。
    pub fn line_text<'a>(&self, sql: &'a str, line: usize) -> &'a str {
        let Some(start) = self.line_start(line) else {
            return "";
        };
        let end = self.starts.get(line).copied().unwrap_or(sql.len());
        sql[start..end].trim_end_matches(['\r', '\n'])
    }
}

/// 便捷包装：一次换算，适合错误定位这种低频调用。
pub fn line_col_of(sql: &str, offset: usize) -> (usize, usize) {
    LineIndex::new(sql).line_col(sql, offset)
}

/// 1-based 字符位置 → 字节偏移（驱动 `position` / 服务端行列换算用）。
///
/// 超出文本长度时夹到末尾；位置 0 视为非法，返回 `None`。
/// 多字节字符按字符计数（与 PostgreSQL `cursorpos` 一致）。
pub fn byte_offset_of_char(text: &str, char_pos: usize) -> Option<usize> {
    if char_pos == 0 {
        return None;
    }
    Some(
        text.char_indices()
            .nth(char_pos - 1)
            .map(|(offset, _)| offset)
            .unwrap_or(text.len()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_offsets_round_trip() {
        let sql = "SELECT 1;\nSELECT 2;";
        let idx = LineIndex::new(sql);
        assert_eq!(idx.line_count(), 2);
        let off = sql.find("2").unwrap();
        assert_eq!(idx.line_col(sql, off), (2, 8));
        assert_eq!(idx.offset_of(sql, 2, 8), Some(off));
    }

    #[test]
    fn crlf_does_not_count_as_a_column() {
        let sql = "SELECT 1;\r\nSELECT 2;";
        let idx = LineIndex::new(sql);
        let off = sql.find("2").unwrap();
        assert_eq!(idx.line_col(sql, off), (2, 8));
        assert_eq!(idx.line_text(sql, 2), "SELECT 2;");
    }

    #[test]
    fn multibyte_chars_count_as_one_column() {
        // 第 2 行的 `列` 是第 8 列，但字节偏移要按 UTF-8 算。
        let sql = "SELECT 1;\nSELECT 列 FROM t;";
        let idx = LineIndex::new(sql);
        let off = sql.find('列').unwrap();
        assert_eq!(idx.line_col(sql, off), (2, 8));
        assert_eq!(idx.offset_of(sql, 2, 8), Some(off));
        assert_eq!(idx.offset_of(sql, 2, 9), Some(off + '列'.len_utf8()));
    }

    #[test]
    fn tab_counts_as_one_column() {
        let sql = "SELECT\t1;\n\tSELECT 2;";
        let idx = LineIndex::new(sql);
        let off = sql.find("SELECT 2").unwrap();
        assert_eq!(idx.line_col(sql, off), (2, 2));
        assert_eq!(idx.offset_of(sql, 2, 2), Some(off));
    }

    #[test]
    fn out_of_range_is_safe() {
        let sql = "SELECT 1";
        let idx = LineIndex::new(sql);
        assert_eq!(idx.offset_of(sql, 9, 1), None);
        assert_eq!(idx.offset_of(sql, 1, 0), None);
        assert_eq!(idx.line_text(sql, 5), "");
        // 列超出行尾：夹到行尾，不 panic。
        assert_eq!(idx.offset_of(sql, 1, 99), Some(sql.len()));
        assert_eq!(idx.line_col(sql, 999), (1, 9));
    }

    #[test]
    fn char_position_maps_to_utf8_bytes() {
        // PostgreSQL 的 `cursorpos` 是 1-based 字符位置，汉字占 3 字节但只算 1 列。
        let sql = "SELECT * FROM t WHERE 列 = 'x'";
        assert_eq!(byte_offset_of_char(sql, 1), Some(0));
        assert_eq!(byte_offset_of_char(sql, 23), sql.find('列'));
        assert_eq!(
            byte_offset_of_char(sql, 24),
            sql.find('列').map(|offset| offset + '列'.len_utf8())
        );
        assert_eq!(byte_offset_of_char(sql, 0), None);
        // 超出长度：夹到末尾。
        assert_eq!(byte_offset_of_char(sql, 999), Some(sql.len()));
        // 再把字节换算回 1-based 行列，恢复原始位置。
        let offset = byte_offset_of_char(sql, 23).unwrap();
        assert_eq!(LineIndex::new(sql).line_col(sql, offset), (1, 23));
    }
}
