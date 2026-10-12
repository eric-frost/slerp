//! Apple's 52 ARKit face shapes as MetaHuman raw controls, so a face panel
//! with the names lip-sync and face-capture tools use can drive the real
//! rig.  The table is chalk's (`tools/humans/bake.py`, which bakes each
//! shape by setting these controls to full strength), so the viewer's face
//! matches chalk's baked heads.  ARKit's Left and MetaHuman's L both mean
//! the character's own left; an eye looking "in" looks toward the nose.

/// Each ARKit shape and the raw controls (the part of the name after
/// `CTRL_expressions.`) it sets to its own weight.
pub const ARKIT_CONTROLS: &[(&str, &[&str])] = &[
    ("browDownLeft", &["browDownL", "browLateralL"]),
    ("browDownRight", &["browDownR", "browLateralR"]),
    ("browInnerUp", &["browRaiseInL", "browRaiseInR"]),
    ("browOuterUpLeft", &["browRaiseOuterL"]),
    ("browOuterUpRight", &["browRaiseOuterR"]),
    ("cheekPuff", &["mouthCheekBlowL", "mouthCheekBlowR"]),
    ("cheekSquintLeft", &["eyeCheekRaiseL"]),
    ("cheekSquintRight", &["eyeCheekRaiseR"]),
    ("eyeBlinkLeft", &["eyeBlinkL"]),
    ("eyeBlinkRight", &["eyeBlinkR"]),
    ("eyeLookDownLeft", &["eyeLookDownL"]),
    ("eyeLookDownRight", &["eyeLookDownR"]),
    ("eyeLookInLeft", &["eyeLookRightL"]),
    ("eyeLookInRight", &["eyeLookLeftR"]),
    ("eyeLookOutLeft", &["eyeLookLeftL"]),
    ("eyeLookOutRight", &["eyeLookRightR"]),
    ("eyeLookUpLeft", &["eyeLookUpL"]),
    ("eyeLookUpRight", &["eyeLookUpR"]),
    ("eyeSquintLeft", &["eyeSquintInnerL"]),
    ("eyeSquintRight", &["eyeSquintInnerR"]),
    ("eyeWideLeft", &["eyeWidenL"]),
    ("eyeWideRight", &["eyeWidenR"]),
    ("jawForward", &["jawFwd"]),
    ("jawLeft", &["jawLeft"]),
    ("jawOpen", &["jawOpen"]),
    ("jawRight", &["jawRight"]),
    ("mouthClose", &["mouthLipsTogetherUL", "mouthLipsTogetherUR", "mouthLipsTogetherDL", "mouthLipsTogetherDR"]),
    ("mouthDimpleLeft", &["mouthDimpleL"]),
    ("mouthDimpleRight", &["mouthDimpleR"]),
    ("mouthFrownLeft", &["mouthCornerDepressL"]),
    ("mouthFrownRight", &["mouthCornerDepressR"]),
    ("mouthFunnel", &["mouthFunnelUL", "mouthFunnelUR", "mouthFunnelDL", "mouthFunnelDR"]),
    ("mouthLeft", &["mouthLeft"]),
    ("mouthLowerDownLeft", &["mouthLowerLipDepressL"]),
    ("mouthLowerDownRight", &["mouthLowerLipDepressR"]),
    ("mouthPressLeft", &["mouthLipsPressL"]),
    ("mouthPressRight", &["mouthLipsPressR"]),
    (
        "mouthPucker",
        &[
            "mouthLipsPurseUL",
            "mouthLipsPurseUR",
            "mouthLipsPurseDL",
            "mouthLipsPurseDR",
            "mouthLipsTowardsUL",
            "mouthLipsTowardsUR",
            "mouthLipsTowardsDL",
            "mouthLipsTowardsDR",
        ],
    ),
    ("mouthRight", &["mouthRight"]),
    ("mouthRollLower", &["mouthLowerLipRollInL", "mouthLowerLipRollInR"]),
    ("mouthRollUpper", &["mouthUpperLipRollInL", "mouthUpperLipRollInR"]),
    ("mouthShrugLower", &["jawChinRaiseDL", "jawChinRaiseDR"]),
    ("mouthShrugUpper", &["jawChinRaiseUL", "jawChinRaiseUR"]),
    ("mouthSmileLeft", &["mouthCornerPullL"]),
    ("mouthSmileRight", &["mouthCornerPullR"]),
    ("mouthStretchLeft", &["mouthStretchL"]),
    ("mouthStretchRight", &["mouthStretchR"]),
    ("mouthUpperUpLeft", &["mouthUpperLipRaiseL"]),
    ("mouthUpperUpRight", &["mouthUpperLipRaiseR"]),
    ("noseSneerLeft", &["noseWrinkleL"]),
    ("noseSneerRight", &["noseWrinkleR"]),
    ("tongueOut", &["tongueOut"]),
];

/// The raw controls ARKit shape `name` sets, or `None` for a name that
/// isn't one of the 52.
pub fn arkit_controls(name: &str) -> Option<&'static [&'static str]> {
    ARKIT_CONTROLS.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, c)| *c)
}

/// The face area a raw control belongs to, from its name's first word:
/// "brow", "eye", "eyelashes", "ear", "nose", "mouth", "jaw", "tongue",
/// "teeth", "neck", or "other".  `CTRL_expressions.` and similar prefixes
/// are ignored.
pub fn control_group(name: &str) -> &'static str {
    let base = name.rsplit('.').next().unwrap_or(name);
    let word: String = base.chars().take_while(|c| c.is_ascii_lowercase()).collect();
    match word.as_str() {
        "brow" => "Brows",
        "eye" => "Eyes",
        "eyelashes" => "Eyelashes",
        "ear" => "Ears",
        "nose" => "Nose",
        "mouth" => "Mouth",
        "jaw" => "Jaw",
        "tongue" => "Tongue",
        "teeth" => "Teeth",
        "neck" => "Neck",
        _ => "Other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_52_shapes_once() {
        assert_eq!(ARKIT_CONTROLS.len(), 52);
        let mut names: Vec<&str> = ARKIT_CONTROLS.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 52);
        assert_eq!(arkit_controls("JAWOPEN"), Some(&["jawOpen"][..]));
        assert_eq!(arkit_controls("frown"), None);
    }

    #[test]
    fn groups_by_first_word() {
        assert_eq!(control_group("CTRL_expressions.browDownL"), "Brows");
        assert_eq!(control_group("CTRL_expressions.eyelashesUpINL"), "Eyelashes");
        assert_eq!(control_group("CTRL_expressions.eyeBlinkL"), "Eyes");
        assert_eq!(control_group("CTRL_expressions.jawOpen"), "Jaw");
        assert_eq!(control_group("head.qx"), "Other");
    }
}
