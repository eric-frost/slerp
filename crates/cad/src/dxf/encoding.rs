//! Turning the bytes of a DXF string into text.  Files from R2007 (AC1021)
//! on are UTF-8.  Older ones use the code page named by the header's
//! `$DWGCODEPAGE`, such as `ANSI_1252` for Western Europe or `ANSI_932` for
//! Japanese; the common ones are done by `encoding_rs`, and the DOS pages
//! AutoCAD for DOS used have 128-character tables here.  Characters a code
//! page lacks can also be written as `\U+XXXX`, which is decoded too.

use encoding_rs::Encoding;
use std::borrow::Cow;

/// The upper half (128 to 255) of cp437.
const CP437: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{A0}";

/// The upper half (128 to 255) of cp850.
const CP850: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜø£Ø×ƒáíóúñÑªº¿®¬½¼¡«»░▒▓│┤ÁÂÀ©╣║╗╝¢¥┐└┴┬├─┼ãÃ╚╔╩╦╠═╬¤ðÐÊËÈıÍÎÏ┘┌█▄¦Ì▀ÓßÔÒõÕµþÞÚÛÙýÝ¯´\u{AD}±‗¾¶§÷¸°¨·¹³²■\u{A0}";

/// The upper half (128 to 255) of cp852.
const CP852: &str = "ÇüéâäůćçłëŐőîŹÄĆÉĹĺôöĽľŚśÖÜŤťŁ×čáíóúĄąŽžĘę¬źČş«»░▒▓│┤ÁÂĚŞ╣║╗╝Żż┐└┴┬├─┼Ăă╚╔╩╦╠═╬¤đĐĎËďŇÍÎě┘┌█▄ŢŮ▀ÓßÔŃńňŠšŔÚŕŰýÝţ´\u{AD}˝˛ˇ˘§÷¸°¨˙űŘř■\u{A0}";

/// How a file's strings are turned into text.
#[derive(Clone, Copy, Debug)]
pub enum Decoder {
    Utf8,
    /// A code page `encoding_rs` knows.
    Rs(&'static Encoding),
    /// A DOS code page: its upper half as characters.
    Dos(&'static str),
    /// Windows-1252, the default of old files, which are also taken as
    /// UTF-8 when they are valid UTF-8 with accents in it: some programs
    /// write UTF-8 into files that declare a code page.
    Western,
}

impl Decoder {
    /// The decoder for a header's `$DWGCODEPAGE` value.
    pub fn from_codepage(name: &str) -> Decoder {
        let n = name.trim().to_ascii_uppercase();
        let num = n.trim_start_matches(|c: char| !c.is_ascii_digit());
        match n.as_str() {
            "UTF-8" | "UTF8" => return Decoder::Utf8,
            "MACINTOSH" | "MAC-ROMAN" => return Decoder::Rs(encoding_rs::MACINTOSH),
            _ => {}
        }
        if n.starts_with("DOS") {
            return match num {
                "437" => Decoder::Dos(CP437),
                "850" => Decoder::Dos(CP850),
                "852" => Decoder::Dos(CP852),
                "866" => Decoder::Rs(encoding_rs::IBM866),
                _ => Decoder::Dos(CP437),
            };
        }
        if n.starts_with("ANSI_") {
            return Decoder::Rs(match num {
                "874" => encoding_rs::WINDOWS_874,
                "932" => encoding_rs::SHIFT_JIS,
                "936" => encoding_rs::GBK,
                "949" => encoding_rs::EUC_KR,
                "950" => encoding_rs::BIG5,
                "1250" => encoding_rs::WINDOWS_1250,
                "1251" => encoding_rs::WINDOWS_1251,
                "1253" => encoding_rs::WINDOWS_1253,
                "1254" => encoding_rs::WINDOWS_1254,
                "1255" => encoding_rs::WINDOWS_1255,
                "1256" => encoding_rs::WINDOWS_1256,
                "1257" => encoding_rs::WINDOWS_1257,
                "1258" => encoding_rs::WINDOWS_1258,
                _ => return Decoder::Western,
            });
        }
        if let Some(part) = n.strip_prefix("8859_") {
            return Decoder::Rs(match part {
                "2" => encoding_rs::ISO_8859_2,
                "3" => encoding_rs::ISO_8859_3,
                "4" => encoding_rs::ISO_8859_4,
                "5" => encoding_rs::ISO_8859_5,
                "6" => encoding_rs::ISO_8859_6,
                "7" => encoding_rs::ISO_8859_7,
                "8" => encoding_rs::ISO_8859_8,
                "9" => encoding_rs::WINDOWS_1254,
                _ => return Decoder::Western,
            });
        }
        Decoder::Western
    }

    /// Text for `bytes`, with `\U+XXXX` escapes resolved.  Bad bytes become
    /// U+FFFD rather than errors.
    pub fn decode<'a>(&self, bytes: &'a [u8]) -> Cow<'a, str> {
        let text: Cow<str> = if bytes.is_ascii() {
            // The common case, with no copy.
            Cow::Borrowed(std::str::from_utf8(bytes).unwrap_or(""))
        } else {
            match self {
                Decoder::Utf8 => match std::str::from_utf8(bytes) {
                    Ok(s) => Cow::Borrowed(s),
                    // A file that claims UTF-8 and isn't: read it as Western.
                    Err(_) => encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes).0,
                },
                Decoder::Western => match std::str::from_utf8(bytes) {
                    Ok(s) => Cow::Borrowed(s),
                    Err(_) => encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes).0,
                },
                Decoder::Rs(e) => e.decode_without_bom_handling(bytes).0,
                Decoder::Dos(table) => {
                    let upper: Vec<char> = table.chars().collect();
                    Cow::Owned(
                        bytes
                            .iter()
                            .map(|&b| {
                                if b < 128 {
                                    b as char
                                } else {
                                    upper.get(b as usize - 128).copied().unwrap_or('\u{FFFD}')
                                }
                            })
                            .collect(),
                    )
                }
            }
        };
        if text.contains("\\U+") { Cow::Owned(unescape(&text)) } else { text }
    }
}

