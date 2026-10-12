// Times the viewer's start-up: the window showing (its placeholder, in
// builds that draw one before the GPU is ready), the GPU ready, the first
// window frame, the first frame
// showing the model, and the first frame with all of it (textures that go
// up after the first frame included), for cold starts and for the warm
// viewer.  Run behind hide-windows (windows get the class agent-test), with
// the video wallpaper held:
//
//   hide-windows.sh start; video-wallpaper hold slerp-startup
//   deno run -A tools/startup-timing.ts [--bin local/target/release/slerp] [--against other-build] [--runs 5]
//     [--xwayland] [--cold-only] [files...]
//
// Cold runs set SLERP_VIEWER_KEEP_ALIVE=0 so each process exits with its
// window.  Warm runs start one viewer that stays loaded, then hand it each
// file through `slerp view`, as a file manager would; their times count
// from the moment the warm viewer took the request, and the command's own
// hand-over time is listed beside them.  Prints a Markdown table of
// medians.

const args = [...Deno.args];
function option(name: string, fallback: string): string {
  const i = args.indexOf(name);
  if (i < 0) return fallback;
  const value = args[i + 1];
  args.splice(i, 2);
  return value;
}
const root = new URL("..", import.meta.url).pathname;
const bin = option("--bin", `${root}local/target/release/slerp`);
const runs = Number(option("--runs", "5"));
// Another build to time cold starts against, run by turns with this one so
// that load from other programs falls on both alike.
const against = option("--against", "");
const coldOnly = args.includes("--cold-only");
if (coldOnly) args.splice(args.indexOf("--cold-only"), 1);
const wayland = !args.includes("--xwayland");
if (!wayland) args.splice(args.indexOf("--xwayland"), 1);
const files = args.length > 0 ? args : [
  `${root}local/assets/gltf/khronos/Duck/glTF-Binary/Duck.glb`,
  // A checkout of chalk beside this repository, when there is one.
  `${root}../chalk/html/demos/humans/tate-tee-lod1.glb`,
  `${root}../chalk/tools/humans/characters/tate/Tate/head.dna`,
  `${root}local/assets/doom/freedoom/freedoom2.wad`,
].filter((f) => {
  try {
    return Deno.statSync(f).isFile;
  } catch {
    console.error(`skipping ${f}: not there`);
    return false;
  }
});

/** The numbers a viewer prints with SLERP_TIMING, in milliseconds. */
interface Times {
  shown?: number;
  gpu?: number;
  window?: number;
  model?: number;
  complete?: number;
  handoff?: number;
}

function parse(text: string): Times {
  const t: Times = {};
  const num = (re: RegExp) => {
    const m = text.match(re);
    return m ? Number(m[1]) : undefined;
  };
  // Builds from before these lines existed said "window ready" (the
  // moment before its first frame) and "first frame with the file".
  const old = num(/first frame with the file at (\d+) ms/);
  t.window = num(/first window frame at (\d+) ms/) ?? num(/window ready at (\d+) ms/);
  // Older builds showed the window with its first frame.
  t.shown = num(/window shown at (\d+) ms/) ?? t.window;
  t.gpu = num(/slerp view: GPU .* ready at (\d+) ms/);
  t.model = num(/first frame with the model at (\d+) ms/) ?? old;
  t.complete = num(/first frame with the file complete at (\d+) ms/) ?? old;
  t.handoff = num(/handed to the warm viewer in ([\d.]+) ms/);
  return t;
}

// Behind hide-windows, KWin sometimes stops sending a hidden Wayland window
// frame callbacks, and on NVIDIA its presents then block for good after the
// first frames (old builds too), so no run finishes.  `--xwayland` runs the
// windows through XWayland instead (on the NVIDIA GPU, which the desktop
// composites on), where they keep drawing.
function slice(extraEnv: Record<string, string>, cmdArgs: string[], program = bin) {
  const unset = wayland ? [] : ["env", "-u", "WAYLAND_DISPLAY"];
  return new Deno.Command("systemd-run", {
    args: ["--user", "--scope", "--quiet", "--slice=heavy-slerp.slice", ...unset, program, ...cmdArgs],
    // Times count work done, as on a visible window, rather than the
    // compositor's pacing of a hidden one.
    env: { SLERP_TIMING: "1", SLERP_NO_VSYNC: "1", ...(wayland ? {} : { SLERP_GPU: "nvidia" }), ...extraEnv },
    stdout: "null",
    stderr: "piped",
  });
}

