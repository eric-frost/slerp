//! The byte-level half of the Windows drag-out code, which needs no Windows
//! API: the `DROPFILES` block that carries the `CF_HDROP` file list, and the
//! premultiplied bitmap behind the drag picture.  Keeping them apart from the
//! COM objects means plain integers are enough to test them, on any system.

/// The size of the `DROPFILES` header: the offset of the list (`u32`), a
/// `POINT` (two `i32`) and two `BOOL`s (`i32` each).
pub(super) const HEADER_LEN: usize = 20;

/// Drops the `\\?\` prefix of a path in the extended form, which many
/// programs that take dropped files can't read: `\\?\C:\a` becomes `C:\a` and
/// `\\?\UNC\server\share` becomes `\\server\share`.  Any other path is
/// returned as it is.
pub(super) fn plain_path(path: &[u16]) -> Vec<u16> {
    let units = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    let verbatim = units(r"\\?\");
    let unc = units(r"\\?\UNC\");
    if let Some(rest) = path.strip_prefix(unc.as_slice()) {
        let mut plain = units(r"\\");
        plain.extend_from_slice(rest);
        plain
    } else if let Some(rest) = path.strip_prefix(verbatim.as_slice()) {
        rest.to_vec()
    } else {
        path.to_vec()
    }
}

/// The contents of a `CF_HDROP` global memory block for these paths, each a
/// UTF-16 string without its terminator.  The header says the list starts
/// right after it and holds wide characters; the list is each path followed
/// by a NUL, and one more NUL ends it (so an empty list is two NULs).  Paths
/// that are empty or hold a NUL would end the list early and are left out.
pub(super) fn drop_files(paths: &[Vec<u16>]) -> Vec<u8> {
    let mut block = Vec::with_capacity(HEADER_LEN + 2 + paths.iter().map(|p| p.len() * 2 + 2).sum::<usize>());
    block.extend_from_slice(&(HEADER_LEN as u32).to_le_bytes());
    block.extend_from_slice(&[0; 8]); // pt: not a drop at a point of the window
    block.extend_from_slice(&0u32.to_le_bytes()); // fNC: a point in the client area
    block.extend_from_slice(&1u32.to_le_bytes()); // fWide: UTF-16, not the ANSI code page
    let mut count = 0;
    for path in paths.iter().filter(|p| !p.is_empty() && !p.contains(&0)) {
        for unit in plain_path(path) {
            block.extend_from_slice(&unit.to_le_bytes());
        }
        block.extend_from_slice(&[0, 0]);
        count += 1;
    }
    block.extend_from_slice(&[0, 0]);
    if count == 0 {
        block.extend_from_slice(&[0, 0]);
    }
    block
}

/// Converts straight-alpha RGBA to the premultiplied BGRA a Windows bitmap
/// with an alpha channel holds, which the shell blends the drag picture from.
pub(super) fn premultiplied_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut bgra = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        let alpha = px[3] as u32;
        let scale = |c: u8| ((c as u32 * alpha + 127) / 255) as u8;
        bgra.extend_from_slice(&[scale(px[2]), scale(px[1]), scale(px[0]), px[3]]);
    }
    bgra
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn u32_at(block: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(block[at..at + 4].try_into().unwrap())
    }

    /// Reads the list the way `DragQueryFileW` does.
    fn list(block: &[u8]) -> Vec<String> {
        let units: Vec<u16> =
            block[u32_at(block, 0) as usize..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let mut paths = Vec::new();
        let mut rest = &units[..];
        while !rest.is_empty() && rest[0] != 0 {
            let end = rest.iter().position(|&u| u == 0).unwrap();
            paths.push(String::from_utf16(&rest[..end]).unwrap());
            rest = &rest[end + 1..];
        }
        paths
    }

    #[test]
    fn header_and_one_path() {
        let block = drop_files(&[wide(r"C:\Temp\a.txt")]);
        assert_eq!(u32_at(&block, 0), 20, "the list follows the 20-byte header");
        assert_eq!(&block[4..12], &[0; 8], "pt");
        assert_eq!(u32_at(&block, 12), 0, "fNC");
        assert_eq!(u32_at(&block, 16), 1, "fWide");
        // 13 characters, a NUL, and the list's own NUL.
        assert_eq!(block.len(), 20 + (13 + 1 + 1) * 2);
        assert_eq!(&block[20..24], &[b'C', 0, b':', 0]);
        assert_eq!(&block[block.len() - 4..], &[0, 0, 0, 0], "double NUL at the end");
        assert_eq!(list(&block), [r"C:\Temp\a.txt"]);
    }

    #[test]
    fn several_paths_keep_their_order() {
        let paths = [r"C:\Temp\b.txt", r"C:\Temp\a.txt", r"D:\c"].map(wide);
        assert_eq!(list(&drop_files(&paths)), [r"C:\Temp\b.txt", r"C:\Temp\a.txt", r"D:\c"]);
    }

    #[test]
    fn awkward_names_survive() {
        let names = [
            r"C:\Temp\notes #1 (50%).txt",
            r"C:\Temp\größe ☃.png",
            "C:\\Temp\\emoji \u{1F600}.bin",
            r"\\server\share\dir\a b.txt",
        ];
        let paths: Vec<Vec<u16>> = names.iter().map(|n| wide(n)).collect();
        assert_eq!(list(&drop_files(&paths)), names);
        // The emoji is a surrogate pair, two units of two bytes.
        let block = drop_files(&paths[2..3]);
        assert_eq!(block.len(), 20 + (paths[2].len() + 1 + 1) * 2);
        assert!(paths[2].len() > names[2].chars().count());
    }

    #[test]
    fn extended_prefixes_are_removed() {
        assert_eq!(plain_path(&wide(r"\\?\C:\Temp\a.txt")), wide(r"C:\Temp\a.txt"));
        assert_eq!(plain_path(&wide(r"\\?\UNC\server\share\a.txt")), wide(r"\\server\share\a.txt"));
        assert_eq!(plain_path(&wide(r"C:\Temp\a.txt")), wide(r"C:\Temp\a.txt"));
        assert_eq!(plain_path(&wide(r"\\server\share")), wide(r"\\server\share"));
        assert_eq!(list(&drop_files(&[wide(r"\\?\C:\x y.txt")])), [r"C:\x y.txt"]);
    }

    #[test]
    fn unusable_paths_are_left_out() {
        let paths = [wide(r"C:\a"), Vec::new(), vec![b'C' as u16, 0, b'x' as u16], wide(r"C:\b")];
        assert_eq!(list(&drop_files(&paths)), [r"C:\a", r"C:\b"]);
    }

    #[test]
    fn an_empty_list_is_two_nuls() {
        let block = drop_files(&[]);
        assert_eq!(block.len(), 20 + 4);
        assert_eq!(&block[20..], &[0, 0, 0, 0]);
        assert!(list(&block).is_empty());
    }

    #[test]
    fn bitmap_is_premultiplied_and_swapped() {
        let bgra = premultiplied_bgra(&[255, 128, 0, 255, 200, 100, 50, 128, 10, 20, 30, 0]);
        assert_eq!(&bgra[0..4], &[0, 128, 255, 255], "opaque pixels only swap red and blue");
        assert_eq!(&bgra[4..8], &[25, 50, 100, 128], "half-transparent colours are halved");
        assert_eq!(&bgra[8..12], &[0, 0, 0, 0], "fully transparent pixels are black");
    }
}
