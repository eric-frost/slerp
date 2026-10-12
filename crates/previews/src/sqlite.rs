//! Just enough of SQLite's file format to read one column of one row:
//! Clip Studio Paint keeps its canvas preview as a PNG blob in an SQLite
//! database inside its own file.  Pages are read from the file on demand,
//! so a database of hundreds of megabytes (the layers' tiles) costs only
//! the pages on the path to the preview.
//!
//! Reads table b-trees (leaf and interior pages), records, and payloads
//! that spill onto overflow pages.  Indexes, WAL files, free lists and
//! writing are left out.  Every page number, offset and length is checked,
//! and walks are capped, so a damaged or hostile database fails rather
//! than loops.

use crate::source::Source;
use slerp_scene::LoadError;

const MAGIC: &[u8] = b"SQLite format 3\0";

/// The most pages visited while looking for one row or one value.
const MAX_STEPS: usize = 100_000;

/// The largest value read.
const MAX_VALUE: u64 = 64 << 20;

/// A database starting at some offset of a file.
pub struct Db<'a> {
    src: &'a mut Source,
    base: u64,
    page_size: u64,
    /// Page size minus the bytes each page reserves at its end.
    usable: u64,
    pages: u64,
}

fn bad(what: &str) -> LoadError {
    LoadError::invalid(format!("SQLite database: {what}"))
}

/// A value of a record.
#[derive(Debug, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }
}

