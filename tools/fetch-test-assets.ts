/**
 * Downloads Slerp's public test corpus into `local/assets/<category>/`, as described by `tools/assets.jsonc`.
 *
 * Usage: `deno run -A tools/fetch-test-assets.ts [--force] [--list] [--jobs N] [category | category/name ...]`
 *
 * With no arguments every entry is fetched.  An entry that already finished (its stamp in `local/assets/.cache/stamps/`
 * matches the entry) is skipped, so the script can be rerun after adding entries or after a network failure.  The
 * corpus is local test data only: `local/` is ignored by Syncthing and Git, and `local/assets/LICENCES.md` records
 * where each file came from and whether it may be redistributed.
 */

/*┌─────────────────╮
  | Types           |
  └─────────────────╯*/
//region Types

/** One download in `tools/assets.jsonc`.  See `tools/README.md` for the meaning of each field. */
interface Entry {
	category:string
	name:string
	url?:string
	licence:string
	redistribute?:boolean
	sha256?:string
	dest?:string
	extract?:Extract
	git?:GitSource
	polyhaven?:PolyHaven
	tool?:Tool
	commands?:string[][]
	size?:string
	note?:string
}

/** Archive handling: which members to keep, how many leading folders to drop, and whether to open Quake `.pak` files. */
interface Extract {
	keep?:string[]
	strip?:number
	pak?:boolean
}

/** A sparse, shallow Git checkout.  `paths` are gitignore-style patterns (non-cone sparse checkout). */
interface GitSource {
	repo?:string
	ref?:string
	paths:string[]
	strip?:number
}

/**
 * A Poly Haven asset fetched through its public API (`https://api.polyhaven.com/files/<id>`), which lists the main
 * file of each format and resolution together with the textures and buffers it includes.  `format` is the API's key
 * (`mtlx`, `gltf`, `blend`, `usd`), and `ext` the file kind inside it when it differs from `format`.
 */
interface PolyHaven {
	id:string
	format:string
	res:string
	ext?:string
}

/** A helper program (an archive holding a binary) that `commands` run as `{tool}`. */
interface Tool {
	url:string
	bin:string
}

/** What a finished entry left behind, stored as its stamp and used for the summary and LICENCES.md. */
interface Stamp {
	key:string
	files:number
	bytes:number
	dest:string
	licence:string
	redistribute:boolean
	url:string
	sha256?:string
}

//endregion

/*┌─────────────────╮
  | Paths           |
  └─────────────────╯*/
//region Paths

const root = new URL('..', import.meta.url).pathname.replace(/\/$/, '')
const assets = `${root}/local/assets`
const cache = `${assets}/.cache`
const manifestPath = `${root}/tools/assets.jsonc`
const userAgent = 'slerp-test-assets/1.0 (+https://github.com/)'

//endregion

/*┌─────────────────╮
  | Helpers         |
  └─────────────────╯*/
//region Helpers

/** Parses JSON with comments and trailing commas, without pulling in a dependency. */
function parseJsonc(text:string):unknown {
	let out = '', i = 0
	while (i < text.length) {
		const c = text[i]
		if (c === '"') {
			const start = i++
			while (i < text.length && text[i] !== '"') i += text[i] === '\\' ? 2 : 1
			out += text.slice(start, ++i)
		}
		else if (c === '/' && text[i + 1] === '/')
			while (i < text.length && text[i] !== '\n') i++
		else if (c === '/' && text[i + 1] === '*') {
			const end = text.indexOf('*/', i + 2)
			i = end < 0 ? text.length : end + 2
		}
		else
			out += text[i++]
	}
	return JSON.parse(out.replace(/,(\s*[\]}])/g, '$1'))
}

/** Converts a glob (`*`, `**`, `?`, `{a,b}`) matched against a whole slash-separated path into a RegExp. */
function globToRegExp(glob:string):RegExp {
	let re = '', i = 0
	while (i < glob.length) {
		const c = glob[i]
		if (c === '*' && glob[i + 1] === '*') {
			i += 2
			if (glob[i] === '/') {
				i++
				re += '(?:.*/)?'
			}
			else
				re += '.*'
			continue
		}
		if (c === '*') re += '[^/]*'
		else if (c === '?') re += '[^/]'
		else if (c === '{') re += '(?:'
		else if (c === '}') re += ')'
		else if (c === ',' && re.lastIndexOf('(?:') > re.lastIndexOf(')')) re += '|'
		else re += c.replace(/[.+^$()|[\]\\]/g, '\\$&')
		i++
	}
	return new RegExp(`^${re}$`, 'i')
}

