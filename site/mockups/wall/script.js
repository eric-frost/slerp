/*┌─────────────────╮
  | Pictures        |
  └─────────────────╯*/
//region Pictures

/**
 * Every picture on the page, in one place.  The pictures live in site/img/, made by
 * tools/site-images.sh, and site/img/images.json describes each one: its alt text, the sample
 * file it shows, the format and who made it.  The lists here only choose pictures and lay them
 * out; alt text, the wall's captions and the credits in the footer come from images.json, so
 * they stay in step with it.
 */
const IMG = '../../img/';

/**
 * The wall, in order.  `size` is "big" (2 by 2 cells) or "wide" (2 by 1), and `fit: "cover"`
 * fills the tile, for pictures with a background of their own (skies, scans, pixel art) rather
 * than objects on a transparent one.  `video` names a loop in loops/ (without its extension),
 * played in place of `path`, which is then its poster.  The list fills the widest layout,
 * 12 columns by 6 rows less the intro, so every picture it names can show.
 */
const WALL = [
	{path:'thumbs/ship.webp', size:'big'},
	{path:'thumbs/knight.webp'},
	{path:'thumbs/venice.webp', size:'wide', fit:'cover'},
	{path:'thumbs/silove.webp', fit:'cover'},
	{path:'thumbs/bricks.webp'},
	{path:'thumbs/lizard.webp', size:'big', fit:'cover'},
	{path:'thumbs/demon.webp'},
	{path:'thumbs/stainedlamp.webp'},
	{path:'thumbs/freedoom.webp', size:'wide', fit:'cover'},
	{path:'thumbs/nefertiti.webp'},
	{path:'thumbs/ct.webp', fit:'cover'},
	{path:'thumbs/gpx.webp'},
	{path:'thumbs/tokyo.webp', size:'big'},
	{path:'thumbs/ddscube.webp', size:'wide'},
	{path:'thumbs/chess.webp'},
	{path:'thumbs/twilight.webp', fit:'cover'},
	{path:'thumbs/lqdm3.webp'},
	{path:'thumbs/peggy.webp'},
	{path:'thumbs/brain.webp', fit:'cover'},
	{path:'thumbs/mansion.webp'},
	{path:'thumbs/diorama.webp'},
	{path:'loops/dance-poster.webp', video:'loops/dance', fit:'cover'},
	{path:'thumbs/desk.webp', fit:'cover'},
	{path:'thumbs/teapot.webp'},
	{path:'thumbs/clover.webp'},
	{path:'thumbs/wood.webp'},
	{path:'thumbs/gene.webp'},
	{path:'thumbs/bbc.webp', fit:'cover'},
	{path:'thumbs/candle.webp', fit:'cover'},
	{path:'thumbs/chest.webp'},
	{path:'thumbs/pear36.webp'},
	{path:'thumbs/island.webp'},
	{path:'thumbs/yeti.webp'},
	{path:'thumbs/shambler.webp'},
	{path:'thumbs/goldengate.webp', fit:'cover'},
	{path:'thumbs/godot.webp'},
	{path:'thumbs/pcfan.webp'},
	{path:'thumbs/tpsheet.webp', fit:'cover'},
	{path:'thumbs/village.webp'},
	{path:'thumbs/gcode.webp'},
	{path:'thumbs/xstile.webp'},
];

