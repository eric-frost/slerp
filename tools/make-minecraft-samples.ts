#!/usr/bin/env -S deno run -A
// Writes Minecraft schematics for testing Slerp into
// local/assets/minecraft-extra/synthetic/ (or --out DIR), all generated here
// and so free of anyone's licence:
//
//   deno run -A tools/make-minecraft-samples.ts [--out DIR]
//
// - castle-town.schem (Sponge v3): a walled castle with towers, a keep, a
//   moat, a village of houses with gabled roofs, trees, fields, fences,
//   paths, flowers and lanterns on rolling terrain; about 1.7 million blocks
//   in a 256 x 96 x 256 box, for the contact sheet and the viewer.
// - huge.schem (Sponge v2): 640 x 160 x 640 cells (65 million) of terrain
//   with towers, for timing thumbnails of very large builds.
// - regions.litematic (Litematica v6): three regions apart from each other,
//   one with negative sizes, with palettes of 3, 17 and 70 states so the
//   packed arrays use 2, 5 and 7 bits per block.
// - well.nbt: a vanilla structure file (a well with stairs, slabs, a fence
//   roof and water).
// - classic.schematic: an MCEdit schematic with numeric block ids and data
//   values (wool colours, stairs, logs, glass).

const root = new URL('..', import.meta.url).pathname
const args = Deno.args
const outIndex = args.indexOf('--out')
const out = outIndex >= 0 ? args[outIndex + 1] : `${root}local/assets/minecraft-extra/synthetic`
await Deno.mkdir(out, { recursive: true })

/*┌─────────────────╮
  | NBT writing     |
  └─────────────────╯*/
//region NBT writing

type Tag =
	| { t: 'byte' | 'short' | 'int' | 'long', v: number | bigint }
	| { t: 'string', v: string }
	| { t: 'bytes', v: Uint8Array }
	| { t: 'ints', v: Int32Array }
	| { t: 'longs', v: BigInt64Array }
	| { t: 'list', of: number, v: Tag[] }
	| { t: 'compound', v: [string, Tag][] }

const byte = (v: number): Tag => ({ t: 'byte', v })
const short = (v: number): Tag => ({ t: 'short', v })
const int = (v: number): Tag => ({ t: 'int', v })
const str = (v: string): Tag => ({ t: 'string', v })
const comp = (v: [string, Tag][]): Tag => ({ t: 'compound', v })
const list = (of: number, v: Tag[]): Tag => ({ t: 'list', of, v })
const kinds: Record<string, number> = { byte: 1, short: 2, int: 3, long: 4, bytes: 7, string: 8, list: 9, compound: 10, ints: 11, longs: 12 }

class Writer {
	parts: Uint8Array[] = []
	head = new DataView(new ArrayBuffer(8))
	num(bytes: number, f: (d: DataView) => void) {
		f(this.head)
		this.parts.push(new Uint8Array(this.head.buffer.slice(0, bytes)))
	}
	u8(v: number) { this.num(1, (d) => d.setUint8(0, v)) }
	i16(v: number) { this.num(2, (d) => d.setInt16(0, v)) }
	i32(v: number) { this.num(4, (d) => d.setInt32(0, v)) }
	i64(v: bigint) { this.num(8, (d) => d.setBigInt64(0, v)) }
	string(s: string) {
		const b = new TextEncoder().encode(s)
		this.i16(b.length)
		this.parts.push(b)
	}
	tag(tag: Tag) {
		switch (tag.t) {
			case 'byte': return this.u8(Number(tag.v) & 255)
			case 'short': return this.i16(Number(tag.v))
			case 'int': return this.i32(Number(tag.v))
			case 'long': return this.i64(BigInt(tag.v))
			case 'string': return this.string(tag.v)
			case 'bytes':
				this.i32(tag.v.length)
				this.parts.push(tag.v)
				return
			case 'ints': {
				this.i32(tag.v.length)
				const b = new DataView(new ArrayBuffer(tag.v.length * 4))
				tag.v.forEach((x, i) => b.setInt32(i * 4, x))
				this.parts.push(new Uint8Array(b.buffer))
				return
			}
			case 'longs': {
				this.i32(tag.v.length)
				const b = new DataView(new ArrayBuffer(tag.v.length * 8))
				tag.v.forEach((x, i) => b.setBigInt64(i * 8, x))
				this.parts.push(new Uint8Array(b.buffer))
				return
			}
			case 'list':
				this.u8(tag.v.length ? tag.of : 0)
				this.i32(tag.v.length)
				tag.v.forEach((e) => this.tag(e))
				return
			case 'compound':
				for (const [k, v] of tag.v) {
					this.u8(kinds[v.t])
					this.string(k)
					this.tag(v)
				}
				this.u8(0)
		}
	}
}