/** Runs a program and throws with its output when it fails. */
async function run(cmd:string[], cwd?:string, env?:Record<string, string>):Promise<string> {
	const res = await new Deno.Command(cmd[0], {args: cmd.slice(1), cwd, env, stdout: 'piped', stderr: 'piped'}).output()
	const text = new TextDecoder().decode(res.stdout) + new TextDecoder().decode(res.stderr)
	if (!res.success) throw new Error(`${cmd.join(' ')} failed:\n${text.slice(-2000)}`)
	return text
}

async function exists(path:string):Promise<boolean> {
	try {
		await Deno.lstat(path)
		return true
	}
	catch {
		return false
	}
}

/** Lists every file below `dir`, as paths relative to it. */
async function walk(dir:string, prefix = ''):Promise<string[]> {
	const out:string[] = []
	for await (const e of Deno.readDir(dir)) {
		const rel = prefix ? `${prefix}/${e.name}` : e.name
		if (e.isDirectory) out.push(...await walk(`${dir}/${e.name}`, rel))
		else if (e.isFile || e.isSymlink) out.push(rel)
	}
	return out
}

/** Counts the files and bytes below a path (or of a single file). */
async function measure(path:string):Promise<{files:number, bytes:number}> {
	const st = await Deno.stat(path)
	if (st.isFile) return {files: 1, bytes: st.size}
	let files = 0, bytes = 0
	for (const f of await walk(path)) {
		files++
		bytes += (await Deno.stat(`${path}/${f}`)).size
	}
	return {files, bytes}
}

async function sha256File(path:string):Promise<string> {
	const data = await Deno.readFile(path)
	const hash = await crypto.subtle.digest('SHA-256', data)
	return Array.from(new Uint8Array(hash), b => b.toString(16).padStart(2, '0')).join('')
}

function formatBytes(n:number):string {
	if (n >= 1e9) return `${(n / 1e9).toFixed(2)} GB`
	if (n >= 1e6) return `${(n / 1e6).toFixed(1)} MB`
	return `${(n / 1e3).toFixed(0)} KB`
}

/** Downloads a URL to a file, through a `.part` file so an interrupted download is never mistaken for a finished one. */
async function download(url:string, path:string):Promise<void> {
	await Deno.mkdir(path.replace(/\/[^/]*$/, ''), {recursive: true})
	let lastError:unknown
	for (let attempt = 1; attempt <= 3; attempt++) {
		try {
			const res = await fetch(url, {headers: {'User-Agent': userAgent}, redirect: 'follow'})
			if (!res.ok || !res.body) throw new Error(`HTTP ${res.status} for ${url}`)
			const file = await Deno.open(`${path}.part`, {write: true, create: true, truncate: true})
			await res.body.pipeTo(file.writable)
			await Deno.rename(`${path}.part`, path)
			return
		}
		catch (e) {
			lastError = e
			if (String(e).includes('HTTP 4')) break
			await new Promise(r => setTimeout(r, 2000 * attempt))
		}
	}
	throw lastError
}

/** Moves `from` to `to`, replacing whatever is there. */
async function replace(from:string, to:string):Promise<void> {
	await Deno.mkdir(to.replace(/\/[^/]*$/, ''), {recursive: true})
	await Deno.remove(to, {recursive: true}).catch(() => {})
	await Deno.rename(from, to)
}

//endregion

/*┌─────────────────╮
  | Archives        |
  └─────────────────╯*/
//region Archives

/** Unpacks an archive into `dir`, choosing the tool from the file's magic bytes rather than its URL. */
async function unpack(file:string, dir:string):Promise<void> {
	await Deno.mkdir(dir, {recursive: true})
	const f = await Deno.open(file)
	const head = new Uint8Array(8)
	await f.read(head)
	f.close()
	const zipOr7z = (head[0] === 0x50 && head[1] === 0x4b) || (head[0] === 0x37 && head[1] === 0x7a && head[2] === 0xbc)
	if (zipOr7z) await run(['7z', 'x', '-y', '-bd', `-o${dir}`, file])
	else await run(['tar', '-xf', file, '-C', dir])
}

/**
 * Unpacks every Quake `.pak` file below `dir` next to itself, into a folder named after it (`id1/pak0.pak` becomes
 * `id1/pak0/maps/e1m1.bsp`).  The format is a 12-byte header (`PACK`, directory offset, directory length) and a
 * directory of 64-byte records (56-byte name, offset, length).
 */
