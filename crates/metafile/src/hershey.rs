//! A single-stroke fallback font: Dr. A. V. Hershey's Roman Simplex.  A
//! metafile names fonts that may not exist on the machine drawing it, and
//! thumbnailers cannot rely on any being installed, so text that no real
//! font can draw becomes these line glyphs.  They are 3 KB of data, scale to
//! any size and read as text, which is what a thumbnail needs.
//!
//! The Hershey fonts were created by Dr. A. V. Hershey at the U.S. National
//! Bureau of Standards and are in the public domain.  Their data format was
//! created by James Hurt of Cognition, Inc.  The distribution terms ask that
//! these two acknowledgements travel with the data.

/// Glyphs for the characters 32 to 126, one per line.  Each starts with the
/// glyph's left and right bounds, then pairs of coordinates; every
/// character is an offset from 'R'.  A pair of " R" lifts the pen.
pub const GLYPHS: &str = r##"
JZ
MWRFRT RRYQZR[SZRY
JZNFNM RVFVM
H]SBLb RYBRb RLOZO RKUYU
H\PBP_ RTBT_ RYIWGTFPFMGKIKKLMMNOOUQWRXSYUYXWZT[P[MZKX
F^[FI[ RNFPHPJOLMMKMIKIIJGLFNFPGSHVHYG[F RWTUUTWTYV[X[ZZ[X[VYTWT
E_\O\N[MZMYNXPVUTXRZP[L[JZIYHWHUISJRQNRMSKSIRGPFNGMIMKNNPQUXWZY[[[\Z\Y
MWRHQGRFSGSIRKQL
KYVBTDRGPKOPOTPYR]T`Vb
KYNBPDRGTKUPUTTYR]P`Nb
JZRFRR RMIWO RWIMO
E_RIR[ RIR[R
MWSZR[QZRYSZS\R^Q_
E_IR[R
MWRYQZR[SZRY
G][BIb
H\QFNGLJKOKRLWNZQ[S[VZXWYRYOXJVGSFQF
H\NJPISFS[
H\LKLJMHNGPFTFVGWHXJXLWNUQK[Y[
H\MFXFRNUNWOXPYSYUXXVZS[P[MZLYKW
H\UFKTZT RUFU[
H\WFMFLOMNPMSMVNXPYSYUXXVZS[P[MZLYKW
H\XIWGTFRFOGMJLOLTMXOZR[S[VZXXYUYTXQVOSNRNOOMQLT
H\YFO[ RKFYF
H\PFMGLILKMMONSOVPXRYTYWXYWZT[P[MZLYKWKTLRNPQOUNWMXKXIWGTFPF
H\XMWPURRSQSNRLPKMKLLINGQFRFUGWIXMXRWWUZR[P[MZLX
MWRMQNROSNRM RRYQZR[SZRY
MWRMQNROSNRM RSZR[QZRYSZS\R^Q_
F^ZIJRZ[
E_IO[O RIU[U
F^JIZRJ[
I[LKLJMHNGPFTFVGWHXJXLWNVORQRT RRYQZR[SZRY
E`WNVLTKQKOLNMMPMSNUPVSVUUVS RQKOMNPNSOUPV RWKVSVUXVZV\T]Q]O\L[JYHWGTFQFNGLHJJILHOHRIUJWLYNZQ[T[WZYYZX RXKWSWUXV
I[RFJ[ RRFZ[ RMTWT
G\KFK[ RKFTFWGXHYJYLXNWOTP RKPTPWQXRYTYWXYWZT[K[
H]ZKYIWGUFQFOGMILKKNKSLVMXOZQ[U[WZYXZV
G\KFK[ RKFRFUGWIXKYNYSXVWXUZR[K[
H[LFL[ RLFYF RLPTP RL[Y[
HZLFL[ RLFYF RLPTP
H]ZKYIWGUFQFOGMILKKNKSLVMXOZQ[U[WZYXZVZS RUSZS
G]KFK[ RYFY[ RKPYP
NVRFR[
JZVFVVUYTZR[P[NZMYLVLT
G\KFK[ RYFKT RPOY[
HYLFL[ RL[X[
F^JFJ[ RJFR[ RZFR[ RZFZ[
G]KFK[ RKFY[ RYFY[
G]PFNGLIKKJNJSKVLXNZP[T[VZXXYVZSZNYKXIVGTFPF
G\KFK[ RKFTFWGXHYJYMXOWPTQKQ
G]PFNGLIKKJNJSKVLXNZP[T[VZXXYVZSZNYKXIVGTFPF RSWY]
G\KFK[ RKFTFWGXHYJYLXNWOTPKP RRPY[
H\YIWGTFPFMGKIKKLMMNOOUQWRXSYUYXWZT[P[MZKX
JZRFR[ RKFYF
G]KFKULXNZQ[S[VZXXYUYF
I[JFR[ RZFR[
F^HFM[ RRFM[ RRFW[ R\FW[
H\KFY[ RYFK[
I[JFRPR[ RZFRP
H\YFK[ RKFYF RK[Y[
KYOBOb RPBPb ROBVB RObVb
KYKFY^
KYTBTb RUBUb RNBUB RNbUb
JZPLRITL RMORJWO RRJR[
JZJ]Z]
MWSFRGQIQKRLSKRJ
I\XMX[ RXPVNTMQMONMPLSLUMXOZQ[T[VZXX
H[LFL[ RLPNNPMSMUNWPXSXUWXUZS[P[NZLX
I[XPVNTMQMONMPLSLUMXOZQ[T[VZXX
I\XFX[ RXPVNTMQMONMPLSLUMXOZQ[T[VZXX
I[LSXSXQWOVNTMQMONMPLSLUMXOZQ[T[VZXX
MYWFUFSGRJR[ ROMVM
I\XMX]W`VaTbQbOa RXPVNTMQMONMPLSLUMXOZQ[T[VZXX
I\MFM[ RMQPNRMUMWNXQX[
NVQFRGSFREQF RRMR[
MWRFSGTFSERF RSMS^RaPbNb
IZMFM[ RWMMW RQSX[
NVRFR[
CaGMG[ RGQJNLMOMQNRQR[ RRQUNWMZM\N]Q][
I\MMM[ RMQPNRMUMWNXQX[
I\QMONMPLSLUMXOZQ[T[VZXXYUYSXPVNTMQM
H[LMLb RLPNNPMSMUNWPXSXUWXUZS[P[NZLX
I\XMXb RXPVNTMQMONMPLSLUMXOZQ[T[VZXX
KXOMO[ ROSPPRNTMWM
J[XPWNTMQMNNMPNRPSUTWUXWXXWZT[Q[NZMX
MYRFRWSZU[W[ ROMVM
I\MMMWNZP[S[UZXW RXMX[
JZLMR[ RXMR[
G]JMN[ RRMN[ RRMV[ RZMV[
J[MMX[ RXMM[
JZLMR[ RXMR[P_NaLbKb
J[XMM[ RMMXM RM[X[
KYTBRCQDPFPHQJRKSMSOQQ RRCQEQGRISJTLTNSPORSTTVTXSZR[Q]Q_Ra RQSSUSWRYQZP\P^Q`RaTb
NVRBRb
KYPBRCSDTFTHSJRKQMQOSQ RRCSESGRIQJPLPNQPURQTPVPXQZR[S]S_Ra RSSQUQWRYSZT\T^S`RaPb
F^IUISJPLONOPPTSVTXTZS[Q RISJQLPNPPQTTVUXUZT[Q[O
KYQFOGNINKOMQNSNUMVKVIUGSFQF
"##;

/// Latin-1 letters 0xC0 to 0xFF drawn as the plain letter they decorate.
const LATIN1_FOLD: &str = "AAAAAAACEEEEIIIIDNOOOOOxOUUUUYPBaaaaaaaceeeeiiiidnooooo/ouuuuypy";

/// Latin Extended-A, 0x100 to 0x17F, likewise.
const LATIN_EXT_A_FOLD: &str = "AaAaAaCcCcCcCcDdDdEeEeEeEeEeGgGgGgGgHhHhIiIiIiIiIiIiJjKkkLlLlLlLlLlNnNnNnnNnOoOoOoOoRrRrRrSsSsSsSsTtTtTtUuUuUuUuUuUuWwYyYZzZzZzf";

/// A glyph in em units: x from the glyph's left bound, y up from the
/// baseline, drawn as polylines.
pub struct Glyph {
    pub advance: f32,
    pub strokes: Vec<Vec<(f32, f32)>>,
}

/// Hershey's cap height is 21 units; Latin fonts' capitals stand about
/// 0.72 em tall, which fixes the scale.
const UNIT: f32 = 0.716 / 21.0;
/// The simplex letters run wide beside typical sans fonts.
const WIDTH: f32 = 0.88;

fn table() -> &'static [Glyph] {
    static TABLE: std::sync::OnceLock<Vec<Glyph>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        GLYPHS
            .lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let b = line.as_bytes();
                let val = |c: u8| c as i32 - b'R' as i32;
                let (left, right) = (val(b[0]), val(b[1]));
                let mut strokes: Vec<Vec<(f32, f32)>> = Vec::new();
                let mut cur: Vec<(f32, f32)> = Vec::new();
                for pair in b[2..].chunks_exact(2) {
                    if pair == b" R" {
                        if cur.len() > 1 {
                            strokes.push(std::mem::take(&mut cur));
                        }
                        cur.clear();
                        continue;
                    }
                    let (x, y) = (val(pair[0]) - left, 9 - val(pair[1]));
                    cur.push((x as f32 * UNIT * WIDTH, y as f32 * UNIT));
                }
                if cur.len() > 1 {
                    strokes.push(cur);
                } else if cur.len() == 1 {
                    // A lone point is a dot.
                    strokes.push(vec![cur[0], (cur[0].0 + 0.01, cur[0].1)]);
                }
                Glyph { advance: (right - left) as f32 * UNIT * WIDTH, strokes }
            })
            .collect()
    })
}

/// Maps a character to the plain letter or punctuation mark whose
/// glyph stands in for it.
fn fold(c: char) -> char {
    match c as u32 {
        0xA0 => ' ',
        0xB7 | 0x2022 | 0x2027 => '*',
        0xC0..=0xFF => LATIN1_FOLD.chars().nth(c as usize - 0xC0).unwrap_or(c),
        0x100..=0x17F => LATIN_EXT_A_FOLD.chars().nth(c as usize - 0x100).unwrap_or(c),
        0x2018 | 0x2019 | 0x201A | 0x2032 => '\'',
        0x201C | 0x201D | 0x201E | 0x2033 => '"',
        0x2010..=0x2015 | 0x2212 => '-',
        0x2026 => '.',
        0x20AC => 'E',
        0xA3 => 'L',
        0xA5 => 'Y',
        0xA9 => 'C',
        0xAE => 'R',
        _ => c,
    }
}

/// The glyph for a character, or `None` when the font has no stand-in.
pub fn glyph(c: char) -> Option<&'static Glyph> {
    let t = table();
    let c = fold(c);
    let i = match c as u32 {
        0x20..=0x7E => c as usize - 0x20,
        0xB0 => 95,
        _ => return None,
    };
    t.get(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_covers_ascii() {
        assert_eq!(table().len(), 96);
        for c in ' '..='~' {
            let g = glyph(c).unwrap();
            assert!(g.advance > 0.0, "{c}");
            assert_eq!(c == ' ', g.strokes.is_empty(), "{c}");
        }
        assert!(glyph('\u{4E2D}').is_none());
    }

    #[test]
    fn accents_fold_to_their_letters() {
        for (a, b) in [('\u{E9}', 'e'), ('\u{C4}', 'A'), ('\u{15F}', 's'), ('\u{201C}', '"'), ('\u{DF}', 'B')] {
            assert_eq!(glyph(a).map(|g| g as *const _), glyph(b).map(|g| g as *const _), "{a}");
        }
    }

    #[test]
    fn capitals_are_cap_height() {
        let a = glyph('H').unwrap();
        let top = a.strokes.iter().flatten().map(|p| p.1).fold(0.0, f32::max);
        assert!((top - 0.716).abs() < 0.01, "{top}");
    }
}
