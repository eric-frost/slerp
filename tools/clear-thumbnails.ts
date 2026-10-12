// Deletes cached file-manager thumbnails of files with the given extensions,
// so Dolphin and other file managers make them again after Slerp changes how
// those formats are drawn.  They keep a thumbnail until its file changes.
//
//   deno run -A tools/clear-thumbnails.ts [--dry-run] kra ora ktx2 ...
//
// Each cached PNG names its source in a `Thumb::URI` text chunk, which KDE
// writes compressed (zTXt), so a plain text search doesn't find it.

const args = Deno.args.filter((a) => a !== "--dry-run");
const dry = Deno.args.includes("--dry-run");
const exts = new Set(args.map((e) => e.toLowerCase().replace(/^\./, "")));
if (exts.size === 0) {
	console.error("usage: clear-thumbnails.ts [--dry-run] EXT...");
	Deno.exit(2);
}

/** The `Thumb::URI` of a thumbnail PNG, or "" when it has none. */
async function uri(bytes: Uint8Array): Promise<string> {
	const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
	const latin = new TextDecoder("latin1");
	for (let at = 8; at + 8 <= bytes.length;) {
		const len = view.getUint32(at);
		const type = latin.decode(bytes.subarray(at + 4, at + 8));
		const data = bytes.subarray(at + 8, Math.min(at + 8 + len, bytes.length));
		at += 12 + len;
		if (type === "IDAT") break;
		if (type !== "tEXt" && type !== "zTXt" && type !== "iTXt") continue;
		const nul = data.indexOf(0);
		if (nul < 0 || latin.decode(data.subarray(0, nul)) !== "Thumb::URI") continue;
		let text = data.subarray(nul + 1);
		if (type === "zTXt") text = await inflate(text.subarray(1));
		if (type === "iTXt") {
			const compressed = text[0] === 1;
			text = text.subarray(2);
			text = text.subarray(text.indexOf(0) + 1);
			text = text.subarray(text.indexOf(0) + 1);
			if (compressed) text = await inflate(text);
		}
		return new TextDecoder().decode(text);
	}
	return "";
}

async function inflate(data: Uint8Array): Promise<Uint8Array> {
	const stream = new Blob([data]).stream().pipeThrough(new DecompressionStream("deflate"));
	return new Uint8Array(await new Response(stream).arrayBuffer());
}

const root = `${Deno.env.get("HOME")}/.cache/thumbnails`;
let removed = 0;
for await (const dir of Deno.readDir(root)) {
	if (!dir.isDirectory) continue;
	for await (const file of Deno.readDir(`${root}/${dir.name}`)) {
		const path = `${root}/${dir.name}/${file.name}`;
		let source = "";
		try {
			source = await uri(await Deno.readFile(path));
		} catch {
			continue;
		}
		const name = decodeURIComponent(source.split("/").pop() ?? "");
		const ext = name.includes(".") ? name.split(".").pop()!.toLowerCase() : "";
		if (!exts.has(ext)) continue;
		removed++;
		if (!dry) await Deno.remove(path);
	}
}
console.log(`${dry ? "would remove" : "removed"} ${removed} cached thumbnails`);