async function writeNbt(path: string, name: string, rootTag: Tag) {
	const w = new Writer()
	w.u8(10)
	w.string(name)
	w.tag(rootTag)
	const raw = new Blob(w.parts as BlobPart[]).stream().pipeThrough(new CompressionStream('gzip'))
	const bytes = new Uint8Array(await new Response(raw).arrayBuffer())
	await Deno.writeFile(path, bytes)
	console.log(`wrote ${path} (${(bytes.length / 1024).toFixed(0)} KB)`)
}

function varints(cells: Uint32Array): Uint8Array {
	const out: number[] = []
	const chunks: Uint8Array[] = []
	for (const v0 of cells) {
		let v = v0
		while (v >= 0x80) {
			out.push((v & 0x7f) | 0x80)
			v >>>= 7
		}
		out.push(v)
		if (out.length > 1 << 20) {
			chunks.push(Uint8Array.from(out))
			out.length = 0
		}
	}
	chunks.push(Uint8Array.from(out))
	const total = chunks.reduce((n, c) => n + c.length, 0)
	const all = new Uint8Array(total)
	let at = 0
	for (const c of chunks) {
		all.set(c, at)
		at += c.length
	}
	return all
}

//endregion

/*┌─────────────────╮
  | Building        |
  └─────────────────╯*/
//region Building

/** A box of block states, x fastest, then z, then y, as schematics store them. */
class Volume {
	cells: Uint32Array
	palette = ['minecraft:air']
	ids = new Map<string, number>([['minecraft:air', 0]])
	constructor(public w: number, public h: number, public l: number) {
		this.cells = new Uint32Array(w * h * l)
	}
	id(state: string): number {
		const s = state.includes(':') ? state : `minecraft:${state}`
		let i = this.ids.get(s)
		if (i === undefined) {
			i = this.palette.length
			this.palette.push(s)
			this.ids.set(s, i)
		}
		return i
	}
	inside(x: number, y: number, z: number) {
		return x >= 0 && y >= 0 && z >= 0 && x < this.w && y < this.h && z < this.l
	}
	set(x: number, y: number, z: number, state: string) {
		if (this.inside(x, y, z)) this.cells[(y * this.l + z) * this.w + x] = this.id(state)
	}
	get(x: number, y: number, z: number): string {
		return this.inside(x, y, z) ? this.palette[this.cells[(y * this.l + z) * this.w + x]] : 'minecraft:air'
	}
	fill(x0: number, y0: number, z0: number, x1: number, y1: number, z1: number, state: string) {
		const id = this.id(state)
		for (let y = Math.max(0, y0); y <= Math.min(this.h - 1, y1); y++) {
			for (let z = Math.max(0, z0); z <= Math.min(this.l - 1, z1); z++) {
				const row = (y * this.l + z) * this.w
				for (let x = Math.max(0, x0); x <= Math.min(this.w - 1, x1); x++) this.cells[row + x] = id
			}
		}
	}
}