async function unpackPaks(dir:string):Promise<void> {
	for (const rel of await walk(dir)) {
		if (!/\.pak$/i.test(rel)) continue
		const data = await Deno.readFile(`${dir}/${rel}`)
		const view = new DataView(data.buffer, data.byteOffset, data.byteLength)
		if (data.length < 12 || new TextDecoder().decode(data.subarray(0, 4)) !== 'PACK') continue
		const dirOfs = view.getInt32(4, true), dirLen = view.getInt32(8, true)
		const outDir = `${dir}/${rel.replace(/\.pak$/i, '')}`
		for (let o = dirOfs; o + 64 <= dirOfs + dirLen && o + 64 <= data.length; o += 64) {
			const nameBytes = data.subarray(o, o + 56)
			const name = new TextDecoder().decode(nameBytes.subarray(0, nameBytes.indexOf(0) < 0 ? 56 : nameBytes.indexOf(0)))
			const pos = view.getInt32(o + 56, true), len = view.getInt32(o + 60, true)
			if (!name || name.includes('..') || pos < 0 || len < 0 || pos + len > data.length) continue
			const out = `${outDir}/${name}`
			await Deno.mkdir(out.replace(/\/[^/]*$/, ''), {recursive: true})
			await Deno.writeFile(out, data.subarray(pos, pos + len))
		}
	}
}

/**
 * Moves the files of `from` that match `keep` into `to`, dropping `strip` leading folders.  Matching happens on paths
 * before stripping, so `keep` globs read like the archive's own listing.
 */
async function select(from:string, to:string, keep?:string[], strip = 0):Promise<number> {
	const res = keep?.map(globToRegExp)
	const staging = `${to}.staging`
	await Deno.remove(staging, {recursive: true}).catch(() => {})
	let n = 0
	for (const rel of await walk(from)) {
		if (rel.startsWith('.git/')) continue
		if (res && !res.some(r => r.test(rel))) continue
		const parts = rel.split('/')
		if (parts.length <= strip) continue
		const out = `${staging}/${parts.slice(strip).join('/')}`
		await Deno.mkdir(out.replace(/\/[^/]*$/, ''), {recursive: true})
		await Deno.rename(`${from}/${rel}`, out)
		n++
	}
	if (!n) throw new Error(`no files matched ${keep?.join(', ') ?? '(everything)'}`)
	await replace(staging, to)
	return n
}

//endregion

/*┌─────────────────╮
  | Sources         |
  └─────────────────╯*/
//region Sources

/** Git settings that make a sparse checkout fetch Git LFS objects for the files it checks out, and only those. */
const lfsConfig = ['-c', 'filter.lfs.smudge=git-lfs smudge -- %f', '-c', 'filter.lfs.process=git-lfs filter-process',
	'-c', 'filter.lfs.required=true', '-c', 'filter.lfs.clean=git-lfs clean -- %f']

/** Makes a shallow, blob-less, sparse checkout of `git.paths` in `dir`. */
async function gitCheckout(repo:string, git:GitSource, dir:string):Promise<void> {
	await Deno.remove(dir, {recursive: true}).catch(() => {})
	await Deno.mkdir(dir.replace(/\/[^/]*$/, ''), {recursive: true})
	const branch = git.ref ? ['--branch', git.ref] : []
	await run(['git', ...lfsConfig, 'clone', '--quiet', '--depth', '1', '--filter=blob:none', '--no-checkout', ...branch, repo, dir])
	await run(['git', ...lfsConfig, '-C', dir, 'sparse-checkout', 'set', '--no-cone', ...git.paths])
	await run(['git', ...lfsConfig, '-C', dir, 'checkout', '--quiet'], undefined, {GIT_LFS_SKIP_SMUDGE: '0'})
	await Deno.remove(`${dir}/.git`, {recursive: true})
}

/** Downloads a Poly Haven asset's main file and every file it includes, at their relative paths, into `dir`. */
async function polyhavenCheckout(p:PolyHaven, dir:string):Promise<void> {
	const res = await fetch(`https://api.polyhaven.com/files/${p.id}`, {headers: {'User-Agent': userAgent}})
	if (!res.ok) throw new Error(`Poly Haven API: HTTP ${res.status} for ${p.id}`)
	type File = {url:string, include?:Record<string, {url:string}>}
	const files = await res.json() as Record<string, Record<string, Record<string, File>>>
	const main = files[p.format]?.[p.res]?.[p.ext ?? p.format]
	if (!main) throw new Error(`Poly Haven ${p.id} has no ${p.format}/${p.res}`)
	await download(main.url, `${dir}/${main.url.split('/').pop()}`)
	for (const [rel, f] of Object.entries(main.include ?? {})) {
		if (rel.includes('..')) continue
		await download(f.url, `${dir}/${rel}`)
	}
}