/** The viewer's modes, as tabs over one big screenshot.  The captions are this page's own. */
const MODES = [
	{name:'Splat', path:'screens/splat.webp', caption:'A Gaussian splat scan of a sculpture in a garden, sorted and drawn on the GPU.'},
	{name:'Model', path:'screens/model.webp', caption:'A glTF ship in the orbit camera.  Drag to turn it, scroll to come closer, or press Tab and fly.'},
	{name:'Animation', path:'screens/animation.webp', caption:'A character and its clips.  Play, pause and step them, or add the clips of an animation library.'},
	{name:'Level', path:'screens/level.webp', caption:'A LibreQuake level in first person from the player\'s start, with its baked light untouched.'},
	{name:'Texture', path:'screens/texture.webp', caption:'An HDR KTX2 texture with its format, mip levels, channels and exposure.'},
	{name:'Sky', path:'screens/sky.webp', caption:'An HDR sky as a panorama, with exposure and tone mapping.'},
	{name:'Minecraft', path:'screens/minecraft.webp', caption:'A Minecraft schematic, block by block, in the orbit camera.'},
	{name:'Material', path:'screens/material.webp', caption:'A MaterialX material on a ball.  Tiling and displacement update the preview while you drag.'},
	{name:'CT', path:'screens/ct.webp', caption:'A chest CT series, with window and level controls and the scan\'s own details.'},
	{name:'MRI', path:'screens/mri.webp', caption:'A brain MRI volume in the slice view.'},
	{name:'G-code', path:'screens/gcode.webp', caption:'A 3D printer\'s toolpath, coloured by what each move prints, layer by layer.'},
];

/** Single pictures, by the data-slot name of the place they fill. */
const SLOTS = {
	contents: {path:'screens/contents.webp'},
	turntable: {path:'loops/turntable-poster.webp', video:'loops/turntable'},
};

/** Dolphin without and with Slerp, the same folder of 28 files, for the comparison slider. */
const FILE_MANAGER = {before:'filemanager/before.webp', after:'filemanager/after.webp'};

/** The link preview set in the page's meta tags; its samples are credited with the rest. */
const SOCIAL = 'social/og.png';

/** How many wall tiles load at once rather than lazily: the first rows on most screens. */
const EAGER_TILES = 14;

//endregion

const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)').matches;

/** What images.json says about each picture, by its path under site/img/. */
const info = fetch(IMG + 'images.json').then(r => r.json()).then(d => new Map(d.pictures.map(p => [p.path, p])))
	.catch(() => new Map());

/**
 * Makes an element with attributes and children in one call.
 * @param {string} tag
 * @param {Object<string,*>} attrs Attributes; `class` and `text` are shortcuts.
 * @param {Array<Node|string>} children */
function el(tag, attrs={}, children=[]) {
	const e = document.createElement(tag);
	for (const [k, v] of Object.entries(attrs)) {
		if (v === undefined || v === null || v === false) continue;
		if (k === 'text') e.textContent = v;
		else if (k === 'class') e.className = v;
		else e.setAttribute(k, v === true ? '' : v);
	}
	e.append(...children);
	return e;
}

/**
 * Makes a looping clip that starts loading and playing only when it comes near the screen,
 * and pauses when it leaves.  Visitors who ask for reduced motion get the poster alone.
 * @param {string} name The loop's path without extension; WebM and MP4 versions exist.
 * @param {string} poster */
function loop(name, poster) {
	if (reducedMotion) return el('img', {src:IMG + poster, alt:'', loading:'lazy', decoding:'async'});
	const v = el('video', {muted:true, loop:true, playsinline:true, preload:'none'}, [
		el('source', {src:IMG + name + '.webm', type:'video/webm'}),
		el('source', {src:IMG + name + '.mp4', type:'video/mp4'}),
	]);
	v.muted = true;
	// The poster is set only when the clip comes near too, since a poster attribute loads at once.
	new IntersectionObserver(([e]) => {
		if (e.isIntersecting) {
			if (!v.poster) v.poster = IMG + poster;
			v.autoplay = true;
			v.preload = 'auto';
			v.play().catch(() => {});
		}
		else v.pause();
	}, {rootMargin:'300px'}).observe(v);
	return v;
}

/**
 * Sets the alt text of a picture or clip from images.json.  A clip is labelled as an image,
 * since it is only a moving picture.
 * @param {HTMLElement} media
 * @param {string|{path:string, video:string}} what A path under site/img/, or an entry with a poster and a loop. */
async function describe(media, what) {
	const path = typeof what === 'string' ? what : media.tagName === 'VIDEO' ? what.video + '.webm' : what.path;
	const p = (await info).get(path);
	if (!p) return;
	if (media.tagName === 'VIDEO') {
		media.setAttribute('role', 'img');
		media.setAttribute('aria-label', p.alt);
	}
	else media.alt = p.alt;
}