async function cold(file: string, program = bin): Promise<Times> {
  // A viewer of its own, even when a warm one (the user's, say) is listening.
  const out = await slice({ SLERP_QUIT_AFTER: "0.3", SLERP_VIEWER_KEEP_ALIVE: "0", SLERP_NO_HANDOFF: "1" }, [
    "view",
    "--class",
    "agent-test",
    file,
  ], program).output();
  return parse(new TextDecoder().decode(out.stderr));
}

/** A viewer that stays loaded, and the text it prints, read as it comes. */
class Warm {
  child!: Deno.ChildProcess;
  text = "";
  done: Promise<void>;
  constructor(first: string) {
    this.child = slice({ SLERP_QUIT_AFTER: "0.3", SLERP_VIEWER_KEEP_ALIVE: "300", SLERP_NO_HANDOFF: "1" }, [
      "view",
      "--class",
      "agent-test",
      first,
    ]).spawn();
    const stderr = this.child.stderr;
    this.done = (async () => {
      for await (const chunk of stderr.pipeThrough(new TextDecoderStream())) {
        this.text += chunk;
      }
    })();
  }
  /** Waits until the text has `count` lines matching `re`; false after
   * 30 seconds. */
  async waitFor(re: RegExp, count: number): Promise<boolean> {
    for (let i = 0; i < 600; i++) {
      if ((this.text.match(new RegExp(re, "g")) ?? []).length >= count) return true;
      await new Promise((r) => setTimeout(r, 50));
    }
    return false;
  }
}

function median(values: (number | undefined)[]): string {
  const v = values.filter((x): x is number => x !== undefined).sort((a, b) => a - b);
  if (v.length === 0) return "–";
  const m = v.length % 2 ? v[(v.length - 1) / 2] : (v[v.length / 2 - 1] + v[v.length / 2]) / 2;
  return m < 10 ? m.toFixed(1) : m.toFixed(0);
}

const name = (f: string) => f.split("/").pop()!;
const rows: string[] = [];

// 1. Cold starts, by turns with the other build when there is one.
const cells = (results: Times[]) =>
  (["shown", "gpu", "window", "model", "complete"] as const).map((k) => median(results.map((r) => r[k]))).join(" | ");
const row = (file: string, start: string, results: Times[], handoff = "") =>
  `| ${name(file)} | ${start} | ${cells(results)} | ${handoff} |`;
for (const file of files) {
  const [mine, theirs]: Times[][] = [[], []];
  for (let i = 0; i < runs; i++) {
    if (against) theirs.push(await cold(file, against));
    mine.push(await cold(file));
  }
  if (against) rows.push(row(file, `cold, ${name(against)}`, theirs));
  rows.push(row(file, against ? `cold, ${name(bin)}` : "cold", mine));
}

// 2. The warm viewer: started once, then handed each file.
if (coldOnly) {
  print();
  Deno.exit(0);
}
print();
rows.length = 0;
const warm = new Warm(files[0]);
if (!await warm.waitFor(/first frame with the file complete/, 1)) {
  console.log("The warm viewer didn't show its first file:\n" + warm.text);
  Deno.exit(1);
}
await new Promise((r) => setTimeout(r, 800)); // its window closes and it listens
let requests = 0;
for (const file of files) {
  const results: Times[] = [];
  for (let i = 0; i < runs; i++) {
    const before = warm.text.length;
    const client = await slice({ SLERP_QUIT_AFTER: "0.3" }, ["view", "--class", "agent-test", file]).output();
    const handoff = parse(new TextDecoder().decode(client.stderr)).handoff;
    requests++;
    // That window, then its closing, before the next request.  Another
    // warm viewer (say, one already running) taking the request shows as
    // a gap in the table.
    if (
      !await warm.waitFor(/first frame with the file complete/, requests + 1) ||
      !await warm.waitFor(/window frames, \d+ view frames drawn/, requests + 1)
    ) {
      console.log(`no window for ${name(file)}: was another warm viewer listening?`);
      continue;
    }
    await new Promise((r) => setTimeout(r, 300));
    results.push({ ...parse(warm.text.slice(before)), handoff });
  }
  rows.push(row(file, "warm", results, median(results.map((r) => r.handoff))));
}
warm.child.kill("SIGTERM");
await warm.done.catch(() => {});

print();

function print() {
  console.log(`Medians of ${runs} runs, in ms from the command starting (cold) or the warm viewer taking it (warm).\n`);
  console.log("| File | Start | Window shown | GPU ready | First frame | Model on screen | Complete | Hand-over |");
  console.log("|---|---|---|---|---|---|---|---|");
  for (const r of rows) console.log(r);
}
