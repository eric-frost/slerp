// Slerp, "the plain page" mockup.  Small enhancements to a page that reads
// fine without them: copy buttons on the command blocks, the format lookup
// and full category lists filled from site/formats.json, and a video loop
// that plays only while it is on screen.

/** Where the format table lives, relative to this page.  On slerp3d.com the
 * page sits beside it, so this becomes "formats.json". */
const FORMATS_URL = '../../formats.json';

/** The one place that says what each thumbnail setting means to a reader.
 * These are trusted HTML. */
const THUMBS = {
	on: 'Thumbnails and the viewer.',
	off: 'Opens in the viewer.  Thumbnails are off by default, since desktops already draw these; switch them on with <code>slerp associate</code>.',
	none: 'Read for its animation, which the viewer plays on a character.  Nothing to draw as a thumbnail.',
};

/*┌─────────────────╮
  | Copy buttons    |
  └─────────────────╯*/
//region Copy buttons
for (const block of document.querySelectorAll('.code')) {
	const button = document.createElement('button');
	button.type = 'button';
	button.className = 'copy';
	button.textContent = 'Copy';
	button.setAttribute('aria-label', 'Copy these commands');
	button.addEventListener('click', async () => {
		const text = block.querySelector('code').textContent.replace(/\n$/, '');
		try {
			await navigator.clipboard.writeText(text);
			button.textContent = 'Copied';
		}
		catch {
			button.textContent = 'Select and copy';
		}
		button.classList.add('done');
		setTimeout(() => { button.textContent = 'Copy'; button.classList.remove('done'); }, 1600);
	});
	block.append(button);
}
//endregion

/*┌─────────────────╮
  | Formats         |
  └─────────────────╯*/
//region Formats

/**
 * One searchable entry per format.
 * @typedef {{name:string, category:string, files:string[], byContent:string[], thumbnails:string}} Entry */

/**
 * Turns one claimed file pattern into a test on a file name: ".ktx2" is a
 * suffix, "*_height.png" a glob.
 * @param pattern {string}
 * @returns {(fileName:string) => boolean} */
function matcher(pattern) {
	if (!pattern.includes('*') && pattern.startsWith('.'))
		return name => name.endsWith(pattern);
	const source = pattern.split('*').map(part => part.replace(/[.+?^${}()|[\]\\]/g, '\\$&')).join('.*');
	const re = new RegExp('^' + source + '$');
	return name => re.test(name);
}

/**
 * Small HTML builder that escapes text.
 * @param text {string} */
function esc(text) {
	return text.replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]);
}

/**
 * The extensions of a format, with those Slerp confirms by content marked.
 * @param f {Entry} */
function extsHtml(f) {
	const plain = f.files.map(e => `<span>${esc(e)}</span>`);
	const sniffed = f.byContent.map(e =>
		`<span class="sniff" title="Other programs use ${esc(e)} too, so Slerp checks the contents first">${esc(e)}</span>`);
	return `<span class="exts">${[...plain, ...sniffed].join(' ')}</span>`;
}

/**
 * Finds the formats a query names.  A query with a dot ("model.fbx",
 * ".nii.gz") is tested as a file name; a bare word is tried as an extension
 * first, then against format and category names.
 * @param entries {Entry[]}
 * @param query {string}
 * @returns {{byFile:Entry[], byName:Entry[], partOf:string[], ext:string}} */
function find(entries, query) {
	const q = query.trim().toLowerCase().replace(/^\*/, '');
	const fileName = q.includes('.') ? (q.startsWith('.') ? 'file' + q : q) : 'file.' + q;
	const ext = fileName.slice(fileName.indexOf('.'));
	const byFile = entries.filter(f => [...f.files, ...f.byContent].some(p => matcher(p)(fileName)));

	// 1. Words also search names, so "quake" or "splat" find their formats.
	let byName = [];
	if (!q.includes('.') && q.length >= 3) {
		const word = new RegExp('\\b' + q.replace(/[.+?^${}()|[\]\\*]/g, '\\$&'), 'i');
		byName = entries.filter(f => !byFile.includes(f) && (word.test(f.name) || word.test(f.category)));
	}

	// 2. An extension Slerp reads only as part of a longer one, such as .gz.
	const partOf = byFile.length ? [] : [...new Set(entries.flatMap(f => [...f.files, ...f.byContent])
		.filter(p => p.startsWith('.') && p.endsWith(ext) && p !== ext))];
	return { byFile, byName, partOf, ext };
}

/**
 * The answer under the lookup box.
 * @param entries {Entry[]}
 * @param query {string} */
