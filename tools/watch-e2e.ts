/**
 * End-to-end checks of the viewer's watch mode, Esc and mouse buttons, with a real window of class `agent-test`.
 *
 * Usage: `deno run -A tools/watch-e2e.ts [--keep] [case ...]` after `tools/cargo-capped build -p slerp`, with
 * something that keeps windows of that class off the screen (a KWin window rule, say).  Cases: `bar`, `always`, `bin`, `buttons`,
 * `esc` (all by default).  Each runs `slerp view --class agent-test` with a fresh state folder, scripted input
 * (`SLERP_INPUT`, see `crates/viewer/src/script.rs`) and a writer here that rewrites the open files while it shows
 * them, then checks the view states the script saved next to its screenshots.  Screenshots of the bar and notes go to
 * `local/artifacts/phase16-watch-*.png` (or `$SLERP_ARTIFACTS`).  Work files are in `/tmp/slerp-watch-e2e/`.
 */

const root = new URL("..", import.meta.url).pathname
const slerp = `${root}local/target/debug/slerp`
const assets = `${root}local/assets`
const artifacts = Deno.env.get("SLERP_ARTIFACTS") ?? `${root}local/artifacts`
const work = "/tmp/slerp-watch-e2e"
/** Where the bar's Reload button is in a 1400 x 900 window with the side panels shown (Breeze, 10 pt fonts). */
const reloadButton = "887,97"

/*┌─────────────────╮
  | Running         |
  └─────────────────╯*/
//region Running

interface Run {
	/** Resolves with the stderr once the viewer exits. */
	done:Promise<string>
	/** Resolves once the file is shown. */
	ready:Promise<void>
	started:number
}

/** Starts the viewer on `file` in its own cgroup, with the state folder `state` and `env` added. */
function view(file:string, state:string, env:Record<string, string>):Run {
	const command = new Deno.Command("systemd-run", {
		args: ["--user", "--scope", "--quiet", "--slice=heavy-slerp.slice", slerp, "view", "--class", "agent-test", file],
		env: {XDG_STATE_HOME: state, SLERP_WINDOW_SIZE: "1400x900", SLERP_TIMING: "1", SLERP_QUIT_AFTER: "30", ...env},
		stdout: "null",
		stderr: "piped",
	})
	const child = command.spawn()
	let log = ""
	let markReady = () => {}
	const ready = new Promise<void>(r => markReady = r)
	const done = (async () => {
		const decoder = new TextDecoder()
		for await (const chunk of child.stderr) {
			log += decoder.decode(chunk)
			if (log.includes("first frame with the file")) markReady()
		}
		await child.status
		return log
	})()
	return {done, ready, started: Date.now()}
}

const sleep = (ms:number) => new Promise(r => setTimeout(r, ms))

/** Writes `bytes` over `path` in place, in two pieces `gap` ms apart, as exporters do. */
async function writeInTwo(path:string, bytes:Uint8Array, gap = 150, split = 0.5) {
	const f = await Deno.open(path, {write: true, truncate: true})
	const cut = Math.floor(bytes.length * split)
	await f.write(bytes.subarray(0, cut))
	await sleep(gap)
	await f.write(bytes.subarray(cut))
	f.close()
}

interface Shown {
	mode:string
	file:string
	orbit?:Record<string, unknown>
	clip?:string
	time?:number
}

/** What a `shot:` step saved next to its screenshot: the mode and file shown, and a 3D view's state. */
function state(png:string):Shown {
	const [mode, file] = Deno.readTextFileSync(`${png}.txt`).split("\n")
	let json = {}
	try {
		const text = Deno.readTextFileSync(`${png}.jsonc`)
		json = JSON.parse(text.split("\n").filter(l => !l.startsWith("//")).join("\n"))
	} catch { /* not a 3D view */ }
	return {mode, file, ...json}
}

function check(ok:boolean, what:string) {
	console.log(`${ok ? "ok  " : "FAIL"} ${what}`)
	if (!ok) failures++
}
let failures = 0

