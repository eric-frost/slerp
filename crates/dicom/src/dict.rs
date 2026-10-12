//! Lookups in the generated data dictionary (`dict_data.rs`): an
//! attribute's VR, which implicit VR files leave out, and the names the
//! inspector shows for tags and UIDs.

use crate::data::{Tag, Vr};
use crate::dict_data::{NAMES, REPEATERS, TAGS, UID_NAMES, UIDS};

fn name_at(text: &'static str, packed: u32) -> &'static str {
    let (start, len) = ((packed >> 8) as usize, (packed & 0xFF) as usize);
    text.get(start..start + len).unwrap_or("")
}

/// The VR and name of a standard attribute, repeating groups (overlays
/// `60xx`) included.  Ambiguous VRs ("US or SS", "OB or OW") give the
/// first; item tags give [`Vr::NONE`].
pub fn lookup(t: Tag) -> Option<(Vr, &'static str)> {
    if let Ok(i) = TAGS.binary_search_by_key(&t, |e| e.0) {
        let (_, vr, name) = TAGS[i];
        return Some((Vr(vr), name_at(NAMES, name)));
    }
    REPEATERS.iter().find(|r| t & r.1 == r.0).map(|&(_, _, vr, name)| (Vr(vr), name_at(NAMES, name)))
}

/// The VR an implicit VR file means for a tag.  Unknown and private tags
/// are UN, except Private Creator elements, which are always text (LO).
pub fn implicit_vr(t: Tag) -> Vr {
    if let Some((vr, _)) = lookup(t) {
        return vr;
    }
    let (group, element) = (t >> 16, t & 0xFFFF);
    if group & 1 == 1 && (0x0010..=0x00FF).contains(&element) {
        return Vr::LO;
    }
    if element == 0 {
        return Vr::UL;
    }
    Vr::UN
}

/// The standard name of a UID: a transfer syntax, SOP class, or other
/// well-known UID.
pub fn uid_name(uid: &str) -> Option<&'static str> {
    UIDS.binary_search_by(|e| e.0.cmp(uid)).ok().map(|i| name_at(UID_NAMES, UIDS[i].1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::tag;

    #[test]
    fn names_and_vrs() {
        assert_eq!(lookup(tag(0x0010, 0x0010)), Some((Vr::PN, "Patient's Name")));
        assert_eq!(lookup(tag(0x0028, 0x0010)), Some((Vr::US, "Rows")));
        assert_eq!(lookup(tag(0x7FE0, 0x0010)).map(|e| e.0), Some(Vr::OB));
        // Overlays repeat in groups 6000 to 601E.
        assert_eq!(lookup(tag(0x6002, 0x3000)).map(|e| e.1), Some("Overlay Data"));
        assert_eq!(implicit_vr(tag(0x0029, 0x0010)), Vr::LO);
        assert_eq!(implicit_vr(tag(0x0029, 0x1010)), Vr::UN);
        assert_eq!(
            uid_name("1.2.840.10008.1.2.4.70"),
            Some("JPEG Lossless, Non-Hierarchical, First-Order Prediction (Process 14 [Selection Value 1])")
        );
        assert_eq!(uid_name("1.2.840.10008.5.1.4.1.1.2"), Some("CT Image Storage"));
        assert_eq!(uid_name("1.2.3"), None);
        // Every packed name lies inside the text.
        for &(_, _, n) in TAGS {
            assert!(!name_at(NAMES, n).is_empty());
        }
    }
}
