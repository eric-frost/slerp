/**
 * Makes the planar and legacy YUV DDS samples that texconv can read but not write, for
 * `local/assets/textures-extra/dds-yuv/` (see the `dds-yuv` note in `tools/assets.jsonc`).
 *
 * Usage: `deno run -A tools/make-yuv-samples.ts <folder>`
 *
 * The folder must hold texconv's `<name>_YUY2.dds`, `<name>_Y210.dds` and `<name>_Y216.dds` (4:2:2, packed).
 * From them it writes `<name>_NV12.dds`, `<name>_420_OPAQUE.dds` and `<name>_NV11.dds` (from YUY2), `<name>_P010.dds`
 * (from Y210), `<name>_P016.dds` (from Y216), and `<name>_UYVY.dds` (YUY2 with each pair's bytes swapped, Direct3D 9
 * header).  4:2:0 chroma is the rounded average of each pair of rows, 4:1:1 chroma the average of each pair of
 * 4:2:2 samples.  Row pitches and plane sizes follow DirectXTex's `ComputePitch`.
 */

/** A DDS file's size and where its texels start. */
interface Dds {width:number, height:number, data:Uint8Array}

function readDds(bytes:Uint8Array):Dds {
	const v = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
	const height = v.getUint32(12, true), width = v.getUint32(16, true)
	const dx10 = String.fromCharCode(...bytes.subarray(84, 88)) === 'DX10'
	return {width, height, data: bytes.subarray(dx10 ? 148 : 128)}
}

/** A DDS header: DX10 with `dxgi`, or Direct3D 9 with this FourCC. */
function header(width:number, height:number, pitch:number, format:number | string):Uint8Array {
	const dx10 = typeof format === 'number'
	const h = new Uint8Array(dx10 ? 148 : 128), v = new DataView(h.buffer)
	h.set([0x44, 0x44, 0x53, 0x20])
	const words:[number, number][] = [[4, 124], [8, 0x100f], [12, height], [16, width], [20, pitch], [28, 1],
		[76, 32], [80, 4], [108, 0x1000]]
	for (const [at, value] of words) v.setUint32(at, value, true)
	h.set([...(dx10 ? 'DX10' : format as string)].map(c => c.charCodeAt(0)), 84)
	if (dx10) for (const [at, value] of [[128, format as number], [132, 3], [140, 1]]) v.setUint32(at, value, true)
	return h
}

/** Splits 4:2:2 pairs (Y0 U Y1 V, components of `bytes` bytes) into luma and chroma sample arrays. */
function split(d:Dds, bytes:number) {
	const pairs = Math.ceil(d.width / 2), pitch = pairs * 4 * bytes
	const read = (at:number) => bytes === 1 ? d.data[at] : d.data[at] | d.data[at + 1] << 8
	const y = new Uint16Array(d.width * d.height), u = new Uint16Array(pairs * d.height), v = new Uint16Array(pairs * d.height)
	for (let r = 0; r < d.height; r++)
		for (let k = 0; k < pairs; k++) {
			const at = r * pitch + k * 4 * bytes
			y[r * d.width + 2 * k] = read(at)
			if (2 * k + 1 < d.width) y[r * d.width + 2 * k + 1] = read(at + 2 * bytes)
			u[r * pairs + k] = read(at + bytes)
			v[r * pairs + k] = read(at + 3 * bytes)
		}
	return {y, u, v, pairs}
}

