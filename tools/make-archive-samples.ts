#!/usr/bin/env -S deno run -A
// Makes the archive samples that cannot be downloaded ready-made, into
// local/assets/archives-extra/ (the folders the slerp-archive tests read):
//
//   deno run -A tools/make-archive-samples.ts pck [--cache DIR] [--out DIR]
//   deno run -A tools/make-archive-samples.ts grp [--out DIR]
//
// `pck` exports real Godot data packs from two of the MIT-licensed
// godot-demo-projects (2d/pong and 2d/bullet_shower) with one Godot editor
// per pack format: 3.6.3 writes format 1, 4.3 writes 2 (4.0 to 4.4 do), 4.6.3
// writes 3 (4.5 and 4.6 do) and 4.7.2 writes 4.  `--export-pack` needs no
// export templates.  Each editor is 30 to 80 MB, so they are downloaded into
// the cache folder (default /tmp/slerp-godot-cache) and can be deleted
// afterwards; the packs are 15 to 25 KB.  It needs Deno, git and 7z.
//
// `grp` writes two tiny Build engine group files by hand (the format is a
// "KenSilverman" magic, a file count, 16-byte name and size entries, then the
// data), since no real group file may be redistributed: one with edge cases
// in names and sizes, and one with no files at all.

const root = new URL('..', import.meta.url).pathname
const args = Deno.args
const opt = (name: string, fallback: string) => {
	const i = args.indexOf(name)
	return i >= 0 && args[i + 1] ? args[i + 1] : fallback
}
const out = opt('--out', `${root}local/assets/archives-extra`)
const cache = opt('--cache', '/tmp/slerp-godot-cache')

/*┌─────────────────╮
  | Godot packs     |
  └─────────────────╯*/
//region Godot packs

interface Build {
	/** The PCK format version this editor writes. */
	format: number
	version: string
	/** The release asset of github.com/godotengine/godot-builds. */
	zip: string
	/** The demo-projects branch whose projects this editor can open (older projects open fine in newer editors). */
	branch: string
	/** Godot 3 has other command line flags and export preset keys than Godot 4. */
	godot3?: boolean
	/** Extra packs of `pong` with encryption on: `dir` encrypts the file list as well as the files named by the filters. */
	encrypted?: ('dir' | 'files')[]
}

const builds: Build[] = [
	{format: 1, version: '3.6.3', zip: 'Godot_v3.6.3-stable_linux_headless.64.zip', branch: '3.x', godot3: true},
	{format: 2, version: '4.3', zip: 'Godot_v4.3-stable_linux.x86_64.zip', branch: '4.3', encrypted: ['dir']},
	{format: 3, version: '4.6.3', zip: 'Godot_v4.6.3-stable_linux.x86_64.zip', branch: '4.3', encrypted: ['dir', 'files']},
	{format: 4, version: '4.7.2', zip: 'Godot_v4.7.2-stable_linux.x86_64.zip', branch: 'master'},
]

/** A throw-away key for the encrypted packs; the packs hold only the public demo files. */
const encryptionKey = '000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f'

/** Runs a command, failing loudly; Godot prints a lot, so its output is shown only when it fails. */
async function run(cmd: string[], cwd?: string, env: Record<string, string> = {}) {
	const r = await new Deno.Command(cmd[0], {args: cmd.slice(1), cwd, env, stdout: 'piped', stderr: 'piped'}).output()
	if (!r.success) {
		console.error(new TextDecoder().decode(r.stdout) + new TextDecoder().decode(r.stderr))
		throw new Error(`${cmd.join(' ')} failed (${r.code})`)
	}
}

/** The Godot editor for `b` in the cache, downloaded first if needed. */
async function editor(b: Build): Promise<string> {
	const dir = `${cache}/godot-${b.version}`
	const exe = `${dir}/${b.zip.replace(/\.zip$/, '')}`
	try {
		await Deno.stat(exe)
		return exe
	} catch { /* Not downloaded yet. */ }
	await Deno.mkdir(dir, {recursive: true})
	const url = `https://github.com/godotengine/godot-builds/releases/download/${b.version}-stable/${b.zip}`
	console.log(`downloading ${url}`)
	const res = await fetch(url)
	if (!res.ok || !res.body) throw new Error(`${url}: ${res.status}`)
	await Deno.writeFile(`${dir}/${b.zip}`, res.body)
	await run(['7z', 'x', '-y', `-o${dir}`, `${dir}/${b.zip}`])
	await Deno.remove(`${dir}/${b.zip}`)
	await Deno.chmod(exe, 0o755)
	return exe
}

/** The demo projects of one branch: a shallow, blob-less, sparse clone with just the two projects used. */
async function demos(branch: string): Promise<string> {
	const dir = `${cache}/demo-projects-${branch}`
	try {
		await Deno.stat(dir)
		return dir
	} catch { /* Not cloned yet. */ }
	await run(['git', 'clone', '-q', '--depth', '1', '--filter=blob:none', '--sparse', '--branch', branch,
		'https://github.com/godotengine/godot-demo-projects', dir])
	await run(['git', 'sparse-checkout', 'set', '--no-cone', '/2d/pong/', '/2d/bullet_shower/'], dir)
	return dir
}