/** Fetches an entry's source (Git checkout, Poly Haven bundle or downloaded archive) into `dir`, unpacked. */
async function fetchSource(e:Entry, dir:string, work:string):Promise<string | undefined> {
	if (e.polyhaven) {
		await polyhavenCheckout(e.polyhaven, dir)
		return
	}
	if (e.git) {
		await gitCheckout(e.git.repo ?? e.url!, e.git, dir)
		return
	}
	const file = `${work}/download`
	await download(e.url!, file)
	const hash = await sha256File(file)
	if (e.sha256 && e.sha256 !== hash) throw new Error(`sha256 mismatch: expected ${e.sha256}, got ${hash}`)
	await unpack(file, dir)
	await Deno.remove(file)
	return hash
}

//endregion

/*┌─────────────────╮
  | Entries         |
  └─────────────────╯*/
//region Entries

/** The fields that decide what gets downloaded; editing only the licence or notes must not trigger a new download. */
const key = (e:Entry) => JSON.stringify({...e, size: undefined, note: undefined, licence: undefined, redistribute: undefined})
const stampPath = (e:Entry) => `${cache}/stamps/${e.category}/${e.name}.json`
const isPlainFile = (e:Entry) => !e.git && !e.polyhaven && !e.extract && !e.commands

/** Where an entry's files end up: a single file for plain downloads, otherwise a folder named after the entry. */
function destination(e:Entry):string {
	if (isPlainFile(e)) return `${assets}/${e.category}/${e.dest ?? decodeURIComponent(new URL(e.url!).pathname.split('/').pop()!)}`
	return `${assets}/${e.category}/${e.dest ?? e.name}`
}

/** Fetches one entry unless its stamp says it is already complete. */
async function fetchEntry(e:Entry, force:boolean):Promise<{stamp:Stamp, skipped:boolean}> {
	const dest = destination(e)
	if (!force) {
		try {
			const stamp = JSON.parse(await Deno.readTextFile(stampPath(e))) as Stamp
			if (stamp.key === key(e) && await exists(dest)) {
				stamp.licence = e.licence
				stamp.redistribute = e.redistribute ?? false
				await Deno.writeTextFile(stampPath(e), JSON.stringify(stamp, null, '\t'))
				return {stamp, skipped: true}
			}
		}
		catch { /* No stamp yet. */ }
	}
	const work = `${cache}/work/${e.category}-${e.name}`
	await Deno.remove(work, {recursive: true}).catch(() => {})
	await Deno.mkdir(work, {recursive: true})
	let hash:string | undefined
	try {
		if (isPlainFile(e)) {
			await download(e.url!, `${work}/file`)
			hash = await sha256File(`${work}/file`)
			if (e.sha256 && e.sha256 !== hash) throw new Error(`sha256 mismatch: expected ${e.sha256}, got ${hash}`)
			await replace(`${work}/file`, dest)
		}
		else if (e.commands) {
			// 1. Fetch the inputs and the tool, 2. run the commands with the output folder as working directory.
			const src = `${work}/src`, toolDir = `${work}/tool`, out = `${work}/out`
			if (e.git || e.polyhaven || e.url) hash = await fetchSource(e, src, work)
			let bin = ''
			if (e.tool) {
				await download(e.tool.url, `${work}/tool.zip`)
				await unpack(`${work}/tool.zip`, toolDir)
				bin = `${toolDir}/${e.tool.bin}`
				await Deno.chmod(bin, 0o755)
			}
			await Deno.mkdir(out, {recursive: true})
			for (const cmd of e.commands) {
				const argv = cmd.map(a => a.replaceAll('{tool}', bin).replaceAll('{src}', src).replaceAll('{out}', out))
				await run(argv, out)
			}
			await select(out, dest)
		}
		else {
			const src = `${work}/src`
			hash = await fetchSource(e, src, work)
			if (e.extract?.pak) await unpackPaks(src)
			await select(src, dest, e.extract?.keep, e.extract?.strip ?? e.git?.strip ?? 0)
		}
	}
	finally {
		await Deno.remove(work, {recursive: true}).catch(() => {})
	}
	const {files, bytes} = await measure(dest)
	const stamp:Stamp = {key: key(e), files, bytes, dest: dest.slice(assets.length + 1), licence: e.licence,
		redistribute: e.redistribute ?? false, url: e.git?.repo ?? e.url ?? (e.polyhaven ? `https://polyhaven.com/a/${e.polyhaven.id}` : ''), sha256: hash}
	await Deno.mkdir(`${cache}/stamps/${e.category}`, {recursive: true})
	await Deno.writeTextFile(stampPath(e), JSON.stringify(stamp, null, '\t'))
	return {stamp, skipped: false}
}