/** A fresh folder for one case, with `saved` as the viewer's state file. */
function caseDir(name:string, saved:Record<string, unknown> = {}):{dir:string, state:string} {
	const dir = `${work}/${name}`
	try { Deno.removeSync(dir, {recursive: true}) } catch { /* new */ }
	Deno.mkdirSync(`${dir}/state/slerp`, {recursive: true})
	Deno.writeTextFileSync(`${dir}/state/slerp/viewer.json`, JSON.stringify(saved))
	return {dir, state: `${dir}/state`}
}

//endregion

/*┌─────────────────╮
  | Test files      |
  └─────────────────╯*/
//region Test files

/** A GLB with each material's base colour factor replaced: a re-export that looks different. */
function tinted(glb:Uint8Array, color:number[]):Uint8Array {
	const view = new DataView(glb.buffer, glb.byteOffset)
	const jsonLength = view.getUint32(12, true)
	const json = JSON.parse(new TextDecoder().decode(glb.subarray(20, 20 + jsonLength)))
	for (const m of json.materials ?? []) (m.pbrMetallicRoughness ??= {}).baseColorFactor = color
	let text = JSON.stringify(json)
	while (text.length % 4) text += " "
	const jsonBytes = new TextEncoder().encode(text)
	const rest = glb.subarray(20 + jsonLength)
	const out = new Uint8Array(20 + jsonBytes.length + rest.length)
	const o = new DataView(out.buffer)
	out.set(new TextEncoder().encode("glTF"), 0)
	o.setUint32(4, 2, true)
	o.setUint32(8, out.length, true)
	o.setUint32(12, jsonBytes.length, true)
	o.setUint32(16, 0x4e4f534a, true)
	out.set(jsonBytes, 20)
	out.set(rest, 20 + jsonBytes.length)
	return out
}

/** A copy of a glTF's `.bin` with the positions of its first mesh stretched by `scale` on each axis. */
function stretched(gltfPath:string, bin:Uint8Array, scale:number[]):Uint8Array {
	const json = JSON.parse(Deno.readTextFileSync(gltfPath))
	const accessor = json.accessors[json.meshes[0].primitives[0].attributes.POSITION]
	const bufferView = json.bufferViews[accessor.bufferView]
	const out = bin.slice()
	const v = new DataView(out.buffer)
	const stride = bufferView.byteStride ?? 12
	for (let i = 0; i < accessor.count; i++) {
		for (let c = 0; c < 3; c++) {
			const at = (bufferView.byteOffset ?? 0) + (accessor.byteOffset ?? 0) + i * stride + c * 4
			v.setFloat32(at, v.getFloat32(at, true) * scale[c], true)
		}
	}
	return out
}

const man = Deno.readFileSync(`${assets}/gltf/khronos/CesiumMan/glTF-Binary/CesiumMan.glb`)
const sameView = (a:Shown, b:Shown) =>
	JSON.stringify(a.orbit) === JSON.stringify(b.orbit) && a.clip === b.clip && a.time === b.time

//endregion

/*┌─────────────────╮
  | Cases           |
  └─────────────────╯*/
//region Cases