/*┌─────────────────╮
  | The wall        |
  └─────────────────╯*/
//region The wall

/**
 * Fills the hero grid with the tiles of WALL.  The first tiles start loading at once, the rest
 * as soon as layout shows they fit on screen (lazy loading alone would fetch the clipped ones
 * too), and their alt text and captions follow when images.json arrives.  The grid shows only
 * whole rows, so the tiles past its bottom edge are hidden from the keyboard and screen readers
 * as well.  The wall is one tab stop; the arrow keys move between its tiles. */
function buildWall() {
	const wall = document.getElementById('wall');
	const pending = new Map(); // tile -> [its picture, not yet in the page or loading, and its address]
	WALL.forEach((t, i) => {
		const cls = ['tile', t.size, t.fit === 'cover' ? 'cover' : 'contain'].filter(Boolean).join(' ');
		const media = t.video ? loop(t.video, t.path)
			: el('img', {src:i < EAGER_TILES ? IMG + t.path : undefined, alt:'', decoding:'async', fetchpriority:i < 6 ? 'high' : undefined,
				width:512, height:t.size === 'wide' ? 256 : 512});
		const file = el('span', {class:'t-file'}), format = el('span', {class:'t-format'});
		const eager = t.video || i < EAGER_TILES;
		const tile = el('figure', {class:cls, tabindex:i === 0 ? '0' : '-1'}, [eager ? media : '', el('figcaption', {}, [file, format])]);
		if (!eager) pending.set(tile, [media, IMG + t.path]);
		wall.append(tile);
		describe(media, t);
		info.then(m => {
			const p = m.get(t.path);
			if (!p) return;
			file.textContent = p.sample;
			format.textContent = t.video ? p.format + ', animated' : p.format;
		});
	});

	// 1. Fit the tiles to the rows that show, again whenever the column count changes.  A big
	//    tile cut in half by the bottom edge shrinks to one cell (repeated, since that moves
	//    the tiles after it), and tiles below the edge are kept out of reach.
	const tiles = [...wall.querySelectorAll('.tile')];
	let visited = false;
	const visible = () => {
		const box = wall.getBoundingClientRect();
		return tiles.filter(t => {
			const r = t.getBoundingClientRect();
			return r.bottom <= box.bottom + 1 && r.right <= box.right + 1;
		});
	};
	const topLeft = list => list.reduce((a, t) => {
		const r = t.getBoundingClientRect(), q = a.getBoundingClientRect();
		return r.top < q.top - 1 || (Math.abs(r.top - q.top) <= 1 && r.left < q.left) ? t : a;
	});
	const setCurrent = tile => {
		for (const t of tiles) t.tabIndex = t === tile ? 0 : -1;
	};
	const fit = () => {
		for (const t of tiles) t.classList.remove('shrunk');
		for (let pass = 0; pass < 8; pass++) {
			const edge = wall.getBoundingClientRect().bottom;
			const cut = tiles.filter(t => {
				const r = t.getBoundingClientRect();
				return t.classList.contains('big') && !t.classList.contains('shrunk') && r.top < edge - 1 && r.bottom > edge + 1;
			});
			if (!cut.length) break;
			for (const t of cut) t.classList.add('shrunk');
		}
		const list = visible();
		if (!list.length) return;
		const shown = new Set(list);
		for (const t of tiles) {
			if (shown.has(t)) t.removeAttribute('aria-hidden');
			else t.setAttribute('aria-hidden', 'true');
			if (shown.has(t) && pending.has(t)) {
				const [img, src] = pending.get(t);
				img.src = src;
				t.prepend(img);
				pending.delete(t);
			}
		}
		// The keyboard enters the wall at its top-left tile, unless a visible one has been visited.
		if (!visited || !shown.has(wall.querySelector('.tile[tabindex="0"]'))) setCurrent(topLeft(list));
	};
	new ResizeObserver(fit).observe(wall);

	// 2. Arrow keys move to the nearest visible tile in that direction, since dense packing
	//    puts tiles on screen in a different order from the list.
	wall.addEventListener('keydown', e => {
		const dirs = {ArrowRight:[1, 0], ArrowLeft:[-1, 0], ArrowDown:[0, 1], ArrowUp:[0, -1]};
		const from = e.target.closest('.tile');
		if (!from) return;
		let to = null;
		if (e.key === 'Home') to = topLeft(visible());
		else if (dirs[e.key]) {
			const [dx, dy] = dirs[e.key];
			const a = from.getBoundingClientRect();
			const ax = a.left + a.width / 2, ay = a.top + a.height / 2;
			let best = Infinity;
			for (const t of visible()) {
				if (t === from) continue;
				const b = t.getBoundingClientRect();
				const bx = b.left + b.width / 2 - ax, by = b.top + b.height / 2 - ay;
				const along = bx * dx + by * dy, across = Math.abs(bx * dy + by * dx);
				if (along <= 4) continue;
				const score = along + across * 2.5;
				if (score < best) { best = score; to = t; }
			}
		}
		if (!to) return;
		e.preventDefault();
		setCurrent(to);
		to.focus();
	});
	wall.addEventListener('focusin', e => {
		const t = e.target.closest('.tile');
		if (t) { visited = true; setCurrent(t); }
	});
}

