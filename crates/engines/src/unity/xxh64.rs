//! XXH64, the hash Unity uses for the file ids of objects inside imported
//! models when no table of them is kept: a mesh called `Body` in an FBX
//! file is `xxh64("Type:Mesh->Body0")` read as a signed number.

const P1: u64 = 11400714785074694791;
const P2: u64 = 14029467366897019727;
const P3: u64 = 1609587929392839161;
const P4: u64 = 9650029242287828579;
const P5: u64 = 2870177450012600261;

fn round(acc: u64, input: u64) -> u64 {
    acc.wrapping_add(input.wrapping_mul(P2)).rotate_left(31).wrapping_mul(P1)
}

fn merge(acc: u64, v: u64) -> u64 {
    (acc ^ round(0, v)).wrapping_mul(P1).wrapping_add(P4)
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap_or([0; 8]))
}

fn u32_at(b: &[u8], i: usize) -> u64 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap_or([0; 4])) as u64
}

/// XXH64 of `data` with seed 0.
pub(crate) fn xxh64(data: &[u8]) -> u64 {
    let n = data.len();
    let mut i = 0;
    let mut h = if n >= 32 {
        let (mut v1, mut v2, mut v3, mut v4) = (P1.wrapping_add(P2), P2, 0u64, 0u64.wrapping_sub(P1));
        while i + 32 <= n {
            v1 = round(v1, u64_at(data, i));
            v2 = round(v2, u64_at(data, i + 8));
            v3 = round(v3, u64_at(data, i + 16));
            v4 = round(v4, u64_at(data, i + 24));
            i += 32;
        }
        let mut h = v1
            .rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
        h = merge(h, v1);
        h = merge(h, v2);
        h = merge(h, v3);
        merge(h, v4)
    } else {
        P5
    };
    h = h.wrapping_add(n as u64);
    while i + 8 <= n {
        h ^= round(0, u64_at(data, i));
        h = h.rotate_left(27).wrapping_mul(P1).wrapping_add(P4);
        i += 8;
    }
    if i + 4 <= n {
        h ^= u32_at(data, i).wrapping_mul(P1);
        h = h.rotate_left(23).wrapping_mul(P2).wrapping_add(P3);
        i += 4;
    }
    while i < n {
        h ^= (data[i] as u64).wrapping_mul(P5);
        h = h.rotate_left(11).wrapping_mul(P1);
        i += 1;
    }
    h ^= h >> 33;
    h = h.wrapping_mul(P2);
    h ^= h >> 29;
    h = h.wrapping_mul(P3);
    h ^ (h >> 32)
}

/// The file id Unity gives the object `name` of type `kind` (`Mesh`,
/// `Material`, ...) inside an imported model.
pub(crate) fn model_file_id(kind: &str, name: &str) -> i64 {
    xxh64(format!("Type:{kind}->{name}0").as_bytes()) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_reference_and_unity() {
        assert_eq!(xxh64(b""), 0xEF46DB3751D8E999);
        assert_eq!(xxh64(b"abc"), 0x44BC2CF5AD770999);
        // Checked against fileIDs Unity wrote for meshes in ML-Agents'
        // WalkerRagdoll.fbx, and a cube's well-known id.
        assert_eq!(model_file_id("Mesh", "Cube"), -5495902117074765545);
        assert_eq!(model_file_id("Mesh", "hipsMesh"), 1736225472013479476);
        // Longer than 32 bytes, through the four-lane loop.
        assert_eq!(xxh64(b"Type:Mesh->a rather long mesh name to cross 32 bytes0"), 6302699053639687695);
    }
}
