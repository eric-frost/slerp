#!/usr/bin/env -S deno run -A
// Makes a very large G-code file for timing `slerp thumbnail`: a sliced
// print repeated over a grid, as if a whole plate of copies were printed at
// once.  Every copy's moves go in the same layer, under one `;LAYER_CHANGE`
// comment per height, as a slicer writes a multi-object plate.
//
//   deno run -A tools/make-big-gcode.ts <in.gcode> <out.gcode> [columns] [rows]
//
// The input needs `;LAYER_CHANGE` comments (PrusaSlicer, OrcaSlicer) and
// absolute X and Y.  Copies sit 70 mm apart in X and 45 mm in Y; 15 by 10
// copies of the 3.6 MB 3DBenchy from `local/assets/paths-extra/gcode-preview/`
// make a 575 MB file with 240 layers.

const [input, output, cols = '15', rows = '10'] = Deno.args
if (!input || !output) {
	console.error('usage: make-big-gcode.ts <in.gcode> <out.gcode> [columns] [rows]')
	Deno.exit(2)
}

const lines = (await Deno.readTextFile(input)).split('\n')
const starts = lines.flatMap((l, i) => (l.trim() === ';LAYER_CHANGE' ? [i] : []))
if (starts.length === 0) {
	console.error(`${input} has no ;LAYER_CHANGE comments`)
	Deno.exit(1)
}

const head = lines.slice(0, starts[0])
const layers = starts.map((s, i) => lines.slice(s, starts[i + 1] ?? lines.length))
const xy = /([XY])(-?[0-9.]+)/g

/** The layer's moves shifted by (dx, dy) millimetres. */
function shift(layer: string[], dx: number, dy: number): string[] {
	return layer.map(l =>
		l.startsWith('G1') || l.startsWith('G0')
			? l.replace(xy, (_, axis: string, v: string) => axis + (parseFloat(v) + (axis === 'X' ? dx : dy)).toFixed(3))
			: l)
}

/** Without the comments that start a layer: for the second and later copies. */
const withoutMarker = (layer: string[]) =>
	layer.filter(l => l.trim() !== ';LAYER_CHANGE' && !l.startsWith(';Z:') && !l.startsWith(';HEIGHT:'))

const out = await Deno.open(output, { write: true, create: true, truncate: true })
const encoder = new TextEncoder()
const write = (text: string) => out.write(encoder.encode(text + '\n'))
await write(head.join('\n'))
for (const layer of layers) {
	let first = true
	for (let r = 0; r < +rows; r++) {
		for (let c = 0; c < +cols; c++) {
			const copy = shift(layer, -70 * c, -45 * r)
			await write((first ? copy : withoutMarker(copy)).join('\n'))
			first = false
		}
	}
}
out.close()
console.log(`${output}: ${layers.length} layers of ${cols} x ${rows} copies`)
