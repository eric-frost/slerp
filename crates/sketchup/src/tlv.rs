//! The tag-length-value records of `model.dat` in 2021 and later files:
//! a two-byte tag, a four-byte little-endian length and that many bytes,
//! which for some tags are more records.
//!
//! Tags are written here as OpenSKP prints them, the two bytes in file
//! order as hexadecimal: the bytes `C4 09` are tag `0xC409`.  Records are
//! read in place, never copied, and walked again when needed, which costs
//! little next to building a tree of millions of nodes.

/// How deep searches go into nested records.  Real files nest about ten
/// deep; a damaged one could nest a record in every six bytes.
pub const MAX_DEPTH: usize = 64;

/// One record.
#[derive(Clone, Copy, Debug)]
pub struct Node<'a> {
    pub tag: u16,
    pub body: &'a [u8],
}

/// The tags whose payload is itself a list of records (OpenSKP's
/// `CONTAINER_TAGS`).  Other payloads are values, and searching never looks
/// inside them.
const CONTAINERS: &[u16] = &[
    0xF401, 0xF701, 0xD430, 0xD530, 0xC832, 0x7C15, 0x8813, 0x8913, 0x8A13, 0x8B13, 0x8C13, 0x8D13, 0x4C1D, 0x6419,
    0xF901, 0x7017, 0x7117, 0xD007, 0xC409, 0x9411, 0x9511, 0x0F01, 0x384A, 0xB80B, 0x9713, 0x2C4C, 0xAC0D, 0xAE0D,
    0xF601, 0xF801, 0x983A, 0x993A, 0x8C3C, 0x8D3C,
    // An Image entity's placement: a standard instance inside these two.
    0x9013, 0x401F, // Guide lines and points and their shared wrapper.
    0x9113, 0x9213, 0x6942, 0x6C42, 0x6842,
];

pub fn is_container(tag: u16) -> bool {
    CONTAINERS.contains(&tag)
}

/// The records in `bytes`, up to the first that runs past the end (as
/// OpenSKP reads them: what follows a damaged record is dropped).
#[derive(Clone)]
pub struct Records<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Records<'a> {
    pub fn new(bytes: &'a [u8]) -> Records<'a> {
        Records { bytes, pos: 0 }
    }
}

impl<'a> Iterator for Records<'a> {
    type Item = Node<'a>;

    fn next(&mut self) -> Option<Node<'a>> {
        let head = self.bytes.get(self.pos..self.pos.checked_add(6)?)?;
        let tag = u16::from_be_bytes([head[0], head[1]]);
        let len = u32::from_le_bytes([head[2], head[3], head[4], head[5]]) as usize;
        let start = self.pos + 6;
        let end = start.checked_add(len)?;
        let body = self.bytes.get(start..end)?;
        self.pos = end;
        Some(Node { tag, body })
    }
}

impl<'a> Node<'a> {
    /// The records inside a container; none for a value.
    pub fn kids(&self) -> Records<'a> {
        if is_container(self.tag) { Records::new(self.body) } else { Records::new(&[]) }
    }

    /// The first record with `tag` among the children, depth first (each
    /// child before its own children), as OpenSKP's `find_child_tag`.
    pub fn find(&self, tag: u16) -> Option<Node<'a>> {
        find_in(self.kids(), tag, 0)
    }

    /// The first direct child with `tag`.
    pub fn child(&self, tag: u16) -> Option<Node<'a>> {
        self.kids().find(|n| n.tag == tag)
    }

    /// Every record with `tag` below this one, in file order.
    pub fn find_all(&self, tag: u16, out: &mut Vec<Node<'a>>) {
        find_all_in(self.kids(), tag, out, 0);
    }

    /// The entity id of the entity this record holds: a `DE05` child, or
    /// the `DE05` at the start of a `DC05` child, or failing both the
    /// first found further down.
    pub fn entity_id(&self) -> Option<u64> {
        entity_id(*self, 0)
    }
}

fn find_in<'a>(nodes: Records<'a>, tag: u16, depth: usize) -> Option<Node<'a>> {
    if depth >= MAX_DEPTH {
        return None;
    }
    for n in nodes {
        if n.tag == tag {
            return Some(n);
        }
        if let Some(found) = find_in(n.kids(), tag, depth + 1) {
            return Some(found);
        }
    }
    None
}