/** A repeatable random number from 0 to 1. */
function rand(x: number, y: number, seed = 0) {
	let h = (x * 374761393 + y * 668265263 + seed * 2246822519) >>> 0
	h = Math.imul(h ^ (h >>> 13), 1274126177) >>> 0
	return ((h ^ (h >>> 16)) >>> 0) / 4294967296
}

function terrain(v: Volume, base: number, amp: number, flatten: (x: number, z: number) => number | null) {
	const heights = new Int32Array(v.w * v.l)
	for (let z = 0; z < v.l; z++) {
		for (let x = 0; x < v.w; x++) {
			let h = Math.round(base + amp * (Math.sin(x * 0.031) * Math.cos(z * 0.027) + 0.5 * Math.sin((x + z) * 0.071)) + rand(x, z) * 1.2)
			const f = flatten(x, z)
			if (f !== null) h = f
			heights[z * v.w + x] = h
			v.fill(x, 0, z, x, h - 4, z, 'stone')
			v.fill(x, h - 3, z, x, h - 1, z, 'dirt')
			v.set(x, h, z, 'grass_block[snowy=false]')
		}
	}
	return heights
}

function tree(v: Volume, x: number, y: number, z: number, wood: string, height: number) {
	for (let i = 1; i <= height; i++) v.set(x, y + i, z, `${wood}_log[axis=y]`)
	for (let dy = height - 2; dy <= height + 1; dy++) {
		const r = dy > height ? 1 : 2
		for (let dz = -r; dz <= r; dz++) {
			for (let dx = -r; dx <= r; dx++) {
				if (Math.abs(dx) + Math.abs(dz) > r + 1 || (dx === 0 && dz === 0 && dy <= height)) continue
				if (v.get(x + dx, y + dy, z + dz) === 'minecraft:air') v.set(x + dx, y + dy, z + dz, `${wood}_leaves[persistent=true]`)
			}
		}
	}
}

/** A house: cobblestone footing, plank walls with log corners, windows, a door and a gabled roof. */
function house(v: Volume, x0: number, y: number, z0: number, w: number, l: number, wall: string, roof: string) {
	const x1 = x0 + w - 1, z1 = z0 + l - 1, top = y + 4
	v.fill(x0, y, z0, x1, y, z1, 'cobblestone')
	v.fill(x0, y + 1, z0, x1, top, z1, wall)
	v.fill(x0 + 1, y + 1, z0 + 1, x1 - 1, top, z1 - 1, 'air')
	for (const [cx, cz] of [[x0, z0], [x1, z0], [x0, z1], [x1, z1]]) v.fill(cx, y + 1, cz, cx, top, cz, 'oak_log[axis=y]')
	for (let x = x0 + 2; x < x1 - 1; x += 3) {
		v.set(x, y + 2, z0, 'glass_pane')
		v.set(x, y + 2, z1, 'glass_pane')
	}
	const door = x0 + Math.floor(w / 2)
	v.set(door, y + 1, z0, 'oak_door[facing=north,half=lower,hinge=left,open=false]')
	v.set(door, y + 2, z0, 'oak_door[facing=north,half=upper,hinge=left,open=false]')
	v.set(door, y + 3, z0 - 1, 'wall_torch[facing=north]')
	// Roof: stairs rising from both long sides to a slab ridge.
	const half = Math.ceil(l / 2)
	for (let i = 0; i < half; i++) {
		const ry = top + 1 + i
		v.fill(x0 - 1, ry, z0 - 1 + i, x1 + 1, ry, z0 - 1 + i, `${roof}_stairs[facing=south,half=bottom]`)
		v.fill(x0 - 1, ry, z1 + 1 - i, x1 + 1, ry, z1 + 1 - i, `${roof}_stairs[facing=north,half=bottom]`)
		v.fill(x0, ry - 1, z0 + i, x0, ry - 1, z1 - i, wall)
		v.fill(x1, ry - 1, z0 + i, x1, ry - 1, z1 - i, wall)
	}
	if (l % 2 === 1) v.fill(x0 - 1, top + half, z0 + half - 1, x1 + 1, top + half, z0 + half - 1, `${roof}_slab[type=bottom]`)
}