/// Resolves `\U+XXXX` (four hex digits) into the character.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find("\\U+") {
        out.push_str(&rest[..at]);
        let hex = rest.get(at + 3..at + 7).unwrap_or("");
        match u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
            Some(c) if hex.len() == 4 => {
                out.push(c);
                rest = &rest[at + 7..];
            }
            _ => {
                out.push_str("\\U+");
                rest = &rest[at + 3..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_pages() {
        let cyr = Decoder::from_codepage("ANSI_1251");
        assert_eq!(cyr.decode(&[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]), "Привет");
        let west = Decoder::from_codepage("ANSI_1252");
        assert_eq!(west.decode(&[0x43, 0x61, 0x66, 0xE9]), "Café");
        // UTF-8 written into a file that names a code page.
        assert_eq!(west.decode("Café".as_bytes()), "Café");
        let jp = Decoder::from_codepage("ANSI_932");
        assert_eq!(jp.decode(&[0x93, 0xFA, 0x96, 0x7B]), "日本");
        let dos = Decoder::from_codepage("DOS437");
        assert_eq!(dos.decode(&[0x80, 0x81, b'a']), "Çüa");
        assert_eq!(Decoder::from_codepage("unknown").decode(b"abc"), "abc");
        assert_eq!(Decoder::Utf8.decode(&[b'a', 0xFF]), "a\u{FF}");
    }

    #[test]
    fn unicode_escapes() {
        let d = Decoder::Utf8;
        assert_eq!(d.decode(b"a\\U+00E9b"), "aéb");
        assert_eq!(d.decode(b"\\U+4E2D\\U+6587"), "中文");
        assert_eq!(d.decode(b"\\U+12"), "\\U+12");
        assert_eq!(d.decode(b"\\U+ZZZZ x"), "\\U+ZZZZ x");
        assert_eq!(d.decode(b"\\U+"), "\\U+");
    }
}
