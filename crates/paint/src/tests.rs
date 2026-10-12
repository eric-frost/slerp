use super::*;

#[test]
fn sniffing() {
    let p = Path::new("a.bin");
    assert_eq!(sniff(p, b"8BPS\0\x01\0\0\0\0\0\0\0\x03\0\0\0\x01\0\0\0\x01\0\x08\0\x03"), Some(PaintFormat::Psd));
    assert_eq!(sniff(p, b"gimp xcf v011\0"), Some(PaintFormat::Xcf));
    assert_eq!(sniff(Path::new("a.xcf.gz"), &[0x1f, 0x8b, 8, 0]), Some(PaintFormat::Xcf));
    assert_eq!(sniff(Path::new("a.tar.gz"), &[0x1f, 0x8b, 8, 0]), None);
    assert_eq!(sniff(p, b"PK\x03\x04"), None);
}