const cases:Record<string, () => Promise<void>> = {
	/** The bar appears once the two pieces are written, and Reload keeps the camera, clip and time. */
	async bar() {
		const {dir, state: st} = caseDir("bar")
		const file = `${dir}/model.glb`
		Deno.writeFileSync(file, man)
		const run = view(file, st, {
			SLERP_INPUT: `drag:160,40 Space sleep:600 shot:${dir}/before.png sleep:1800 ` +
				`shot:${artifacts}/phase16-watch-bar.png clickat:${reloadButton} sleep:800 shot:${dir}/after.png`,
			SLERP_SCREENSHOT: `${dir}/final.png`,
		})
		await run.ready
		await sleep(1000)
		await writeInTwo(file, tinted(man, [1, 0.25, 0.2, 1]))
		const log = await run.done
		check(log.includes("model.glb reloaded in"), "bar: Reload reloaded the file")
		const [before, bar, after] = [state(`${dir}/before.png`), state(`${artifacts}/phase16-watch-bar.png`), state(`${dir}/after.png`)]
		check(sameView(before, bar), "bar: nothing moved before Reload")
		check(sameView(before, after), `bar: camera, clip and time kept (${JSON.stringify(after.orbit)}, ${after.clip} at ${after.time})`)
		check((before.orbit?.yaw as number) !== 0, "bar: the drag turned the camera")
	},

	/** With Always Reload on: a half-written file fails with a note and the old model stays; the rest reloads by
	 * itself with the "Reloaded" note, keeping the view. */
	async always() {
		const {dir, state: st} = caseDir("always", {alwaysReload: true})
		const file = `${dir}/model.glb`
		Deno.writeFileSync(file, man)
		const run = view(file, st, {
			SLERP_INPUT: `drag:-120,30 Space sleep:600 shot:${dir}/before.png sleep:1200 ` +
				`shot:${artifacts}/phase16-watch-failed.png sleep:1300 shot:${artifacts}/phase16-watch-reloaded.png`,
			SLERP_SCREENSHOT: `${dir}/final.png`,
		})
		await run.ready
		await sleep(900)
		// 1. A file cut off inside its JSON, then quiet: the reload fails.
		//    (Cut in its binary chunk, the glTF loader still reads what is
		//    there, with warnings.)
		const red = tinted(man, [0.3, 0.5, 1, 1])
		const cut = 4000
		const f = await Deno.open(file, {write: true, truncate: true})
		await f.write(red.subarray(0, cut))
		await sleep(1300)
		// 2. The rest: it reloads by itself.
		await f.write(red.subarray(cut))
		f.close()
		const log = await run.done
		const reloads = log.split("model.glb reloaded in").length - 1
		check(reloads === 1, `always: one reload by itself, after the failed one (${reloads})`)
		const [before, failed, reloaded] = [state(`${dir}/before.png`), state(`${artifacts}/phase16-watch-failed.png`), state(`${artifacts}/phase16-watch-reloaded.png`)]
		check(sameView(before, failed), "always: the failed reload kept the old model's view")
		check(sameView(before, reloaded), "always: the reload kept the camera, clip and time")
	},

	/** A glTF whose `.bin` alone changes: the bar appears, and F5 reloads keeping the camera. */
	async bin() {
		const {dir, state: st} = caseDir("bin")
		const src = `${assets}/gltf/khronos/Box/glTF`
		Deno.copyFileSync(`${src}/Box.gltf`, `${dir}/Box.gltf`)
		Deno.copyFileSync(`${src}/Box0.bin`, `${dir}/Box0.bin`)
		const run = view(`${dir}/Box.gltf`, st, {
			SLERP_INPUT: `drag:200,-30 sleep:600 shot:${dir}/before.png sleep:1500 ` +
				`shot:${artifacts}/phase16-watch-bin.png F5 sleep:800 shot:${dir}/after.png`,
			SLERP_SCREENSHOT: `${dir}/final.png`,
		})
		await run.ready
		await sleep(900)
		await writeInTwo(`${dir}/Box0.bin`, stretched(`${dir}/Box.gltf`, Deno.readFileSync(`${dir}/Box0.bin`), [1.6, 1, 1]))
		const log = await run.done
		check(log.includes("Box.gltf reloaded in"), "bin: F5 reloaded after the .bin changed")
		check(sameView(state(`${dir}/before.png`), state(`${dir}/after.png`)), "bin: camera kept")
	},

	/** Mouse forward and back step through the folder, over the view and over a side panel. */
	async buttons() {
		const {dir, state: st} = caseDir("buttons")
		for (const n of ["a", "b", "c"]) Deno.writeFileSync(`${dir}/${n}.glb`, man)
		const run = view(`${dir}/b.glb`, st, {
			SLERP_INPUT: `press:forward sleep:700 shot:${dir}/1.png press:back:1250,600 sleep:700 shot:${dir}/2.png ` +
				`press:back sleep:700 shot:${dir}/3.png`,
			SLERP_SCREENSHOT: `${dir}/final.png`,
		})
		await run.done
		const names = [1, 2, 3].map(i => state(`${dir}/${i}.png`).file.split("/").pop())
		check(names.join() === "c.glb,b.glb,a.glb", `buttons: forward, back over the panel, back (${names.join()})`)
	},

	/** In a gallery, the mouse's back button and Esc each leave an open picture; then Esc closes the window when
	 * the option is on, and does nothing more when it is off. */
	async esc() {
		const wad = `${assets}/quake/avix_qcastle/charlie.wad`
		for (const on of [true, false]) {
			const {dir, state: st} = caseDir(`esc-${on}`, {escCloses: on})
			const run = view(wad, st, {
				SLERP_INPUT: `click:0.08,0.12 sleep:400 shot:${dir}/1.png press:back sleep:400 shot:${dir}/2.png ` +
					`click:0.08,0.12 sleep:400 shot:${dir}/3.png Escape sleep:400 shot:${dir}/4.png ` +
					`Escape sleep:2500 shot:${dir}/5.png`,
				SLERP_QUIT_AFTER: "14",
			})
			await run.ready
			const t0 = Date.now()
			await run.done
			const seconds = (Date.now() - t0) / 1000
			const modes = [1, 2, 3, 4].map(i => state(`${dir}/${i}.png`).mode)
			const want = ["Gallery picture", "Gallery", "Gallery picture", "Gallery"]
			check(modes.join() === want.join(), `esc ${on ? "on" : "off"}: back button and Esc leave the picture (${modes.join(", ")})`)
			const last = (() => { try { Deno.statSync(`${dir}/5.png`); return true } catch { return false } })()
			if (on) check(!last && seconds < 7, `esc on: the next Esc closed the window (${seconds.toFixed(1)} s)`)
			else check(last && seconds > 12, `esc off: the window stayed open (${seconds.toFixed(1)} s)`)
		}
	},

	/** A panel section opened in one run is open in the next, through the state file. */
	async sections() {
		const {dir, state: st} = caseDir("sections")
		Deno.writeFileSync(`${dir}/model.glb`, man)
		// "Camera" in the right-hand panel of a character, closed at first.
		const first = view(`${dir}/model.glb`, st, {
			SLERP_INPUT: `sleep:300 clickat:1130,423 sleep:600`,
			SLERP_SCREENSHOT: `${dir}/first.png`,
		})
		await first.done
		const saved = JSON.parse(Deno.readTextFileSync(`${st}/slerp/viewer.json`))
		check(saved.sections?.Camera === true, `sections: Camera saved as open (${JSON.stringify(saved.sections)})`)
		const second = view(`${dir}/model.glb`, st, {SLERP_SCREENSHOT: `${artifacts}/phase16-sections-remembered.png`})
		await second.done
	},
}

//endregion

const keep = Deno.args.includes("--keep")
const wanted = Deno.args.filter(a => !a.startsWith("--"))
Deno.mkdirSync(work, {recursive: true})
Deno.mkdirSync(artifacts, {recursive: true})
for (const [name, run] of Object.entries(cases)) {
	if (wanted.length && !wanted.includes(name)) continue
	await run()
}
// The screenshots stay; what the script saved beside them goes.
for (const e of Deno.readDirSync(artifacts)) {
	if (/^phase16-.*\.png\.(txt|jsonc)$/.test(e.name)) Deno.removeSync(`${artifacts}/${e.name}`)
}
if (!keep) Deno.removeSync(work, {recursive: true})
console.log(failures ? `${failures} failed` : "all passed")
Deno.exit(failures ? 1 : 0)
