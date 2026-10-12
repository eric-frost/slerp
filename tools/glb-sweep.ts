// Converts every 3D sample under the given folders to GLB with
// `slerp convert`, validates each written file with the Khronos glTF
// Validator (tools/validate-gltf.ts), and prints a summary: how many
// converted, validated with no errors, failed, or weren't 3D scenes, with
// timings.  A file that isn't a scene but lists parts (a WAD's maps) is
// converted through its first part.
//
// Usage: deno run -A tools/glb-sweep.ts --slerp PATH --out DIR FOLDER...
// Writes DIR/<n>.glb and DIR/report.json.

const args = [...Deno.args];
const take = (flag: string, fallback: string) => {
  const i = args.indexOf(flag);
  if (i < 0) return fallback;
  const value = args[i + 1];
  args.splice(i, 2);
  return value;
};
const slerp = take("--slerp", "local/target/release/slerp");
const out = take("--out", "local/glb-sweep");
const perFile = Number(take("--timeout", "120"));
await Deno.mkdir(out, { recursive: true });

// Companion files and pictures, which models refer to but which aren't
// scenes themselves.
const skip = new Set([
  "png", "jpg", "jpeg", "tga", "bmp", "ktx2", "ktx", "dds", "bin", "mtl", "txt", "md", "json", "license",
  "webp", "hdr", "exr", "tif", "tiff", "gif", "vtf", "vmt", "vvd", "vtx", "phy", "ani", "lit", "cfg", "html",
  "css", "js", "xml", "zip", "gz", "psd", "wav", "ogg", "mp3", "pdf", "rtf", "lmp", "sha256", "csv", "log",
  "meta", "mcx", "import", "uid", "cs", "sample", "bphys", "gd", "shader", "asset", "mat", "lit", "cdr", "vsd",
  "pcx", "uasset", "pal", "cfg", "ini", "yml", "yaml",
]);

async function* walk(dir: string): AsyncGenerator<string> {
  for await (const e of Deno.readDir(dir)) {
    const path = `${dir}/${e.name}`;
    if (e.isDirectory) yield* walk(path);
    else if (e.isFile || e.isSymlink) yield path;
  }
}

interface Result {
  file: string;
  part?: string;
  status: "written" | "not a scene" | "load failed" | "write failed" | "timeout";
  ms: number;
  /// The load and the write alone, from `slerp convert`'s report.
  loadMs?: number;
  writeMs?: number;
  bytes?: number;
  out?: string;
  message?: string;
  warnings?: number;
  errors?: number;
  validatorWarnings?: number;
}

async function run(cmd: string[], timeout: number) {
  const child = new Deno.Command(cmd[0], { args: cmd.slice(1), stdout: "piped", stderr: "piped" }).spawn();
  const timer = setTimeout(() => {
    try {
      child.kill("SIGKILL");
    } catch { /* already gone */ }
  }, timeout * 1000);
  const o = await child.output();
  clearTimeout(timer);
  const text = (b: Uint8Array) => new TextDecoder().decode(b);
  return { code: o.code, signal: o.signal, stdout: text(o.stdout), stderr: text(o.stderr) };
}

const files: string[] = [];
for (const folder of args) {
  for await (const f of walk(folder)) {
    const ext = f.split(".").pop()!.toLowerCase();
    if (f.includes("/.") || skip.has(ext) || f.endsWith("SOURCES.md")) continue;
    files.push(f);
  }
}
files.sort();
console.error(`${files.length} files`);