//endregion

/*┌─────────────────╮
  | Screenshots     |
  └─────────────────╯*/
//region Screenshots

/** Puts each picture of SLOTS into the place that names it. */
function fillSlots() {
	for (const frame of document.querySelectorAll('[data-slot]')) {
		const s = SLOTS[frame.dataset.slot];
		if (!s) continue;
		const media = s.video ? loop(s.video, s.path) : el('img', {src:IMG + s.path, alt:'', loading:'lazy', decoding:'async'});
		frame.append(media);
		describe(media, s);
	}
}

/**
 * Builds the viewer's mode tabs over one big screenshot, following the ARIA tabs pattern:
 * the arrow keys, Home and End move between tabs and show each mode as they go. */
function buildStage() {
	const tabs = document.getElementById('modes');
	const frame = document.getElementById('stage-frame');
	const caption = document.getElementById('stage-caption');
	const img = el('img', {alt:'', width:1920, height:1200, loading:'lazy', decoding:'async'});
	frame.append(img);
	const buttons = MODES.map((m, i) => {
		const b = el('button', {type:'button', role:'tab', id:'mode-' + i, 'aria-controls':'stage', 'aria-selected':'false', tabindex:'-1', text:m.name});
		b.addEventListener('click', () => show(i));
		b.addEventListener('pointerenter', () => { new Image().src = IMG + m.path; }, {once:true});
		tabs.append(b);
		return b;
	});
	const show = (i, focus=false) => {
		const m = MODES[i];
		img.src = IMG + m.path;
		img.alt = '';
		describe(img, m.path);
		caption.textContent = m.caption;
		buttons.forEach((b, j) => {
			b.setAttribute('aria-selected', String(i === j));
			b.tabIndex = i === j ? 0 : -1;
		});
		document.getElementById('stage').setAttribute('aria-labelledby', 'mode-' + i);
		if (focus) buttons[i].focus();
	};
	tabs.addEventListener('keydown', e => {
		const i = buttons.indexOf(document.activeElement);
		const n = buttons.length;
		const to = {ArrowRight:(i + 1) % n, ArrowLeft:(i - 1 + n) % n, Home:0, End:n - 1}[e.key];
		if (to === undefined || i < 0) return;
		e.preventDefault();
		show(to, true);
	});
	show(0);
}

//endregion

/*┌─────────────────╮
  | File manager    |
  └─────────────────╯*/
//region File manager

/**
 * Lays Dolphin's two screenshots over each other and wires the slider between them: the
 * "without" picture shows left of the line and the "with" picture right of it.  The labels
 * move the line all the way to one side. */