function tower(v: Volume, cx: number, y: number, cz: number, r: number, height: number) {
	for (let dz = -r; dz <= r; dz++) {
		for (let dx = -r; dx <= r; dx++) {
			const d = Math.hypot(dx, dz)
			if (d > r + 0.4) continue
			const wall = d > r - 1.5
			v.fill(cx + dx, y, cz + dz, cx + dx, y + height, cz + dz, wall ? (rand(dx, dz, 5) < 0.2 ? 'mossy_stone_bricks' : 'stone_bricks') : 'air')
			if (!wall) v.set(cx + dx, y, cz + dz, 'spruce_planks')
			if (wall && (dx + dz) % 2 === 0) v.set(cx + dx, y + height + 1, cz + dz, 'stone_brick_wall')
			if (wall && dz === 0 && (y + height) % 1 === 0) {
				for (let wy = y + 6; wy < y + height - 2; wy += 6) v.set(cx + dx, wy, cz + dz, 'glass_pane')
			}
		}
	}
	// A cone roof of dark oak stairs facing outwards.
	for (let i = 0; i <= r + 1; i++) {
		const rr = r + 1 - i
		for (let dz = -rr; dz <= rr; dz++) {
			for (let dx = -rr; dx <= rr; dx++) {
				const d = Math.max(Math.abs(dx), Math.abs(dz))
				if (d !== rr) continue
				const facing = Math.abs(dx) >= Math.abs(dz) ? (dx > 0 ? 'west' : 'east') : (dz > 0 ? 'north' : 'south')
				v.set(cx + dx, y + height + 2 + i, cz + dz, rr === 0 ? 'dark_oak_planks' : `dark_oak_stairs[facing=${facing},half=bottom]`)
			}
		}
	}
	v.set(cx, y + height + 4 + r, cz, 'lantern[hanging=false]')
}