fn find_all_in<'a>(nodes: Records<'a>, tag: u16, out: &mut Vec<Node<'a>>, depth: usize) {
    if depth >= MAX_DEPTH {
        return;
    }
    for n in nodes {
        if n.tag == tag {
            out.push(n);
        }
        find_all_in(n.kids(), tag, out, depth + 1);
    }
}

fn entity_id(node: Node, depth: usize) -> Option<u64> {
    if depth >= MAX_DEPTH {
        return None;
    }
    for c in node.kids() {
        if c.tag == 0xDE05 {
            return Some(var_int(c.body));
        }
        if c.tag == 0xDC05 && c.body.starts_with(&[0xDE, 0x05]) {
            return Some(wrapped_id(c.body));
        }
    }
    node.kids().find_map(|c| entity_id(c, depth + 1))
}

/// The id inside a `DC05` payload that starts with a `DE05` record.
pub fn wrapped_id(body: &[u8]) -> u64 {
    let len = body.get(2..6).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    var_int(body.get(6..6usize.saturating_add(len)).unwrap_or(&[]))
}

/// A little-endian unsigned integer of whatever length the payload has (ids
/// and references are written in as few bytes as they need).  Bytes past
/// the eighth are ignored.
pub fn var_int(bytes: &[u8]) -> u64 {
    bytes.iter().take(8).enumerate().fold(0u64, |v, (i, &b)| v | (b as u64) << (8 * i))
}

/// `n` little-endian doubles at the start of `bytes`, when it holds them.
pub fn doubles<const N: usize>(bytes: &[u8]) -> Option<[f64; N]> {
    let mut out = [0.0; N];
    for (i, o) in out.iter_mut().enumerate() {
        let b = bytes.get(i * 8..i * 8 + 8)?;
        *o = f64::from_le_bytes(b.try_into().ok()?);
    }
    Some(out)
}

/// The payload of the first record with `tag` in a plain list of records.
pub fn item(bytes: &[u8], tag: u16) -> Option<&[u8]> {
    Records::new(bytes).find(|n| n.tag == tag).map(|n| n.body)
}

/// A payload read as a list of records that must fill it exactly (as the
/// scene list nests them), or `None`.
pub fn exact_items(bytes: &[u8]) -> Option<Vec<Node<'_>>> {
    let mut out = Vec::new();
    let mut r = Records::new(bytes);
    for n in r.by_ref() {
        if n.tag == 0 {
            return None;
        }
        out.push(n);
    }
    (r.pos == bytes.len()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(tag: u16, body: &[u8]) -> Vec<u8> {
        let mut v = tag.to_be_bytes().to_vec();
        v.extend((body.len() as u32).to_le_bytes());
        v.extend(body);
        v
    }

    #[test]
    fn walks_containers_only() {
        let inner = rec(0xDE05, &[0x34, 0x12]);
        let vertex = rec(0xC409, &[rec(0xDC05, &[0xDE, 0x05, 2, 0, 0, 0, 7, 0]), rec(0xC509, &[0; 24])].concat());
        let leaf = rec(0x1234, &inner);
        let list = [vertex, leaf].concat();
        let top = Node { tag: 0xF601, body: &list };
        let v = top.find(0xC409).unwrap();
        assert_eq!(v.entity_id(), Some(7));
        // A value's payload is never searched, even when it looks like records.
        assert!(top.find(0xDE05).is_none());
        assert_eq!(var_int(&[0x34, 0x12]), 0x1234);
    }

    #[test]
    fn stops_at_records_that_overrun() {
        let mut bytes = rec(0xC509, &[1, 2, 3]);
        bytes.extend([0xAA, 0xBB, 0xFF, 0xFF, 0xFF, 0x7F, 0]);
        assert_eq!(Records::new(&bytes).count(), 1);
        assert!(exact_items(&bytes).is_none());
        let deep = (0..1000).fold(Vec::new(), |inner, _| rec(0xF601, &inner));
        assert!(Node { tag: 0xF601, body: &deep }.find(0x1111).is_none());
    }
}