function answer(entries, query) {
	if (!query.trim())
		return '';
	const { byFile, byName, partOf, ext } = find(entries, query);
	// The viewer reads PDF, but Slerp isn't a PDF tool; the FAQ says why.
	if (ext === '.pdf' && byFile.length)
		return '<p><span class="verdict yes">In the viewer.</span>Thumbnails and the default app stay with your desktop\'s PDF reader.  '
			+ '<a href="#pdf">Why not PDF?</a></p>';
	const row = f => `<li><span class="fname">${esc(f.name)}</span><span class="fcat">${esc(f.category)}</span>`
		+ `<span class="fhow">${extsHtml(f)}<span>${THUMBS[f.thumbnails] ?? ''}</span></span></li>`;
	const shown = [...byFile, ...byName].slice(0, 8);
	const more = byFile.length + byName.length - shown.length;

	if (byFile.length) {
		const lead = byFile.length === 1 ? '' : `${byFile.length} formats use ${esc(ext)}; Slerp looks inside each file to tell which it is.`;
		return `<p><span class="verdict yes">Yes.</span>${lead}</p><ul>${shown.map(row).join('')}</ul>`
			+ (more > 0 ? `<p>And ${more} more.</p>` : '');
	}
	if (byName.length)
		return `<p><span class="verdict yes">${byName.length} ${byName.length === 1 ? 'format matches' : 'formats match'}.</span></p>`
			+ `<ul>${shown.map(row).join('')}</ul>` + (more > 0 ? `<p>And ${more} more.</p>` : '');
	if (partOf.length)
		return `<p><span class="verdict no">Not on its own.</span>Slerp reads ${partOf.map(p => `<code>${esc(p)}</code>`).join(', ')}, but not every ${esc(ext)} file.</p>`;
	return `<p><span class="verdict no">Not yet.</span>Slerp doesn't read ${esc(ext)} files.  `
		+ `<a href="https://github.com/eric-frost/slerp/issues">Ask for it</a>, ideally with a sample you may share.</p>`;
}

/**
 * Loads the format table, fills each category's full list and turns on the
 * lookup box.  Without the table the page keeps its static summary. */
async function setUpFormats() {
	let data;
	try {
		const response = await fetch(FORMATS_URL);
		if (!response.ok)
			return;
		data = await response.json();
	}
	catch {
		return;
	}

	/** @type {Entry[]} */
	const entries = data.categories.flatMap(c => c.formats.map(f => ({ ...f, category: c.name })));

	// 1. Each category row opens to its full list.
	const rows = [...document.querySelectorAll('#cats .cat')];
	for (const category of data.categories) {
		const row = rows.find(r => r.querySelector('.cat-name')?.textContent === category.name);
		if (!row)
			continue;
		row.querySelector('.cat-n').textContent = String(category.formats.length);
		const tag = f => f.thumbnails === 'off' ? '<span class="tag">thumbnail off</span>'
			: f.thumbnails === 'none' ? '<span class="tag">no thumbnail</span>' : '';
		row.querySelector('.cat-all').innerHTML = '<ul>' + category.formats.map(f =>
			`<li>${esc(f.name)}${tag(f)}<br>${extsHtml({ ...f, category: category.name })}</li>`).join('') + '</ul>';
	}

	// 2. The lookup answers as you type.
	const form = document.getElementById('lookup');
	const input = document.getElementById('ext');
	const out = document.getElementById('answer');
	const update = () => { out.innerHTML = answer(entries, input.value); };
	input.addEventListener('input', update);
	form.addEventListener('submit', e => { e.preventDefault(); update(); });
	for (const button of form.querySelectorAll('.lookup-try button'))
		button.addEventListener('click', () => { input.value = button.textContent; update(); input.focus(); });
	form.hidden = false;
}

setUpFormats();
//endregion

/*┌─────────────────╮
  | Video loops     |
  └─────────────────╯*/
//region Video loops
// A loop loads nothing, not even its poster, until it comes near the screen.
// Then it plays while on screen and pauses when it leaves; with reduced
// motion it stays a poster.  Readers without this script get the poster as
// an <img> from <noscript>, and the video stays hidden.
document.documentElement.classList.add('js');
const motion = matchMedia('(prefers-reduced-motion: no-preference)');
for (const video of document.querySelectorAll('video.loop')) {
	let near = false;
	const update = () => {
		if (near && !video.poster)
			video.poster = video.dataset.poster;
		if (near && motion.matches) {
			video.preload = 'auto';
			video.play().catch(() => {});
		}
		else
			video.pause();
	};
	new IntersectionObserver(([entry]) => { near = entry.isIntersecting; update(); }, { rootMargin: '400px 0px' }).observe(video);
	motion.addEventListener('change', update);
}
//endregion