function castleTown(): Volume {
	const v = new Volume(256, 96, 256)
	const c = 128, wallR = 46, moatIn = 50, moatOut = 56, ground = 24
	const flat = (x: number, z: number) => {
		const d = Math.max(Math.abs(x - c), Math.abs(z - c))
		if (d <= moatOut + 2) return ground
		return null
	}
	const heights = terrain(v, 24, 5, flat)
	// The moat.
	for (let z = 0; z < v.l; z++) {
		for (let x = 0; x < v.w; x++) {
			const d = Math.max(Math.abs(x - c), Math.abs(z - c))
			if (d >= moatIn && d <= moatOut) {
				v.fill(x, ground - 3, z, x, ground, z, 'water[level=0]')
				v.set(x, ground - 4, z, rand(x, z, 9) < 0.5 ? 'sand' : 'gravel')
			}
		}
	}
	// A bridge to the gate.
	v.fill(c - 2, ground + 1, c - moatOut - 1, c + 2, ground + 1, c - moatIn + 1, 'spruce_planks')
	for (let z = c - moatOut - 1; z <= c - moatIn + 1; z += 2) {
		v.set(c - 3, ground + 2, z, 'spruce_fence')
		v.set(c + 3, ground + 2, z, 'spruce_fence')
	}
	// Curtain walls with a walkway, crenellations and a gate.
	for (let z = c - wallR; z <= c + wallR; z++) {
		for (let x = c - wallR; x <= c + wallR; x++) {
			const d = Math.max(Math.abs(x - c), Math.abs(z - c))
			if (d >= wallR - 2) {
				v.fill(x, ground + 1, z, x, ground + 14, z, rand(x, z, 3) < 0.15 ? 'cobblestone' : 'stone_bricks')
				if (d === wallR && (x + z) % 2 === 0) v.set(x, ground + 15, z, 'stone_brick_wall')
			}
		}
	}
	v.fill(c - 2, ground + 1, c - wallR, c + 2, ground + 6, c - wallR + 2, 'air')
	v.fill(c - 2, ground + 7, c - wallR, c + 2, ground + 7, c - wallR, 'stone_brick_slab[type=top]')
	for (const [dx, dz] of [[-1, -1], [1, -1], [-1, 1], [1, 1]]) tower(v, c + dx * wallR, ground + 1, c + dz * wallR, 6, 22)
	// The keep: a stone hall with tall windows, a red roof and a banner of
	// wool.
	const k0 = c - 14, k1 = c + 14
	v.fill(k0, ground + 1, k0 + 4, k1, ground + 20, k1 - 4, 'polished_andesite')
	v.fill(k0 + 1, ground + 1, k0 + 5, k1 - 1, ground + 19, k1 - 5, 'air')
	v.fill(k0 + 1, ground + 1, k0 + 5, k1 - 1, ground + 1, k1 - 5, 'dark_oak_planks')
	for (let x = k0 + 3; x < k1 - 1; x += 4) {
		v.fill(x, ground + 5, k0 + 4, x, ground + 15, k0 + 4, 'light_blue_stained_glass_pane')
		v.fill(x, ground + 5, k1 - 4, x, ground + 15, k1 - 4, 'light_blue_stained_glass_pane')
	}
	for (let i = 0; i <= 11; i++) {
		v.fill(k0 - 1, ground + 21 + i, k0 + 3 + i, k1 + 1, ground + 21 + i, k0 + 3 + i, 'red_terracotta')
		v.fill(k0 - 1, ground + 21 + i, k1 - 3 - i, k1 + 1, ground + 21 + i, k1 - 3 - i, 'red_terracotta')
	}
	v.fill(k0 - 1, ground + 33, c, k1 + 1, ground + 33, c, 'red_terracotta')
	tower(v, c, ground + 1, c, 4, 44)
	v.fill(c, ground + 55, c, c, ground + 61, c, 'oak_fence')
	v.fill(c + 1, ground + 58, c, c + 4, ground + 61, c, 'red_wool')
	// The courtyard: a well, gardens and lanterns.
	v.fill(c - 30, ground, c - 30, c + 30, ground, c - 18, 'dirt_path')
	for (let x = c - 36; x <= c + 36; x += 6) v.set(x, ground + 1, c - 22, 'lantern[hanging=false]')
	for (let i = 0; i < 300; i++) {
		const x = c - 42 + Math.floor(rand(i, 1, 4) * 84), z = c - 42 + Math.floor(rand(i, 2, 4) * 84)
		if (v.get(x, ground + 1, z) === 'minecraft:air' && v.get(x, ground, z).startsWith('minecraft:grass_block')) {
			v.set(x, ground + 1, z, ['poppy', 'dandelion', 'short_grass', 'cornflower', 'oxeye_daisy'][i % 5])
		}
	}
	// The village outside: houses along a road, fields and trees.
	v.fill(c - 2, ground, 0, c + 2, ground, c - moatOut - 2, 'dirt_path')
	const roofs = ['spruce', 'dark_oak', 'brick', 'stone_brick']
	const walls = ['oak_planks', 'birch_planks', 'spruce_planks', 'white_terracotta']
	let n = 0
	for (let z = 6; z < c - moatOut - 14; z += 16) {
		for (const side of [-1, 1]) {
			const x0 = side < 0 ? c - 16 : c + 6
			const y = heights[(z + 4) * v.w + x0 + 4]
			v.fill(x0 - 1, y - 6, z - 1, x0 + 11, y - 1, z + 9, 'dirt')
			v.fill(x0 - 1, y + 1, z - 1, x0 + 11, y + 40, z + 9, 'air')
			house(v, x0, y, z, 11, 9, walls[n % 4], roofs[n % 4])
			n++
		}
	}
	for (let z = 20; z < 236; z += 2) {
		for (let x = 4; x < 60; x++) {
			const y = heights[z * v.w + x]
			if (Math.max(Math.abs(x - c), Math.abs(z - c)) <= moatOut + 3 || x > c - 20) continue
			if (x % 14 === 0) v.set(x, y + 1, z, 'oak_fence')
			else if (z % 6 === 0) v.set(x, y, z, 'water[level=0]')
			else {
				v.set(x, y, z, 'farmland[moisture=7]')
				v.set(x, y + 1, z, `wheat[age=${(x + z) % 8}]`)
			}
		}
	}
	for (let i = 0; i < 220; i++) {
		const x = 2 + Math.floor(rand(i, 7, 1) * 252), z = 2 + Math.floor(rand(i, 8, 1) * 252)
		const d = Math.max(Math.abs(x - c), Math.abs(z - c))
		if (d < moatOut + 4 || (x > c - 20 && x < c + 22 && z < c - moatOut)) continue
		if ((x < 62 && z > 18 && z < 238)) continue
		const y = heights[z * v.w + x]
		tree(v, x, y, z, i % 3 === 0 ? 'birch' : i % 5 === 0 ? 'spruce' : 'oak', 4 + (i % 3))
	}
	return v
}

