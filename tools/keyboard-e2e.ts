// End-to-end check of the viewer's keyboard on panel controls: opens a Doom
// WAD, clicks its map picker (a combo box) and steps through the maps with
// the arrow keys, End and the mouse wheel, then presses Escape and checks
// that the arrow keys step through files again.  Each step's map comes
// from the view state the input script's `shot:` step writes beside its
// screenshot.  Run behind hide-windows:
//
//   hide-windows.sh start
//   deno run -A tools/keyboard-e2e.ts [--bin local/target/release/slerp] [--out dir] [--xwayland]
//
// The window keeps its panel state in a temporary XDG_STATE_HOME, so the
// panels sit where this script expects them.  `--xwayland` runs it through
// XWayland, for when KWin holds back a hidden Wayland window's frames (see
// startup-timing.ts).

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
const out = option("--out", await Deno.makeTempDir({ prefix: "slerp-keys-" }));
const wad = `${root}local/assets/doom/freedoom/freedoom2.wad`;
const xwayland = args.includes("--xwayland");
await Deno.mkdir(out, { recursive: true });

// Where the map picker sits in a 1200 by 760 window (small enough for any
// screen KWin may place it on) with the panels as they start: the first
// control in the right-hand panel's Maps section, 168 points in from the
// window's right edge (found from a screenshot).
const picker = option("--picker", "1032,137");
const shot = (name: string) => `shot:${out}/${name}.png`;
const steps = [
  "sleep:500",
  shot("0-start"),
  // A click opens the list and focuses the box; a second click closes the
  // list and keeps the focus.
  `clickat:${picker}`,
  "sleep:200",
  `clickat:${picker}`,
  "sleep:200",
  "Down",
  "sleep:800",
  shot("1-down"),
  "Down",
  "Down",
  "sleep:800",
  shot("2-down-down"),
  // Towards the user: the next map.
  `scrollat:${picker}:-1`,
  "sleep:800",
  shot("3-wheel"),
  "End",
  "sleep:800",
  shot("4-end"),
  "Home",
  "sleep:800",
  shot("5-home"),
  // Escape gives the keys back to the view, where Right is the next file.
  "Escape",
  "sleep:200",
  "Right",
  "sleep:1500",
  shot("6-next-file"),
];

const state = await Deno.makeTempDir({ prefix: "slerp-state-" });
const cmd = new Deno.Command("systemd-run", {
  args: [
    "--user",
    "--scope",
    "--quiet",
    "--slice=heavy-slerp.slice",
    ...(xwayland ? ["env", "-u", "WAYLAND_DISPLAY"] : []),
    bin,
    "view",
    "--class",
    "agent-test",
    wad,
  ],
  env: {
    ...(xwayland ? { SLERP_GPU: "nvidia" } : {}),
    SLERP_NO_HANDOFF: "1",
    SLERP_VIEWER_KEEP_ALIVE: "0",
    SLERP_WINDOW_SIZE: "1200x760",
    SLERP_INPUT: steps.join(" "),
    SLERP_SCREENSHOT: `${out}/7-end.png`,
    XDG_STATE_HOME: state,
  },
  stdout: "null",
  stderr: "piped",
});
const result = await cmd.output();
const log = new TextDecoder().decode(result.stderr);

/** The map and file a shot showed, from the files written beside it. */
async function shown(name: string): Promise<{ part?: string; file: string }> {
  const text = await Deno.readTextFile(`${out}/${name}.png.txt`).catch(() => "");
  const jsonc = await Deno.readTextFile(`${out}/${name}.png.jsonc`).catch(() => "");
  const part = jsonc.match(/"part":\s*"([^"]+)"/)?.[1];
  return { part, file: text.split("\n")[1] ?? "" };
}

const expected: [string, string | undefined][] = [
  ["0-start", "MAP01"],
  ["1-down", "MAP02"],
  ["2-down-down", "MAP04"],
  ["3-wheel", "MAP05"],
  ["4-end", "MAP32"],
  ["5-home", "MAP01"],
];
let failed = false;
for (const [name, part] of expected) {
  const s = await shown(name);
  const ok = s.part === part && s.file === wad;
  failed ||= !ok;
  console.log(`${ok ? "ok  " : "FAIL"} ${name}: ${s.part ?? "no map"} in ${s.file.split("/").pop()} (want ${part})`);
}
const next = await shown("6-next-file");
const stepped = next.file !== "" && next.file !== wad;
failed ||= !stepped;
console.log(`${stepped ? "ok  " : "FAIL"} 6-next-file: ${next.file.split("/").pop() || "nothing"} (want another file)`);
console.log(`screenshots in ${out}`);
if (failed) {
  console.log(log);
  Deno.exit(1);
}