const results: Result[] = [];
let n = 0;
for (const file of files) {
  const glb = `${out}/${String(n++).padStart(4, "0")}.glb`;
  const convert = async (part?: string): Promise<Result> => {
    const start = performance.now();
    const cmd = [slerp, "convert", file, glb, ...(part ? ["--part", part] : [])];
    const r = await run(cmd, perFile);
    const ms = Math.round(performance.now() - start);
    const warnings = r.stderr.split("\n").filter((l) => l.startsWith("warning:")).length;
    if (r.signal) return { file, part, status: "timeout", ms, message: `killed by ${r.signal}` };
    if (r.code === 0) {
      const bytes = (await Deno.stat(glb)).size;
      const m = r.stdout.match(/loaded in (\d+) ms, written in (\d+) ms/);
      const [loadMs, writeMs] = m ? [Number(m[1]), Number(m[2])] : [undefined, undefined];
      return { file, part, status: "written", ms, loadMs, writeMs, bytes, out: glb, warnings };
    }
    const message = r.stderr.trim().split("\n").pop() ?? "";
    const notScene = message.includes("isn't a 3D scene") || message.includes("not a supported format") ||
      message.includes("not a format Slerp reads");
    // The writer fails only for an output too large for GLB or a write
    // error; anything else is the loader refusing the file.
    const writer = message.includes("too large for a GLB") || message.includes(glb);
    const status = notScene ? "not a scene" : writer ? "write failed" : "load failed";
    return { file, part, status, ms, message };
  };
  let result = await convert();
  if (result.status === "not a scene" && /\.(wad|pk3|pk7)$/i.test(file)) {
    const info = await run([slerp, "info", file], perFile);
    const parts = info.stdout.split("\n").find((l) => l.startsWith("parts: "))?.slice(7).split(" ") ?? [];
    if (parts.length > 0) result = await convert(parts[0]);
  }
  results.push(result);
  console.error(`${result.status.padEnd(11)} ${String(result.ms).padStart(6)} ms  ${file}${result.part ? ` (${result.part})` : ""}`);
}

// Validate in batches, so one bad file doesn't stop the rest.
const written = results.filter((r) => r.out);
for (let i = 0; i < written.length; i += 20) {
  const batch = written.slice(i, i + 20);
  const v = await run(
    ["deno", "run", "--allow-read", "--allow-env", "tools/validate-gltf.ts", "--json", ...batch.map((r) => r.out!)],
    600,
  );
  for (const line of v.stdout.split("\n").filter((l) => l.startsWith("{"))) {
    const j = JSON.parse(line);
    const r = batch.find((b) => b.out === j.path);
    if (!r) continue;
    r.errors = j.crash ? 1 : j.errors;
    r.validatorWarnings = j.warnings;
    if (j.errors > 0 || j.crash) r.message = j.crash ?? j.messages.slice(0, 3).join(" | ");
  }
}

// Files a batch lost (the validator stopping part-way), one at a time.
for (const r of written.filter((r) => r.errors === undefined)) {
  const v = await run(["deno", "run", "--allow-read", "--allow-env", "tools/validate-gltf.ts", "--json", r.out!], 600);
  const line = v.stdout.split("\n").find((l) => l.startsWith("{"));
  if (!line) continue;
  const j = JSON.parse(line);
  r.errors = j.crash ? 1 : j.errors;
  r.validatorWarnings = j.warnings;
  if (j.errors > 0 || j.crash) r.message = j.crash ?? j.messages.slice(0, 3).join(" | ");
}

await Deno.writeTextFile(`${out}/report.json`, JSON.stringify(results, null, 1));
const count = (s: string) => results.filter((r) => r.status === s).length;
const valid = written.filter((r) => r.errors === 0);
const times = written.map((r) => r.ms).sort((a, b) => a - b);
console.log(`files tried: ${results.length}`);
console.log(`written: ${written.length}, validated with no errors: ${valid.length}`);
console.log(
  `not 3D scenes: ${count("not a scene")}, couldn't be loaded: ${count("load failed")}, ` +
    `writer failed: ${count("write failed")}, timed out: ${count("timeout")}`,
);
if (times.length) {
  const total = times.reduce((a, b) => a + b, 0);
  console.log(
    `convert time (load + write, ms): median ${times[times.length >> 1]}, ` +
      `90th ${times[Math.floor(times.length * 0.9)]}, max ${times[times.length - 1]}, total ${Math.round(total / 1000)} s`,
  );
}
const writes = written.map((r) => r.writeMs ?? 0).sort((a, b) => a - b);
if (writes.length) {
  const mb = written.reduce((a, r) => a + (r.bytes ?? 0), 0) / 1e6;
  const total = writes.reduce((a, b) => a + b, 0);
  console.log(
    `writer alone (ms): median ${writes[writes.length >> 1]}, 90th ${writes[Math.floor(writes.length * 0.9)]}, ` +
      `max ${writes[writes.length - 1]}, total ${Math.round(total / 1000)} s for ${Math.round(mb)} MB of GLB`,
  );
}
for (const r of written.filter((r) => r.errors !== 0)) console.log(`invalid: ${r.file}: ${r.message}`);
for (const r of results.filter((r) => r.status === "write failed" || r.status === "timeout")) {
  console.log(`${r.status}: ${r.file}: ${r.message}`);
}