function huge(): Volume {
	const v = new Volume(640, 160, 640)
	const heights = terrain(v, 50, 18, () => null)
	for (let i = 0; i < 400; i++) {
		const x = Math.floor(rand(i, 3, 2) * 630), z = Math.floor(rand(i, 4, 2) * 630)
		const y = heights[z * v.w + x]
		if (i % 4 === 0) v.fill(x, y + 1, z, x + 6, Math.min(158, y + 40), z + 6, ['white_concrete', 'cyan_terracotta', 'glass', 'bricks'][i % 16 >> 2])
		else tree(v, x, y, z, 'oak', 5)
	}
	return v
}

//endregion

/*┌─────────────────╮
  | Writing files   |
  └─────────────────╯*/
//region Writing files

async function sponge(path: string, v: Volume, version: 2 | 3) {
	const palette = comp(v.palette.map((s, i) => [s, int(i)]))
	const data: Tag = { t: 'bytes', v: varints(v.cells) }
	const size: [string, Tag][] = [['Width', short(v.w)], ['Height', short(v.h)], ['Length', short(v.l)]]
	const meta: Tag = comp([['Name', str('Castle town')], ['Author', str('Slerp test generator')]])
	if (version === 3) {
		const inner = comp([['Version', int(3)], ['DataVersion', int(3953)], ...size, ['Metadata', meta], ['Blocks', comp([['Palette', palette], ['Data', data]])]])
		await writeNbt(path, '', comp([['Schematic', inner]]))
	} else {
		await writeNbt(path, 'Schematic', comp([['Version', int(2)], ['DataVersion', int(3465)], ...size, ['Metadata', meta], ['PaletteMax', int(v.palette.length)], ['Palette', palette], ['BlockData', data]]))
	}
	let blocks = 0
	for (const c of v.cells) if (c) blocks++
	console.log(`  ${v.w}x${v.h}x${v.l}, ${blocks} blocks, ${v.palette.length} states`)
}

/** Packs indices as Litematica does: tightly, across word boundaries. */
function pack(cells: Uint32Array, bits: number): BigInt64Array {
	const words = Math.ceil((cells.length * bits) / 64)
	const out = new BigUint64Array(words)
	for (let i = 0; i < cells.length; i++) {
		const start = i * bits, w = Math.floor(start / 64), o = start % 64
		out[w] |= BigInt.asUintN(64, BigInt(cells[i]) << BigInt(o))
		if (o + bits > 64) out[w + 1] |= BigInt(cells[i]) >> BigInt(64 - o)
	}
	return new BigInt64Array(out.buffer)
}