/** The export preset file (`export_presets.cfg`): one Linux preset, optionally with encryption. */
function preset(b: Build, encrypt?: 'dir' | 'files'): string {
	const filters = encrypt ? '*.ctex,*.stex,*.scn,*.gdc' : ''
	if (b.godot3) return `[preset.0]
name="Linux/X11"
platform="Linux/X11"
runnable=true
export_filter="all_resources"
include_filter=""
exclude_filter=""
export_path=""

[preset.0.options]
binary_format/64_bits=true
binary_format/embed_pck=false
`
	return `[preset.0]
name="Linux"
platform="Linux"
runnable=true
export_filter="all_resources"
include_filter=""
exclude_filter=""
export_path=""
encryption_include_filters="${filters}"
encryption_exclude_filters=""
encrypt_pck=${encrypt ? 'true' : 'false'}
encrypt_directory=${encrypt === 'dir' ? 'true' : 'false'}

[preset.0.options]
binary_format/embed_pck=false
binary_format/architecture="x86_64"
`
}

async function makePacks() {
	await Deno.mkdir(`${out}/pck`, {recursive: true})
	for (const b of builds) {
		const exe = await editor(b)
		const demoDir = await demos(b.branch)
		const jobs: {project: string, encrypt?: 'dir' | 'files'}[] =
			[{project: 'pong'}, {project: 'bullet_shower'}, ...(b.encrypted ?? []).map(e => ({project: 'pong', encrypt: e}))]
		for (const {project, encrypt} of jobs) {
			// 1. A scratch copy of the project, since importing writes into its folder.
			const work = await Deno.makeTempDir({dir: cache, prefix: 'project-'})
			await run(['cp', '-r', `${demoDir}/2d/${project}/.`, work])
			await Deno.writeTextFile(`${work}/export_presets.cfg`, preset(b, encrypt))
			// 2. Import the textures and scenes, which the pack holds in their imported form, then export.
			const name = `v${b.format}-godot-${b.version}-${project}${encrypt ? `-encrypted-${encrypt === 'dir' ? 'dir' : 'files'}` : ''}.pck`
			const env: Record<string, string> = encrypt ? {GODOT_SCRIPT_ENCRYPTION_KEY: encryptionKey} : {}
			const headless = b.godot3 ? [] : ['--headless']
			await run([exe, ...headless, '--path', work, ...(b.godot3 ? ['-e', '--quit'] : ['--import'])], work, env)
			await run([exe, ...headless, '--path', work, '--export-pack', b.godot3 ? 'Linux/X11' : 'Linux', `${out}/pck/${name}`], work, env)
			await Deno.remove(work, {recursive: true})
			console.log(name)
		}
	}
}

//endregion

/*┌─────────────────╮
  | Group files     |
  └─────────────────╯*/
//region Group files

/** A Build engine group file: "KenSilverman", the file count, 12-byte NUL-padded names with 32-bit sizes, then the data. */
function group(files: [string, Uint8Array][]): Uint8Array {
	const text = new TextEncoder()
	const header = new Uint8Array(16 + 16 * files.length)
	const view = new DataView(header.buffer)
	header.set(text.encode('KenSilverman'))
	view.setUint32(12, files.length, true)
	files.forEach(([name, data], i) => {
		header.set(text.encode(name), 16 + 16 * i)
		view.setUint32(16 + 16 * i + 12, data.length, true)
	})
	return new Uint8Array([...header, ...files.flatMap(([, data]) => [...data])])
}

async function makeGroups() {
	const dir = `${out}/grp/synthetic`
	await Deno.mkdir(dir, {recursive: true})
	const text = (s: string) => new TextEncoder().encode(s)
	await Deno.writeFile(`${dir}/small.grp`, group([
		['HELLO.TXT', text('Hello from a hand-made group file.\r\n')],
		['EMPTY.DAT', new Uint8Array(0)],
		// Exactly 12 characters: the name has no NUL terminator.
		['TWELVECHARS.', text('A name that fills all 12 bytes.')],
		['lower.txt', text('Names are usually upper case, but nothing forces it.')],
		['PALETTE.DAT', Uint8Array.from({length: 768}, (_, i) => (i * 7) & 63)],
		['SPACE IN.TXT', text('A name with a space.')],
	]))
	await Deno.writeFile(`${dir}/empty.grp`, group([]))
	console.log('small.grp, empty.grp')
}

//endregion

if (args[0] === 'pck') await makePacks()
else if (args[0] === 'grp') await makeGroups()
else {
	console.error('usage: make-archive-samples.ts pck|grp [--cache DIR] [--out DIR]')
	Deno.exit(2)
}