function buildFileManager() {
	const box = document.getElementById('compare');
	const after = el('img', {class:'after', src:IMG + FILE_MANAGER.after, alt:'', width:1600, height:1000, loading:'lazy', decoding:'async'});
	const before = el('img', {class:'before', src:IMG + FILE_MANAGER.before, alt:'', width:1600, height:1000, loading:'lazy', decoding:'async'});
	box.prepend(after, before);
	describe(after, FILE_MANAGER.after);
	describe(before, FILE_MANAGER.before);
	const range = document.getElementById('compare-range');
	const set = v => {
		range.value = v;
		box.style.setProperty('--pos', v + '%');
		range.setAttribute('aria-valuetext', v <= 0 ? 'All with Slerp' : v >= 100 ? 'All without Slerp' : `Without Slerp left of ${v} percent, with Slerp right of it`);
	};
	range.addEventListener('input', () => set(+range.value));
	for (const b of document.querySelectorAll('[data-compare]')) b.addEventListener('click', () => set(+b.dataset.compare));
	set(+range.value);
}

//endregion

/*┌─────────────────╮
  | Format finder   |
  └─────────────────╯*/
//region Format finder

/**
 * Formats the site leaves out of its list, by extension.  None: the list shows every format, so the categories add up
 * to the total the page states; only the highlighted examples leave PDF out. */
const UNLISTED = [];

/**
 * Lists site/formats.json by category and filters it as the visitor types.  A query matches
 * an extension it starts (so "ktx" finds .ktx and .ktx2), any word of a format's name, or a
 * category's name, which shows the whole category. */
async function buildFinder() {
	const cats = document.getElementById('cats');
	const status = document.getElementById('status');
	const input = document.getElementById('q');
	let data;
	try {
		data = await (await fetch('../../formats.json')).json();
	} catch {
		status.textContent = 'The format list could not be loaded.';
		return;
	}
	const total = data.categories.reduce((n, c) => n + c.formats.length, 0);
	const summary = `${total} formats in ${data.categories.length} categories.`;
	status.textContent = summary;

	// 1. Build every category once; filtering only hides rows.
	const rows = [];
	const groups = data.categories.map(c => {
		const formats = c.formats.filter(f => !f.files.some(x => UNLISTED.includes(x)));
		const exts = [...new Set(formats.flatMap(f => f.files.concat(f.byContent)).filter(x => x.startsWith('.')))];
		const peek = exts.slice(0, 7);
		const list = el('ul', {class:'fmt-list'});
		for (const f of formats) {
			const chips = [
				...f.files.map(x => el('code', {text:x})),
				...f.byContent.map(x => el('code', {class:'by-content', title:'Recognised by its contents', text:x})),
			];
			const tag = {on:['on', 'Thumbnails'], off:['off', 'Off by default'], none:['none', 'Viewer only']}[f.thumbnails] ?? ['none', 'Viewer'];
			const li = el('li', {class:'fmt'}, [
				el('span', {class:'fmt-name', text:f.name}),
				el('span', {class:'fmt-ext'}, chips),
				el('span', {class:'tag ' + tag[0], text:tag[1]}),
			]);
			list.append(li);
			rows.push({li, f, cat:c.name.toLowerCase(), keys:f.files.concat(f.byContent).map(x => x.toLowerCase())});
		}
		const more = exts.length > peek.length ? ` +${exts.length - peek.length}` : '';
		const count = el('span', {class:'cat-count', text:String(formats.length)});
		const details = el('details', {class:'cat'}, [
			el('summary', {}, [
				el('span', {class:'cat-name', text:c.name}),
				count,
				el('span', {class:'cat-peek', text:peek.join('  ') + more}),
			]),
			list,
		]);
		cats.append(details);
		return {details, count, list};
	});

	// 2. Filter on every keystroke.
	const words = s => s.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);
	const matches = (row, q) => {
		if (row.cat.includes(q)) return true;
		for (const k of row.keys) {
			const bare = k.replace(/^\*/, '');
			if (bare.startsWith('.' + q) || bare.startsWith(q) || bare.includes('.' + q)) return true;
		}
		return q.length > 1 && words(row.f.name).some(w => w.startsWith(q));
	};
	const apply = () => {
		const q = input.value.trim().toLowerCase().replace(/^\*?\./, '');
		if (!q) {
			for (const r of rows) r.li.hidden = false;
			for (const g of groups) {
				g.details.hidden = false;
				g.details.open = false;
				g.count.textContent = String(g.list.children.length);
			}
			status.textContent = summary;
			return;
		}
		let n = 0;
		for (const r of rows) {
			const hit = matches(r, q);
			r.li.hidden = !hit;
			if (hit) n++;
		}
		for (const g of groups) {
			const hits = [...g.list.children].filter(li => !li.hidden).length;
			g.details.hidden = !hits;
			g.details.open = hits > 0;
			g.count.textContent = hits === g.list.children.length ? String(hits) : `${hits} of ${g.list.children.length}`;
		}
		const shown = input.value.trim();
		if (n) status.textContent = `${n} format${n === 1 ? '' : 's'} match "${shown}".`;
		else status.replaceChildren(`Nothing matches "${shown}" yet.  `,
			el('a', {href:'https://github.com/eric-frost/slerp/issues', text:'Ask for it on GitHub'}), ', with a sample file if you can share one.');
	};
	input.addEventListener('input', apply);
	for (const b of document.querySelectorAll('.tries button')) {
		b.addEventListener('click', () => {
			input.value = b.dataset.q;
			apply();
			input.focus();
		});
	}
	if (input.value) apply();
}