async function litematic(path: string) {
	const regions: [string, Tag][] = []
	const xyz = (x: number, y: number, z: number) => comp([['x', int(x)], ['y', int(y)], ['z', int(z)]])
	const add = (name: string, pos: [number, number, number], size: [number, number, number], v: Volume) => {
		const bits = Math.max(2, Math.ceil(Math.log2(v.palette.length)))
		const states = v.palette.map((s) => {
			const [n, props] = s.split('[')
			const entries: [string, Tag][] = [['Name', str(n)]]
			if (props) entries.push(['Properties', comp(props.replace(']', '').split(',').map((kv) => kv.split('=') as [string, string]).map(([k, val]) => [k, str(val)]))])
			return comp(entries)
		})
		// A negative size runs back from the position, so the cells' order is
		// still from the low corner.
		regions.push([name, comp([['Position', xyz(...pos)], ['Size', xyz(...size)], ['BlockStatePalette', list(10, states)], ['BlockStates', { t: 'longs', v: pack(v.cells, bits) }], ['TileEntities', list(10, [])], ['Entities', list(10, [])]])])
		console.log(`  region ${name}: ${v.palette.length} states, ${bits} bits`)
	}
	// 3 states: a stone platform with a glass dome.
	const a = new Volume(9, 5, 9)
	a.fill(0, 0, 0, 8, 0, 8, 'smooth_stone')
	for (let y = 1; y < 5; y++) for (let z = 0; z < 9; z++) for (let x = 0; x < 9; x++) {
		const d = Math.hypot(x - 4, y * 1.6, z - 4)
		if (d > 3.6 && d < 4.6) a.set(x, y, z, 'glass')
	}
	add('dome', [0, 0, 0], [9, 5, 9], a)
	// 17 states: a striped wool tower, written with negative sizes.
	const b = new Volume(4, 16, 4)
	const dyes = ['white', 'orange', 'magenta', 'light_blue', 'yellow', 'lime', 'pink', 'gray', 'light_gray', 'cyan', 'purple', 'blue', 'brown', 'green', 'red', 'black']
	for (let y = 0; y < 16; y++) b.fill(0, y, 0, 3, y, 3, `${dyes[y]}_wool`)
	add('tower', [17, 15, 3], [-4, -16, -4], b)
	// 70 states: a sampler of blocks in rows.
	const names = ['stone', 'granite', 'diorite', 'andesite', 'cobblestone', 'bricks', 'sandstone', 'red_sandstone', 'prismarine', 'end_stone', 'purpur_block', 'quartz_block', 'netherrack', 'nether_bricks', 'blackstone', 'deepslate', 'tuff', 'calcite', 'mud_bricks', 'terracotta', 'oak_planks', 'spruce_planks', 'birch_planks', 'jungle_planks', 'acacia_planks', 'dark_oak_planks', 'mangrove_planks', 'cherry_planks', 'bamboo_planks', 'crimson_planks', 'warped_planks', 'copper_block', 'exposed_copper', 'weathered_copper', 'oxidized_copper', 'gold_block', 'iron_block', 'diamond_block', 'emerald_block', 'lapis_block', 'redstone_block', 'coal_block', 'amethyst_block', 'glowstone', 'sea_lantern', 'shroomlight', 'honeycomb_block', 'slime_block', 'ice', 'packed_ice', 'snow_block', 'clay', 'moss_block', 'hay_block', 'bone_block', 'oak_log', 'birch_log', 'cherry_log', 'pumpkin', 'melon', 'tnt', 'bookshelf', 'crafting_table', 'furnace', 'oak_leaves', 'azalea_leaves', 'mossy_cobblestone', 'obsidian', 'sponge']
	const cols = 10
	const s = new Volume(cols * 2, 1, Math.ceil(names.length / cols) * 2)
	names.forEach((n, i) => s.set((i % cols) * 2, 0, Math.floor(i / cols) * 2, n))
	add('sampler', [-30, 0, 0], [s.w, 1, s.l], s)
	const meta = comp([['Name', str('Three regions')], ['Author', str('Slerp test generator')], ['RegionCount', int(3)], ['EnclosingSize', xyz(48, 16, 14)], ['TotalBlocks', int(0)], ['TotalVolume', int(0)]])
	await writeNbt(path, '', comp([['MinecraftDataVersion', int(3953)], ['Version', int(6)], ['SubVersion', int(1)], ['Metadata', meta], ['Regions', comp(regions)]]))
}

