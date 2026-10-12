// Seeds one fuzz target's corpus from the test corpus (local/assets).
//
//   deno run --allow-read --allow-write tools/fuzz-seed.ts \
//     <assets dir> <corpus dir> <extension list> <per extension> <max bytes>
//
// The extension list is what the target binary prints with
// SLERP_FUZZ_LIST_EXTS=1, one per line; a file's line number is its
// selector (see fuzz/src/lib.rs).  Each seed is the selector (one byte, or
// two little-endian ones past 256 extensions), a zero flags byte, then a
// sample file whose name ends in that extension (the longest match wins).
// Up to <per extension> files are taken per extension, spread evenly over
// the samples sorted by size so small and middling ones both come, none
// over <max bytes>.  Seeds are named by content hash, so seeding twice adds
// nothing.

const [assets, corpus, listFile, perArg, maxArg] = Deno.args;
if (!assets || !corpus || !listFile) {
  console.error("usage: fuzz-seed.ts <assets> <corpus> <exts file> [per ext] [max bytes]");
  Deno.exit(2);
}
const per = Number(perArg ?? 40);
const maxBytes = Number(maxArg ?? 600_000);
const exts = (await Deno.readTextFile(listFile)).split("\n").map((s) => s.trim().toLowerCase()).filter((s) => s !== "");
const wide = exts.length > 256;

/** Every regular file under `dir`, following links, with its size. */
async function* walk(dir: string): AsyncGenerator<{ path: string; size: number }> {
  let entries: Deno.DirEntry[];
  try {
    entries = await Array.fromAsync(Deno.readDir(dir));
  } catch {
    return;
  }
  for (const e of entries) {
    const path = `${dir}/${e.name}`;
    let info: Deno.FileInfo;
    try {
      info = await Deno.stat(path);
    } catch {
      continue;
    }
    if (info.isDirectory) yield* walk(path);
    else if (info.isFile && info.size > 0 && info.size <= maxBytes) yield { path, size: info.size };
  }
}

// 1. Sample files by the longest extension of the list they end in.
const byExt = new Map<number, { path: string; size: number }[]>();
for await (const f of walk(assets)) {
  const name = f.path.slice(f.path.lastIndexOf("/") + 1).toLowerCase();
  let best = -1;
  exts.forEach((e, i) => {
    if (e !== "" && name.endsWith(`.${e}`) && (best < 0 || e.length > exts[best].length)) best = i;
  });
  if (best >= 0) {
    if (!byExt.has(best)) byExt.set(best, []);
    byExt.get(best)!.push(f);
  }
}

// 2. An even spread of each extension's samples, written with their header.
await Deno.mkdir(corpus, { recursive: true });
let written = 0;
for (const [index, files] of byExt) {
  files.sort((a, b) => a.size - b.size || a.path.localeCompare(b.path));
  const step = Math.max(1, files.length / per);
  const picked = new Set<number>();
  for (let k = 0; k < Math.min(per, files.length); k++) picked.add(Math.floor(k * step));
  for (const k of picked) {
    const body = await Deno.readFile(files[k].path);
    const head = wide ? [index & 0xff, index >> 8, 0] : [index, 0];
    const seed = new Uint8Array(head.length + body.length);
    seed.set(head);
    seed.set(body, head.length);
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-1", seed));
    const hex = Array.from(digest.slice(0, 10), (b) => b.toString(16).padStart(2, "0")).join("");
    await Deno.writeFile(`${corpus}/seed-${hex}`, seed);
    written++;
  }
}
const target = corpus.slice(corpus.lastIndexOf("/") + 1);
console.log(`${target}: ${written} seeds for ${byExt.size} of ${exts.length} extensions`);