/// Reads a big-endian variable-length integer: up to eight bytes of seven
/// bits each, then a ninth of eight.
fn varint(b: &[u8], at: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for i in 0..9 {
        let byte = *b.get(*at)?;
        *at += 1;
        if i == 8 {
            return Some(v << 8 | byte as u64);
        }
        v = v << 7 | (byte & 0x7F) as u64;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

impl<'a> Db<'a> {
    /// Opens the database whose header starts at `base` in `src`, and
    /// which takes `len` bytes.
    pub fn open(src: &'a mut Source, base: u64, len: u64) -> Result<Db<'a>, LoadError> {
        let h = src.exact(base, 100)?;
        if !h.starts_with(MAGIC) {
            return Err(bad("no header"));
        }
        let page_size = match u16::from_be_bytes([h[16], h[17]]) {
            1 => 65536,
            n if n >= 512 && n.is_power_of_two() => n as u64,
            _ => return Err(bad("bad page size")),
        };
        let usable = page_size - h[20] as u64;
        if usable < 480 {
            return Err(bad("bad reserved space"));
        }
        Ok(Db { src, base, page_size, usable, pages: len / page_size })
    }

    fn page(&mut self, n: u64) -> Result<Vec<u8>, LoadError> {
        if n == 0 || n > self.pages {
            return Err(bad("page number out of range"));
        }
        self.src.exact(self.base + (n - 1) * self.page_size, self.page_size)
    }

    /// Finds the first row of the table b-tree rooted at `root` for which
    /// `want` returns true, and returns its values.  `want` sees each row's
    /// values with blobs and long texts left unread (as `Null`), so
    /// scanning stays cheap; the matching row is then read whole.
    pub fn find_row(
        &mut self,
        root: u64,
        mut want: impl FnMut(&[Value]) -> bool,
    ) -> Result<Option<Vec<Value>>, LoadError> {
        // Depth-first through interior pages, left to right.  Each page
        // is visited once, so a damaged tree pointing back at itself ends.
        let mut stack = vec![root];
        let mut seen = std::collections::HashSet::new();
        while let Some(n) = stack.pop() {
            if !seen.insert(n) {
                continue;
            }
            if seen.len() > MAX_STEPS || stack.len() > MAX_STEPS {
                return Err(bad("too many pages"));
            }
            let page = self.page(n)?;
            let h = if n == 1 { 100 } else { 0 };
            let kind = *page.get(h).ok_or_else(|| bad("short page"))?;
            let cells = u16::from_be_bytes([page[h + 3], page[h + 4]]) as usize;
            match kind {
                // An interior table page: children in key order, then the
                // right-most one.
                0x05 => {
                    let right = u32::from_be_bytes(page[h + 8..h + 12].try_into().unwrap_or([0; 4])) as u64;
                    let mut children = Vec::with_capacity(cells + 1);
                    for i in 0..cells {
                        let ptr = self.cell_pointer(&page, h + 12, i)?;
                        let child = page.get(ptr..ptr + 4).ok_or_else(|| bad("bad cell"))?;
                        children.push(u32::from_be_bytes(child.try_into().unwrap_or([0; 4])) as u64);
                    }
                    children.push(right);
                    stack.extend(children.into_iter().rev());
                }
                // A leaf: the rows.
                0x0D => {
                    for i in 0..cells {
                        let ptr = self.cell_pointer(&page, h + 8, i)?;
                        let mut at = ptr;
                        let size = varint(&page, &mut at).ok_or_else(|| bad("bad cell"))?;
                        let _rowid = varint(&page, &mut at).ok_or_else(|| bad("bad cell"))?;
                        let peek = self.record(&page, at, size, false)?;
                        if want(&peek) {
                            return self.record(&page, at, size, true).map(Some);
                        }
                    }
                }
                _ => return Err(bad("not a table page")),
            }
        }
        Ok(None)
    }

    fn cell_pointer(&self, page: &[u8], array: usize, i: usize) -> Result<usize, LoadError> {
        let p = page.get(array + i * 2..array + i * 2 + 2).ok_or_else(|| bad("bad cell pointer"))?;
        let ptr = u16::from_be_bytes([p[0], p[1]]) as usize;
        if ptr >= page.len() {
            return Err(bad("bad cell pointer"));
        }
        Ok(ptr)
    }

    /// The payload of a cell whose `size` bytes start at `at` in `page`,
    /// spilling onto overflow pages.  With `whole` false, only the part on
    /// the page is returned.
    fn payload(&mut self, page: &[u8], at: usize, size: u64, whole: bool) -> Result<Vec<u8>, LoadError> {
        // How much stays on the page, as SQLite decides it for table
        // leaves.
        let u = self.usable;
        let x = u - 35;
        let local = if size <= x {
            size
        } else {
            let m = (u - 12) * 32 / 255 - 23;
            let k = m + (size - m) % (u - 4);
            if k <= x { k } else { m }
        };
        let end = at.checked_add(local as usize).ok_or_else(|| bad("bad cell"))?;
        let mut out = page.get(at..end).ok_or_else(|| bad("cell runs past its page"))?.to_vec();
        if !whole || local == size {
            return Ok(out);
        }
        if size > MAX_VALUE {
            return Err(LoadError::TooLarge(format!("a {size}-byte SQLite record")));
        }
        let next = page.get(end..end + 4).ok_or_else(|| bad("bad overflow pointer"))?;
        let mut next = u32::from_be_bytes(next.try_into().unwrap_or([0; 4])) as u64;
        let mut steps = 0;
        while (out.len() as u64) < size {
            steps += 1;
            if next == 0 || steps > MAX_STEPS {
                return Err(bad("broken overflow chain"));
            }
            let p = self.page(next)?;
            next = u32::from_be_bytes([p[0], p[1], p[2], p[3]]) as u64;
            let take = (size - out.len() as u64).min(u - 4) as usize;
            out.extend_from_slice(&p[4..4 + take]);
        }
        Ok(out)
    }

    /// Decodes a record.  With `whole` false, values that aren't wholly
    /// on the page come back as `Null`.
    fn record(&mut self, page: &[u8], at: usize, size: u64, whole: bool) -> Result<Vec<Value>, LoadError> {
        let data = self.payload(page, at, size, whole)?;
        let mut h = 0usize;
        let header_len = varint(&data, &mut h).ok_or_else(|| bad("bad record"))? as usize;
        let mut types = Vec::new();
        while h < header_len.min(data.len()) && types.len() < 4096 {
            types.push(varint(&data, &mut h).ok_or_else(|| bad("bad record"))?);
        }
        let mut body = header_len;
        let mut values = Vec::with_capacity(types.len());
        for t in types {
            let len = match t {
                0 | 8 | 9 => 0,
                1..=4 => t as usize,
                5 => 6,
                6 | 7 => 8,
                n if n >= 12 => ((n - 12) / 2) as usize,
                _ => return Err(bad("bad record")),
            };
            let Some(bytes) = body.checked_add(len).and_then(|end| data.get(body..end)) else {
                // Past what was read: a value only the whole record holds.
                if whole {
                    return Err(bad("record shorter than its header says"));
                }
                values.push(Value::Null);
                body = body.saturating_add(len);
                continue;
            };
            body += len;
            let int = || {
                bytes.iter().fold(if bytes.first().is_some_and(|b| b & 0x80 != 0) { -1i64 } else { 0 }, |v, &b| {
                    v << 8 | b as i64
                })
            };
            values.push(match t {
                0 => Value::Null,
                8 => Value::Int(0),
                9 => Value::Int(1),
                1..=6 => Value::Int(int()),
                7 => Value::Float(f64::from_bits(u64::from_be_bytes(bytes.try_into().unwrap_or([0; 8])))),
                n if n % 2 == 0 => Value::Blob(bytes.to_vec()),
                _ => Value::Text(String::from_utf8_lossy(bytes).into_owned()),
            });
        }
        Ok(values)
    }

    /// The root page and column names of the table called `name`, from
    /// the schema table on page 1.
    pub fn table(&mut self, name: &str) -> Result<Option<(u64, Vec<String>)>, LoadError> {
        let row = self.find_row(1, |v| {
            v.first().and_then(Value::as_text) == Some("table") && v.get(1).and_then(Value::as_text) == Some(name)
        })?;
        let Some(row) = row else { return Ok(None) };
        let root = row.get(3).and_then(Value::as_int).filter(|&r| r > 0).ok_or_else(|| bad("bad schema"))? as u64;
        let sql = row.get(4).and_then(Value::as_text).unwrap_or_default();
        Ok(Some((root, column_names(sql))))
    }
}

/// The column names of a `CREATE TABLE` statement, in order: the first
/// word of each comma-separated definition inside the outer parentheses,
/// skipping table constraints.
pub fn column_names(sql: &str) -> Vec<String> {
    let Some(open) = sql.find('(') else { return Vec::new() };
    let mut names = Vec::new();
    let mut depth = 0;
    let mut part = String::new();
    for c in sql[open + 1..].chars() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => break,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                names.push(std::mem::take(&mut part));
                continue;
            }
            _ => {}
        }
        part.push(c);
    }
    names.push(part);
    names
        .iter()
        .filter_map(|p| p.split_whitespace().next())
        .map(|w| w.trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']').to_string())
        .filter(|w| !["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"].contains(&w.to_ascii_uppercase().as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_column_names() {
        let sql = "CREATE TABLE CanvasPreview(_PW_ID INTEGER PRIMARY KEY AUTOINCREMENT, CanvasId INTEGER DEFAULT(0), ImageData BLOB, \"ImageWidth\" INTEGER, PRIMARY KEY(x))";
        assert_eq!(column_names(sql), ["_PW_ID", "CanvasId", "ImageData", "ImageWidth"]);
    }

    #[test]
    fn reads_varints() {
        let mut at = 0;
        assert_eq!(varint(&[0x81, 0x00], &mut at), Some(128));
        let mut at = 0;
        assert_eq!(varint(&[0xFF; 9], &mut at), Some(u64::MAX));
        let mut at = 0;
        assert_eq!(varint(&[0x81], &mut at), None);
    }
}