/** Writes a 4:2:0 file (NV12 layout, components of `bytes` bytes) from 4:2:2 samples. */
function planar420(d:Dds, bytes:number, dxgi:number):Uint8Array {
	const {y, u, v, pairs} = split(d, bytes)
	const pitch = pairs * 2 * bytes, rows = d.height + Math.ceil(d.height / 2)
	const out = new Uint8Array(pitch * rows)
	const write = (at:number, value:number) => {
		out[at] = value & 0xff
		if (bytes === 2) out[at + 1] = value >> 8
	}
	for (let r = 0; r < d.height; r++)
		for (let x = 0; x < d.width; x++) write(r * pitch + x * bytes, y[r * d.width + x])
	for (let j = 0; j < Math.ceil(d.height / 2); j++) {
		const r0 = 2 * j, r1 = Math.min(2 * j + 1, d.height - 1)
		for (let k = 0; k < pairs; k++) {
			const at = (d.height + j) * pitch + k * 2 * bytes
			// P010 keeps its 10 bits at the top: averaging can't disturb the low 6 zero bits once re-masked.
			const mask = dxgi === 104 ? 0xffc0 : 0xffff
			write(at, Math.round((u[r0 * pairs + k] + u[r1 * pairs + k]) / 2) & mask)
			write(at + bytes, Math.round((v[r0 * pairs + k] + v[r1 * pairs + k]) / 2) & mask)
		}
	}
	return concat(header(d.width, d.height, pitch, dxgi), out)
}

/** Writes NV11 (4:1:1) from 8-bit 4:2:2 samples. */
function nv11(d:Dds):Uint8Array {
	const {y, u, v, pairs} = split(d, 1)
	const pitch = Math.ceil(d.width / 4) * 4, quads = Math.ceil(d.width / 4)
	const out = new Uint8Array(pitch * d.height * 2)
	for (let r = 0; r < d.height; r++) {
		for (let x = 0; x < d.width; x++) out[r * pitch + x] = y[r * d.width + x]
		for (let k = 0; k < quads; k++) {
			const at = d.height * pitch + r * (pitch / 2) + k * 2
			const k1 = Math.min(2 * k + 1, pairs - 1)
			out[at] = Math.round((u[r * pairs + 2 * k] + u[r * pairs + k1]) / 2)
			out[at + 1] = Math.round((v[r * pairs + 2 * k] + v[r * pairs + k1]) / 2)
		}
	}
	return concat(header(d.width, d.height, pitch, 110), out)
}

/** Writes UYVY (Direct3D 9 FourCC) from YUY2. */
function uyvy(d:Dds):Uint8Array {
	const pitch = Math.ceil(d.width / 2) * 4
	const out = d.data.slice(0, pitch * d.height)
	for (let at = 0; at + 1 < out.length; at += 2) [out[at], out[at + 1]] = [out[at + 1], out[at]]
	return concat(header(d.width, d.height, pitch, 'UYVY'), out)
}

function concat(a:Uint8Array, b:Uint8Array):Uint8Array {
	const out = new Uint8Array(a.length + b.length)
	out.set(a)
	out.set(b, a.length)
	return out
}

const dir = Deno.args[0]
if (!dir) {
	console.error('usage: deno run -A tools/make-yuv-samples.ts <folder>')
	Deno.exit(2)
}
for (const entry of Deno.readDirSync(dir)) {
	const m = entry.name.match(/^(.*)_YUY2\.dds$/)
	if (!m) continue
	const name = `${dir}/${m[1]}`
	const yuy2 = readDds(Deno.readFileSync(`${name}_YUY2.dds`))
	Deno.writeFileSync(`${name}_NV12.dds`, planar420(yuy2, 1, 103))
	Deno.writeFileSync(`${name}_420_OPAQUE.dds`, planar420(yuy2, 1, 106))
	Deno.writeFileSync(`${name}_NV11.dds`, nv11(yuy2))
	Deno.writeFileSync(`${name}_UYVY.dds`, uyvy(yuy2))
	for (const [packed, dxgi] of [['Y210', 104], ['Y216', 105]] as const) {
		try {
			const d = readDds(Deno.readFileSync(`${name}_${packed}.dds`))
			Deno.writeFileSync(`${name}_${dxgi === 104 ? 'P010' : 'P016'}.dds`, planar420(d, 2, dxgi))
		}
		catch (e) {
			if (!(e instanceof Deno.errors.NotFound)) throw e
		}
	}
	console.log(`wrote the planar and UYVY files for ${m[1]}`)
}
