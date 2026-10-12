// A GPU radix sort of 24-bit keys with 32-bit values, 8 bits per pass,
// for drawing Gaussian splats back to front.
//
// It uses only workgroup memory, barriers and workgroup atomics, no
// subgroup operations and no waiting on other workgroups, so it runs alike
// on Vulkan, Metal, Direct3D 12 and lavapipe, none of which promise that
// one workgroup makes progress while another waits.  Each pass is three
// dispatches:
//
// 1. `cs_count`: every tile of 4096 keys counts its digits.
// 2. `cs_scan`: one workgroup per digit turns the tiles' counts into
//    where each tile's keys of that digit start, and the digit's total.
// 3. `cs_scatter`: every tile adds the totals of smaller digits and moves
//    its keys there.  It ranks keys within the tile in their original
//    order, which keeps the sort stable, as a least-significant-digit
//    radix sort needs.

struct SortParams {
    count: u32,
    shift: u32,
    tiles: u32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> sp: SortParams;
@group(0) @binding(1) var<storage, read> keys_in: array<u32>;
@group(0) @binding(2) var<storage, read> values_in: array<u32>;
@group(0) @binding(3) var<storage, read_write> keys_out: array<u32>;
@group(0) @binding(4) var<storage, read_write> values_out: array<u32>;
// Per digit, per tile: the tile's count of the digit, then (after
// `cs_scan`) where the tile's first key of that digit goes among the
// digit's keys.
@group(0) @binding(5) var<storage, read_write> counts: array<u32>;
// Keys per digit.
@group(0) @binding(6) var<storage, read_write> totals: array<u32>;

const WG: u32 = 256u;
const ITEMS: u32 = 16u;
const TILE: u32 = 4096u;

var<workgroup> histogram: array<atomic<u32>, 256>;
var<workgroup> scratch: array<u32, 256>;
var<workgroup> offsets: array<atomic<u32>, 256>;
var<workgroup> digits: array<u32, 256>;

fn digit(key: u32) -> u32 {
    return (key >> sp.shift) & 255u;
}

// The exclusive prefix sum of `v` over the workgroup, and the total.
// Every invocation must call it.
fn prefix_sum(lid: u32, v: u32) -> vec2u {
    scratch[lid] = v;
    workgroupBarrier();
    for (var step = 1u; step < WG; step <<= 1u) {
        var add = 0u;
        if lid >= step {
            add = scratch[lid - step];
        }
        workgroupBarrier();
        scratch[lid] += add;
        workgroupBarrier();
    }
    let inclusive = scratch[lid];
    let total = scratch[WG - 1u];
    workgroupBarrier();
    return vec2u(inclusive - v, total);
}

@compute @workgroup_size(256)
fn cs_count(@builtin(local_invocation_index) lid: u32, @builtin(workgroup_id) wid: vec3u) {
    atomicStore(&histogram[lid], 0u);
    workgroupBarrier();
    let tile = wid.x;
    for (var k = 0u; k < ITEMS; k++) {
        let i = tile * TILE + k * WG + lid;
        if i < sp.count {
            atomicAdd(&histogram[digit(keys_in[i])], 1u);
        }
    }
    workgroupBarrier();
    counts[lid * sp.tiles + tile] = atomicLoad(&histogram[lid]);
}

@compute @workgroup_size(256)
fn cs_scan(@builtin(local_invocation_index) lid: u32, @builtin(workgroup_id) wid: vec3u) {
    let row = wid.x * sp.tiles;
    var running = 0u;
    for (var first = 0u; first < sp.tiles; first += WG) {
        let t = first + lid;
        var v = 0u;
        if t < sp.tiles {
            v = counts[row + t];
        }
        let s = prefix_sum(lid, v);
        if t < sp.tiles {
            counts[row + t] = running + s.x;
        }
        running += s.y;
    }
    if lid == 0u {
        totals[wid.x] = running;
    }
}

@compute @workgroup_size(256)
fn cs_scatter(@builtin(local_invocation_index) lid: u32, @builtin(workgroup_id) wid: vec3u) {
    let tile = wid.x;
    // Where this tile's keys of each digit go: keys of smaller digits,
    // then this digit's keys in earlier tiles.
    let base = prefix_sum(lid, totals[lid]);
    atomicStore(&offsets[lid], base.x + counts[lid * sp.tiles + tile]);
    workgroupBarrier();
    for (var k = 0u; k < ITEMS; k++) {
        let i = tile * TILE + k * WG + lid;
        let valid = i < sp.count;
        var key = 0u;
        var d = 256u;
        if valid {
            key = keys_in[i];
            d = digit(key);
        }
        digits[lid] = d;
        workgroupBarrier();
        // Earlier keys of this 256 with the same digit go first.
        var rank = 0u;
        for (var j = 0u; j < lid; j++) {
            rank += select(0u, 1u, digits[j] == d);
        }
        if valid {
            let at = atomicLoad(&offsets[d]) + rank;
            keys_out[at] = key;
            values_out[at] = values_in[i];
        }
        workgroupBarrier();
        if valid {
            atomicAdd(&offsets[d], 1u);
        }
        workgroupBarrier();
    }
}
