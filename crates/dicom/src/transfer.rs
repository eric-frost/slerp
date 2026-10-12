//! Transfer syntaxes: how a data set is encoded (explicit or implicit VR,
//! byte order, deflate) and how its pixel data is compressed.

/// How pixel data is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// Uncompressed ("native") pixel data.
    Native,
    /// Uncompressed frames in encapsulated fragments (1.2.840.10008.1.2.1.98).
    EncapsulatedNative,
    /// Each frame deflated on its own (1.2.840.10008.1.2.8.1, for 1-bit
    /// segmentations).
    DeflatedFrames,
    /// Baseline, extended and progressive JPEG (processes 1 to 13).
    Jpeg,
    /// Lossless JPEG (processes 14 and 15).
    JpegLossless,
    JpegLs,
    Jpeg2000,
    /// High-throughput JPEG 2000 (Part 15).
    HtJpeg2000,
    Rle,
    /// MPEG-2, MPEG-4, H.264 and HEVC video.
    Video,
    JpegXl,
    /// A transfer syntax this crate doesn't know; its fragments are sniffed.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferSyntax {
    pub uid: String,
    pub explicit_vr: bool,
    pub big_endian: bool,
    /// The data set after the file meta information is deflated.
    pub deflated: bool,
    pub codec: Codec,
}

pub const IMPLICIT_LE: &str = "1.2.840.10008.1.2";
pub const EXPLICIT_LE: &str = "1.2.840.10008.1.2.1";
pub const EXPLICIT_BE: &str = "1.2.840.10008.1.2.2";

impl TransferSyntax {
    pub fn from_uid(uid: &str) -> TransferSyntax {
        let uid = uid.trim_matches(|c: char| c == '\0' || c == ' ');
        let (mut explicit_vr, mut big_endian, mut deflated) = (true, false, false);
        let codec = match uid {
            IMPLICIT_LE => {
                explicit_vr = false;
                Codec::Native
            }
            EXPLICIT_LE => Codec::Native,
            EXPLICIT_BE => {
                big_endian = true;
                Codec::Native
            }
            // Deflated Explicit VR Little Endian, and JPIP's deflated
            // referenced pixel data.
            "1.2.840.10008.1.2.1.99" | "1.2.840.10008.1.2.4.95" => {
                deflated = true;
                Codec::Native
            }
            "1.2.840.10008.1.2.1.98" => Codec::EncapsulatedNative,
            "1.2.840.10008.1.2.8.1" => Codec::DeflatedFrames,
            "1.2.840.10008.1.2.4.57"
            | "1.2.840.10008.1.2.4.58"
            | "1.2.840.10008.1.2.4.65"
            | "1.2.840.10008.1.2.4.66"
            | "1.2.840.10008.1.2.4.70" => Codec::JpegLossless,
            "1.2.840.10008.1.2.4.80" | "1.2.840.10008.1.2.4.81" => Codec::JpegLs,
            "1.2.840.10008.1.2.4.90"
            | "1.2.840.10008.1.2.4.91"
            | "1.2.840.10008.1.2.4.92"
            | "1.2.840.10008.1.2.4.93" => Codec::Jpeg2000,
            "1.2.840.10008.1.2.4.201" | "1.2.840.10008.1.2.4.202" | "1.2.840.10008.1.2.4.203" => Codec::HtJpeg2000,
            "1.2.840.10008.1.2.5" => Codec::Rle,
            "1.2.840.10008.1.2.4.110" | "1.2.840.10008.1.2.4.111" | "1.2.840.10008.1.2.4.112" => Codec::JpegXl,
            u if u.starts_with("1.2.840.10008.1.2.4.10") => Codec::Video,
            // The other JPEG processes (.4.50 to .4.64, hierarchical and
            // progressive ones included).
            u if u.starts_with("1.2.840.10008.1.2.4.5") || u.starts_with("1.2.840.10008.1.2.4.6") => Codec::Jpeg,
            _ => Codec::Unknown,
        };
        TransferSyntax { uid: uid.to_string(), explicit_vr, big_endian, deflated, codec }
    }

    pub fn implicit_le() -> TransferSyntax {
        TransferSyntax::from_uid(IMPLICIT_LE)
    }

    /// The standard's name for the transfer syntax.
    pub fn name(&self) -> String {
        crate::dict::uid_name(&self.uid).map(str::to_string).unwrap_or_else(|| self.uid.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings() {
        let t = |uid: &str| TransferSyntax::from_uid(uid);
        assert_eq!((t(IMPLICIT_LE).explicit_vr, t(IMPLICIT_LE).big_endian), (false, false));
        assert_eq!((t(EXPLICIT_LE).explicit_vr, t(EXPLICIT_LE).big_endian), (true, false));
        assert_eq!((t(EXPLICIT_BE).explicit_vr, t(EXPLICIT_BE).big_endian), (true, true));
        assert!(t("1.2.840.10008.1.2.1.99").deflated);
        assert!(!t(EXPLICIT_LE).deflated);
        // UIDs are padded with a NUL to an even length.
        assert_eq!(t("1.2.840.10008.1.2.1\0").codec, Codec::Native);
        assert_eq!(t("1.2.840.10008.1.2.1\0").uid, EXPLICIT_LE);
    }

    #[test]
    fn codecs_by_uid() {
        let codec = |uid: &str| TransferSyntax::from_uid(uid).codec;
        for (uid, want) in [
            ("1.2.840.10008.1.2.4.50", Codec::Jpeg),
            ("1.2.840.10008.1.2.4.51", Codec::Jpeg),
            ("1.2.840.10008.1.2.4.57", Codec::JpegLossless),
            ("1.2.840.10008.1.2.4.70", Codec::JpegLossless),
            ("1.2.840.10008.1.2.4.80", Codec::JpegLs),
            ("1.2.840.10008.1.2.4.81", Codec::JpegLs),
            ("1.2.840.10008.1.2.4.90", Codec::Jpeg2000),
            ("1.2.840.10008.1.2.4.91", Codec::Jpeg2000),
            ("1.2.840.10008.1.2.4.201", Codec::HtJpeg2000),
            ("1.2.840.10008.1.2.5", Codec::Rle),
            ("1.2.840.10008.1.2.4.100", Codec::Video),
            ("1.2.840.10008.1.2.4.102", Codec::Video),
            ("1.2.840.10008.1.2.4.110", Codec::JpegXl),
            ("1.2.840.10008.1.2.1.98", Codec::EncapsulatedNative),
            ("1.2.840.10008.1.2.8.1", Codec::DeflatedFrames),
            ("1.2.3.4", Codec::Unknown),
        ] {
            assert_eq!(codec(uid), want, "{uid}");
        }
        assert_eq!(TransferSyntax::from_uid("1.2.840.10008.1.2.5").name(), "RLE Lossless");
        assert_eq!(TransferSyntax::from_uid("9.9").name(), "9.9");
    }
}