//endregion

/*┌─────────────────╮
  | Small things    |
  └─────────────────╯*/
//region Small things

/**
 * Credits the sample files behind every picture this page uses, from images.json: first those
 * whose licence asks for it (Creative Commons Attribution, MIT, BSD, Apache), then the CC0 and
 * public-domain ones as a courtesy.  Each sample is listed once, however many pictures show it. */
async function buildCredits() {
	const m = await info;
	const paths = [
		...WALL.flatMap(t => t.video ? [t.path, t.video + '.webm', t.video + '.mp4'] : [t.path]),
		...MODES.map(x => x.path),
		...Object.values(SLOTS).flatMap(s => s.video ? [s.path, s.video + '.webm', s.video + '.mp4'] : [s.path]),
		FILE_MANAGER.before, FILE_MANAGER.after, SOCIAL,
	];
	const samples = new Map();
	for (const path of paths) {
		const p = m.get(path);
		if (!p) continue;
		for (const s of p.samples ?? []) samples.set(s.file + '|' + s.author, s);
	}
	const free = s => /^CC0|public domain/i.test(s.spdx || s.licence);
	const byName = (a, b) => a.file.localeCompare(b.file, 'en', {sensitivity:'base'});
	const item = s => el('li', {}, [
		s.source ? el('a', {href:s.source, text:s.file}) : s.file,
		` by ${s.author}, `,
		s.licenceUrl ? el('a', {href:s.licenceUrl, text:s.licence}) : s.licence,
		'.',
	]);
	const all = [...samples.values()];
	document.getElementById('credits-required').append(...all.filter(s => !free(s)).sort(byName).map(item));
	document.getElementById('credits-courtesy').append(...all.filter(free).sort(byName).map(item));
}

/** Adds a Copy button to each command block. */
function addCopyButtons() {
	if (!navigator.clipboard) return;
	for (const pre of document.querySelectorAll('pre')) {
		const b = el('button', {type:'button', class:'copy', text:'Copy'});
		b.addEventListener('click', async () => {
			await navigator.clipboard.writeText(pre.querySelector('code').textContent);
			b.textContent = 'Copied';
			setTimeout(() => b.textContent = 'Copy', 1600);
		});
		pre.append(b);
	}
}

//endregion

buildWall();
fillSlots();
buildStage();
buildFileManager();
addCopyButtons();
buildFinder();
buildCredits();