async function structure(path: string) {
	const v = new Volume(5, 6, 5)
	v.fill(0, 0, 0, 4, 0, 4, 'cobblestone')
	v.fill(0, 1, 0, 4, 1, 4, 'stone_bricks')
	v.fill(1, 1, 1, 3, 1, 3, 'water[level=0]')
	for (const [x, z] of [[0, 0], [4, 0], [0, 4], [4, 4]]) v.fill(x, 2, z, x, 4, z, 'oak_fence')
	for (let x = 0; x < 5; x++) {
		v.set(x, 5, 0, 'oak_stairs[facing=south,half=bottom]')
		v.set(x, 5, 4, 'oak_stairs[facing=north,half=bottom]')
		for (let z = 1; z < 4; z++) v.set(x, 5, z, 'oak_slab[type=bottom]')
	}
	v.set(2, 4, 2, 'lantern[hanging=true]')
	const palette = v.palette.slice(1).map((s) => {
		const [n, props] = s.split('[')
		const e: [string, Tag][] = [['Name', str(n)]]
		if (props) e.push(['Properties', comp(props.replace(']', '').split(',').map((kv) => kv.split('=') as [string, string]).map(([k, val]) => [k, str(val)]))])
		return comp(e)
	})
	const blocks: Tag[] = []
	for (let y = 0; y < v.h; y++) for (let z = 0; z < v.l; z++) for (let x = 0; x < v.w; x++) {
		const id = v.cells[(y * v.l + z) * v.w + x]
		if (id) blocks.push(comp([['state', int(id - 1)], ['pos', list(3, [int(x), int(y), int(z)])]]))
	}
	await writeNbt(path, '', comp([['DataVersion', int(3465)], ['size', list(3, [int(5), int(6), int(5)])], ['palette', list(10, palette)], ['blocks', list(10, blocks)], ['entities', list(10, [])]]))
}

async function classic(path: string) {
	const [w, h, l] = [8, 6, 8]
	const blocks = new Uint8Array(w * h * l), data = new Uint8Array(w * h * l)
	const set = (x: number, y: number, z: number, id: number, d = 0) => {
		const i = (y * l + z) * w + x
		blocks[i] = id
		data[i] = d
	}
	for (let z = 0; z < l; z++) for (let x = 0; x < w; x++) set(x, 0, z, 35, (x + z) % 16)
	for (let y = 1; y < 5; y++) for (let x = 0; x < w; x++) {
		set(x, y, 0, 17, 0)
		set(x, y, l - 1, 17, 0)
	}
	for (let x = 1; x < w - 1; x++) set(x, 2, 0, 20)
	for (let x = 0; x < w; x++) {
		set(x, 5, 1, 53, 2)
		set(x, 5, l - 2, 53, 3)
	}
	set(3, 1, 3, 89)
	set(5, 1, 5, 38, 0)
	await writeNbt(path, 'Schematic', comp([['Width', short(w)], ['Height', short(h)], ['Length', short(l)], ['Materials', str('Alpha')], ['Blocks', { t: 'bytes', v: blocks }], ['Data', { t: 'bytes', v: data }], ['Entities', list(10, [])], ['TileEntities', list(10, [])]]))
}

//endregion

await sponge(`${out}/castle-town.schem`, castleTown(), 3)
await sponge(`${out}/huge.schem`, huge(), 2)
await litematic(`${out}/regions.litematic`)
await structure(`${out}/well.nbt`)
await classic(`${out}/classic.schematic`)