//endregion

/*┌─────────────────╮
  | Main            |
  └─────────────────╯*/
//region Main

/** Writes `local/assets/LICENCES.md` from every stamp, so the provenance of each file is one lookup away. */
async function writeLicences(entries:Entry[]):Promise<void> {
	const lines = ['# Test asset licences', '', 'Generated by `tools/fetch-test-assets.ts`.  "no" in the last column means the files may be used for local testing but must not be committed or published.', '',
		'| Path | Licence | Source | Redistribute |', '|---|---|---|---|']
	for (const e of entries) {
		try {
			const s = JSON.parse(await Deno.readTextFile(stampPath(e))) as Stamp
			lines.push(`| \`${s.dest}\` | ${s.licence} | ${s.url} | ${s.redistribute ? 'yes' : 'no'} |`)
		}
		catch { /* Not fetched. */ }
	}
	await Deno.writeTextFile(`${assets}/LICENCES.md`, lines.join('\n') + '\n')
}

async function main():Promise<void> {
	const args = [...Deno.args]
	const force = args.includes('--force'), list = args.includes('--list')
	const jobsAt = args.indexOf('--jobs')
	const jobs = jobsAt >= 0 ? Number(args[jobsAt + 1]) : 8
	const filters = args.filter((a, i) => !a.startsWith('--') && (jobsAt < 0 || i !== jobsAt + 1))
	const manifest = parseJsonc(await Deno.readTextFile(manifestPath)) as {entries:Entry[]}
	const all = manifest.entries
	const chosen = all.filter(e => !filters.length || filters.some(f => f === e.category || f === `${e.category}/${e.name}`))
	if (list) {
		for (const e of chosen) console.log(`${e.category}/${e.name}\t${e.licence}\t${e.size ?? ''}`)
		return
	}
	if (!chosen.length) {
		console.error(`No entries match ${filters.join(' ')}.  Categories: ${[...new Set(all.map(e => e.category))].join(' ')}`)
		Deno.exit(1)
	}

	// Run the entries in parallel, a few at a time, collecting results per category.
	const results:{e:Entry, stamp?:Stamp, skipped?:boolean, error?:string}[] = []
	let next = 0
	const worker = async () => {
		while (next < chosen.length) {
			const e = chosen[next++]
			const t0 = performance.now()
			try {
				const {stamp, skipped} = await fetchEntry(e, force)
				results.push({e, stamp, skipped})
				if (!skipped) console.log(`ok    ${e.category}/${e.name}: ${stamp.files} files, ${formatBytes(stamp.bytes)} in ${((performance.now() - t0) / 1000).toFixed(0)} s`)
			}
			catch (err) {
				results.push({e, error: String(err instanceof Error ? err.message : err)})
				console.log(`FAIL  ${e.category}/${e.name}: ${String(err instanceof Error ? err.message : err).split('\n')[0]}`)
			}
		}
	}
	await Promise.all(Array.from({length: Math.max(1, jobs)}, worker))
	await writeLicences(all)

	// Summary per category.
	console.log('\nCategory        Entries  Files      Size')
	const cats = [...new Set(chosen.map(e => e.category))]
	let totalBytes = 0
	for (const c of cats) {
		const rs = results.filter(r => r.e.category === c && r.stamp)
		const files = rs.reduce((s, r) => s + r.stamp!.files, 0), bytes = rs.reduce((s, r) => s + r.stamp!.bytes, 0)
		totalBytes += bytes
		console.log(`${c.padEnd(16)}${String(rs.length).padStart(7)}${String(files).padStart(7)}${formatBytes(bytes).padStart(10)}`)
	}
	console.log(`${'total'.padEnd(30)}${formatBytes(totalBytes).padStart(10)}`)
	const failed = results.filter(r => r.error)
	if (failed.length) {
		console.log(`\n${failed.length} failed:`)
		for (const r of failed) console.log(`  ${r.e.category}/${r.e.name}: ${r.error!.split('\n').slice(0, 3).join(' | ')}`)
		Deno.exit(1)
	}
}

await main()

//endregion
