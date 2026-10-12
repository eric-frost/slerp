//! A tiny single-stroke font for DXF text: Dr. A. V. Hershey's Roman
//! Simplex, drawn as polylines.  CAD programs draw text with stroke fonts
//! of their own (AutoCAD's SHX files), and the exact shapes matter little in
//! a thumbnail; what matters is that text takes up the right room and reads
//! as text.
//!
//! The Hershey fonts were created by Dr. A. V. Hershey at the U.S. National
//! Bureau of Standards and are in the public domain.  Their data format was
//! created by James Hurt of Cognition, Inc.  The distribution terms ask that
//! these two acknowledgements travel with the data.

use glam::DVec2;

/// Glyphs for the characters 32 to 127, one per line.  Each starts with the
/// glyph's left and right bounds, then pairs of coordinates; every
/// character is an offset from 'R'.  A pair of " R" lifts the pen.
const GLYPHS: &str = r##"
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

/// Latin-1 Supplement letters U+00C0 to U+00FF as the plain letter drawn
/// instead: the font has no accents.
const LATIN1: &str = "AAAAAAACEEEEIIIIDNOOOOOxOUUUUYPsaaaaaaaceeeeiiiidnooooo/ouuuuypy";
/// The same for Latin Extended-A, U+0100 to U+017F.
const LATIN_A: &str = concat!(
    "AaAaAaCcCcCcCcDd",
    "DdEeEeEeEeEeGgGg",
    "GgGgHhHhIiIiIiIi",
    "IiIiJjKkkLlLlLlL",
    "lLlNnNnNnnNnOoOo",
    "OoOoRrRrRrSsSsSs",
    "SsTtTtTtUuUuUuUu",
    "UuUuWwYyYZzZzZzs",
);

/// Where a glyph's strokes are in its line of `GLYPHS`: the bounds and the
/// pen-down runs, ready to turn into points.
fn glyph_line(index: usize) -> Option<&'static [u8]> {
    GLYPHS.trim_start_matches('\n').lines().nth(index).map(str::as_bytes)
}

/// Height of a capital letter in the font's own units, which every
/// coordinate is divided by so that text height 1 means capitals 1 high.
const CAP: f64 = 21.0;

/// Calls `stroke` with each polyline that draws `c`, with the character's
/// origin (the left end of its baseline) at (0, 0) and capitals 1 high, and
/// returns how far the pen moves on afterwards.  Characters the font lacks
/// come out as a small box, so that text still takes up its room.
pub fn glyph(c: char, mut stroke: impl FnMut(&[DVec2])) -> f64 {
    let c = plain_letter(c);
    match c {
        '\u{B0}' => {
            // A degree sign: a small ring raised to the top.
            ring(&mut stroke, 0.25, 0.8, 0.2);
            return 0.6;
        }
        '\u{B1}' => {
            stroke(&[DVec2::new(0.3, 0.55), DVec2::new(0.3, 0.2)]);
            stroke(&[DVec2::new(0.12, 0.38), DVec2::new(0.48, 0.38)]);
            stroke(&[DVec2::new(0.12, 0.0), DVec2::new(0.48, 0.0)]);
            return 0.6;
        }
        '\u{2300}' | '\u{D8}' | '\u{F8}' => {
            // A diameter sign (and Ø): a circle with a slash.
            ring(&mut stroke, 0.38, 0.5, 0.38);
            stroke(&[DVec2::new(0.0, -0.05), DVec2::new(0.76, 1.05)]);
            return 0.9;
        }
        _ => {}
    }
    let code = c as u32;
    let line = if (32..128).contains(&code) { glyph_line((code - 32) as usize) } else { None };
    let Some(line) = line.filter(|l| l.len() >= 2) else {
        let w = 0.5;
        stroke(&[
            DVec2::new(0.05, 0.0),
            DVec2::new(w, 0.0),
            DVec2::new(w, 0.7),
            DVec2::new(0.05, 0.7),
            DVec2::new(0.05, 0.0),
        ]);
        return 0.75;
    };
    let left = line[0] as i32 - 82;
    let right = line[1] as i32 - 82;
    let mut run: Vec<DVec2> = Vec::new();
    for pair in line[2..].chunks_exact(2) {
        if pair == b" R" {
            if run.len() > 1 {
                stroke(&run);
            }
            run.clear();
            continue;
        }
        // Hershey's y runs downwards, from the middle of the capitals: the
        // baseline is 9 below it.
        let x = (pair[0] as i32 - 82 - left) as f64 / CAP;
        let y = (82 - pair[1] as i32 + 9) as f64 / CAP;
        run.push(DVec2::new(x, y));
    }
    if run.len() > 1 {
        stroke(&run);
    } else if run.len() == 1 {
        // A lone point (the dot of an i, a full stop) as a tiny stroke.
        stroke(&[run[0], run[0] + DVec2::new(0.02, 0.0)]);
    }
    ((right - left) as f64 / CAP).max(0.1)
}

/// The width a character takes, without drawing it.
pub fn advance(c: char) -> f64 {
    glyph(c, |_| {})
}

/// Maps accented Latin letters to the plain ones the font has.
fn plain_letter(c: char) -> char {
    let code = c as u32;
    let table = match code {
        0xC0..=0xFF if !matches!(code, 0xD8 | 0xF8) => Some((LATIN1, 0xC0)),
        0x100..=0x17F => Some((LATIN_A, 0x100)),
        _ => None,
    };
    match table {
        Some((letters, base)) => letters.as_bytes().get((code - base) as usize).map_or(c, |&b| b as char),
        None => c,
    }
}

/// An octagon centred at (`cx`, `cy`) with radius `r`.
fn ring(stroke: &mut impl FnMut(&[DVec2]), cx: f64, cy: f64, r: f64) {
    let pts: Vec<DVec2> = (0..=12)
        .map(|i| {
            let a = i as f64 * std::f64::consts::TAU / 12.0;
            DVec2::new(cx + r * a.cos(), cy + r * a.sin())
        })
        .collect();
    stroke(&pts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_cover_their_ranges() {
        assert_eq!(GLYPHS.trim_start_matches('\n').lines().count(), 96);
        assert_eq!(LATIN1.len(), 64);
        assert_eq!(LATIN_A.len(), 128);
    }

    #[test]
    fn capitals_are_one_high() {
        let mut top: f64 = 0.0;
        let adv = glyph('A', |s| s.iter().for_each(|p| top = top.max(p.y)));
        assert!((top - 1.0).abs() < 0.05, "{top}");
        assert!(adv > 0.5 && adv < 1.2);
        assert!(advance(' ') > 0.3);
    }

    #[test]
    fn accents_and_unknown_characters() {
        assert_eq!(advance('é'), advance('e'));
        assert!(advance('Ж') > 0.0);
        let mut strokes = 0;
        glyph('\u{B0}', |_| strokes += 1);
        assert_eq!(strokes, 1);
    }
}
