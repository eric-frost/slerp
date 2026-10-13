/**
 * The Slerp product page: install buttons that name the visitor's platform, the before-and-after divider, the
 * tabs (install steps and viewer screenshots), copy buttons, the animation loop and the format search.  Plain modern
 * JS with no build step; the page works without it, minus these.
 */

// The format list, generated from Slerp's format table, relative to this page.
const FORMATS_URL = '../../formats.json';

// Formats the site doesn't list.  Empty: the search lists every format, so the categories add up to the total the
// page states, and only the highlighted examples leave PDF out.
const UNLISTED_FORMATS = new Set();

const PLATFORM_NAMES = {linux: 'Linux', windows: 'Windows', macos: 'macOS'};

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)');

/*┌─────────────────╮
  | Platform        |
  └─────────────────╯*/
//region Platform

/**
 * Guess the visitor's desktop platform from the browser.  Phones and tablets get null, since Slerp is for desktops.
 * @returns {'linux'|'windows'|'macos'|null} */
function detectPlatform() {
	const ua = navigator.userAgent;
	const platform = (navigator.userAgentData?.platform || navigator.platform || '').toLowerCase();
	if (/android|iphone|ipad|ipod/i.test(ua) || navigator.userAgentData?.mobile)
		return null;
	if (platform.startsWith('win') || /windows/i.test(ua))
		return 'windows';
	// iPads ask for the desktop site and report themselves as Macs, but have touch screens.
	if (platform.startsWith('mac') || /mac os x/i.test(ua))
		return navigator.maxTouchPoints > 1 ? null : 'macos';
	if (/linux|x11|cros|bsd/i.test(platform + ' ' + ua))
		return 'linux';
	return null;
}

/**
 * Name the platform on the install buttons, list the other two below them and open its install tab. */
function initPlatform() {
	const platform = detectPlatform();
	const link = (p, text = PLATFORM_NAMES[p]) => `<a href="#install" data-tab-link="${p}">${text}</a>`;
	const others = Object.keys(PLATFORM_NAMES).filter(p => p !== platform);
	for (const label of $$('[data-download-label]'))
		label.textContent = platform ? `Install on ${PLATFORM_NAMES[platform]}` : 'Install Slerp';
	for (const a of $$('a[data-download]'))
		a.dataset.tabLink = platform || 'linux';
	const also = $('[data-also]');
	if (also)
		also.innerHTML = platform
			? `Also for ${link(others[0])} and ${link(others[1])}.`
			: `For ${link('linux')}, ${link('windows')} and ${link('macos')}.`;
	if (platform)
		selectTab($('#tab-' + platform));
	// Any link that names a platform opens its tab as it scrolls to the install section.
	document.addEventListener('click', e => {
		const a = e.target.closest('[data-tab-link]');
		if (a)
			selectTab($('#tab-' + a.dataset.tabLink));
	});
}
//endregion

/*┌─────────────────╮
  | Tabs            |
  └─────────────────╯*/
//region Tabs

/**
 * Show a tab's panel and hide the other panels of its tab list.  A panel with data-title also renames the window
 * title marked data-shot-title beside it, so the viewer screenshots carry their file names.
 * @param tab {HTMLElement|null}
 * @param focus {boolean} Move the keyboard focus to the tab too. */
function selectTab(tab, focus = false) {
	if (!tab)
		return;
	for (const other of $$('[role="tab"]', tab.closest('[role="tablist"]'))) {
		const on = other === tab;
		const panel = $('#' + other.getAttribute('aria-controls'));
		other.setAttribute('aria-selected', on);
		other.tabIndex = on ? 0 : -1;
		panel.hidden = !on;
		if (on && panel.dataset.title)
			panel.parentElement.querySelector('[data-shot-title]').textContent = panel.dataset.title;
	}
	if (focus)
		tab.focus();
}

/**
 * Make every tab list on the page work by mouse and keyboard: arrows move between tabs, Home and End go to the ends. */
function initTabs() {
	for (const list of $$('[role="tablist"]')) {
		const tabs = $$('[role="tab"]', list);
		selectTab(tabs.find(t => t.getAttribute('aria-selected') === 'true') || tabs[0]);
		list.addEventListener('click', e => selectTab(e.target.closest('[role="tab"]')));
		list.addEventListener('keydown', e => {
			let i = tabs.indexOf(e.target.closest('[role="tab"]'));
			if (e.key === 'ArrowRight') i = (i + 1) % tabs.length;
			else if (e.key === 'ArrowLeft') i = (i - 1 + tabs.length) % tabs.length;
			else if (e.key === 'Home') i = 0;
			else if (e.key === 'End') i = tabs.length - 1;
			else return;
			e.preventDefault();
			selectTab(tabs[i], true);
		});
	}
}

/**
 * Copy a code block's text, through the Clipboard API where the page is secure and a hidden selection otherwise. */
function initCopy() {
	for (const button of $$('.copy')) {
		button.addEventListener('click', async () => {
			const text = $('code', button.parentElement).textContent;
			try {
				await navigator.clipboard.writeText(text);
			}
			catch {
				const area = Object.assign(document.createElement('textarea'), {value: text});
				area.style.cssText = 'position:fixed;opacity:0';
				document.body.append(area);
				area.select();
				document.execCommand('copy');
				area.remove();
			}
			button.textContent = 'Copied';
			clearTimeout(button.timer);
			button.timer = setTimeout(() => button.textContent = 'Copy', 1600);
		});
	}
}
//endregion

/*┌─────────────────╮
  | Before & after  |
  └─────────────────╯*/
//region Before and after

/**
 * Make the before-and-after divider draggable with a mouse, pen or finger, and steppable with the keyboard.  The
 * layers underneath can be anything; this only sets the --pos custom property they are clipped by.
 * @param root {HTMLElement} The .compare element. */
function initCompare(root) {
	const divider = $('.divider', root);
	let pos = 50, hint = 0, touched = false;

	function set(p) {
		pos = Math.min(100, Math.max(0, p));
		root.style.setProperty('--pos', pos.toFixed(2) + '%');
		const after = Math.round(100 - pos);
		divider.setAttribute('aria-valuenow', Math.round(pos));
		divider.setAttribute('aria-valuetext', after >= 100 ? "The whole folder shows Slerp's thumbnails"
			: after <= 0 ? 'The whole folder shows generic icons' : `${after} percent of the folder shows Slerp's thumbnails`);
	}
	function stopHint() {
		touched = true;
		cancelAnimationFrame(hint);
	}
	const fromPointer = e => {
		const box = root.getBoundingClientRect();
		return (e.clientX - box.left) / box.width * 100;
	};

	// 1. Pointer: press anywhere in the folder to move the divider there, then drag.  Vertical swipes still scroll
	// the page on touch screens, because the element only claims horizontal panning (touch-action: pan-y).
	root.addEventListener('pointerdown', e => {
		if (e.button !== 0)
			return;
		stopHint();
		root.setPointerCapture(e.pointerId);
		root.classList.add('dragging');
		set(fromPointer(e));
	});
	root.addEventListener('pointermove', e => {
		if (root.classList.contains('dragging'))
			set(fromPointer(e));
	});
	for (const type of ['pointerup', 'pointercancel', 'lostpointercapture'])
		root.addEventListener(type, () => root.classList.remove('dragging'));

	// 2. Keyboard, as for any slider: arrows step, Shift or Page Up and Down step further, Home and End go to the ends.
	divider.addEventListener('keydown', e => {
		const step = e.shiftKey ? 10 : 4;
		const moves = {ArrowLeft: -step, ArrowDown: -step, ArrowRight: step, ArrowUp: step, PageDown: -20, PageUp: 20};
		let p;
		if (e.key in moves) p = pos + moves[e.key];
		else if (e.key === 'Home') p = 0;
		else if (e.key === 'End') p = 100;
		else return;
		e.preventDefault();
		stopHint();
		set(p);
	});

	// 3. The first time the folder scrolls into view, sweep the divider once to show that it moves.
	if (reducedMotion.matches || !('IntersectionObserver' in window))
		return;
	const observer = new IntersectionObserver(entries => {
		if (!entries[0].isIntersecting)
			return;
		observer.disconnect();
		if (touched)
			return;
		const stops = [50, 30, 70, 50], segment = 700;
		const ease = t => t < .5 ? 2 * t * t : 1 - (-2 * t + 2) ** 2 / 2;
		let start = 0;
		const frame = now => {
			start ||= now;
			const t = (now - start) / segment, i = Math.floor(t);
			if (touched)
				return;
			if (i >= stops.length - 1)
				return set(50);
			set(stops[i] + (stops[i + 1] - stops[i]) * ease(t - i));
			hint = requestAnimationFrame(frame);
		};
		setTimeout(() => hint = requestAnimationFrame(frame), 400);
	}, {threshold: .55});
	observer.observe(root);
}
//endregion

/*┌─────────────────╮
  | Loops           |
  └─────────────────╯*/
//region Loops

/**
 * Play a muted video loop only while it is on screen.  Nothing but its poster loads until it comes within a screen
 * of the viewport, and with reduced motion asked for it never plays.  Its button pauses and resumes it, and a loop
 * the visitor paused stays paused.
 * @param video {HTMLVideoElement} */
function initLoop(video) {
	const button = video.parentElement.querySelector('.loop-toggle');
	if (reducedMotion.matches || !('IntersectionObserver' in window))
		return;
	let paused = false;
	const show = playing => {
		button.classList.toggle('paused', !playing);
		button.setAttribute('aria-label', playing ? 'Pause the animation' : 'Play the animation');
	};
	button.hidden = false;
	button.addEventListener('click', () => {
		paused = !video.paused;
		if (paused)
			video.pause();
		else
			video.play().catch(() => {});
	});
	video.addEventListener('play', () => show(true));
	video.addEventListener('pause', () => show(false));
	new IntersectionObserver(entries => {
		const near = entries[0].isIntersecting;
		if (near && !paused) {
			video.preload = 'auto';
			video.autoplay = true;
			video.play().catch(() => {});
		}
		else if (!near)
			video.pause();
	}, {rootMargin: '50% 0px'}).observe(video);
	show(false);
}
//endregion

/*┌─────────────────╮
  | Formats         |
  └─────────────────╯*/
//region Formats

const escapeHtml = s => s.replace(/[&<>"']/g, c => `&#${c.charCodeAt(0)};`);

/**
 * Escape text for HTML and wrap the parts that match any search word in <mark>.
 * @param text {string}
 * @param words {string[]} Lower-case search words. */
function highlight(text, words) {
	if (!words.length)
		return escapeHtml(text);
	const pattern = new RegExp('(' + words.map(w => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|') + ')', 'gi');
	return text.split(pattern).map((part, i) => i % 2 ? `<mark>${escapeHtml(part)}</mark>` : escapeHtml(part)).join('');
}

/**
 * Load site/formats.json and let the search box and the category cards filter it.  With an empty search and no
 * category picked, the cards show; otherwise a list of matching formats grouped by category replaces them. */
async function initFormats() {
	const input = $('#fmt-q'), cards = $('#fmt-cats'), results = $('#fmt-results'), status = $('#fmt-status');

	// 0. Fetch the list only when the section comes within a screen of the viewport, or the search box is used.
	await new Promise(resolve => {
		const observer = new IntersectionObserver(entries => {
			if (entries[0].isIntersecting) {
				observer.disconnect();
				resolve();
			}
		}, {rootMargin: '100% 0px'});
		observer.observe($('#formats'));
		input.addEventListener('focus', resolve, {once: true});
	});
	let data;
	try {
		data = await (await fetch(FORMATS_URL)).json();
	}
	catch {
		status.textContent = 'The list of formats could not be loaded.';
		return;
	}

	// 1. Flatten the categories into one searchable list, and keep the card counts in step with the data.
	const formats = data.categories.flatMap(c => c.formats.filter(f => !UNLISTED_FORMATS.has(f.name)).map(f => {
		const extensions = [...f.files, ...f.byContent];
		return {...f, category: c.name, extensions, haystack: [c.name, f.name, ...extensions].join(' ').toLowerCase()};
	}));
	for (const c of data.categories) {
		const count = $(`[data-cat="${CSS.escape(c.name)}"] .n`, cards);
		if (count)
			count.textContent = formats.filter(f => f.category === c.name).length;
	}
	if (matchMedia('(max-width: 480px)').matches)
		input.placeholder = 'Search formats';

	let category = null;

	// 2. Draw the cards, or the matching formats.
	function render() {
		const query = input.value.trim().toLowerCase();
		const words = query.split(/\s+/).filter(Boolean).map(w => w.replace(/^\*/, ''));
		if (!words.length && !category) {
			cards.hidden = false;
			results.hidden = true;
			status.textContent = '';
			return;
		}
		const found = formats.filter(f => (!category || f.category === category) && words.every(w => f.haystack.includes(w)));
		const groups = Map.groupBy(found, f => f.category);
		const plural = n => n === 1 ? '1 format' : `${n} formats`;
		const title = category ? `${escapeHtml(category)} <span>${found.length}</span>` : `${plural(found.length)} found`;
		status.textContent = category
			? `${category}: ${plural(found.length)}${words.length ? ` matching "${input.value.trim()}"` : ''}`
			: `${plural(found.length)} matching "${input.value.trim()}"`;

		let html = `<div class="res-head"><h3>${title}</h3><button type="button" class="res-back">`
			+ `<svg viewBox="0 0 16 16" aria-hidden="true"><use href="#i-chev-left"/></svg>All categories</button></div>`;
		if (!found.length)
			html += `<p class="res-empty">Nothing matches that.  Try a file extension such as <code>.vox</code>, or a program such as `
				+ `<em>Blender</em>.  Missing a format?  Ask for it on <a href="https://github.com/eric-frost/slerp">GitHub</a>.</p>`;
		for (const [name, list] of groups) {
			html += `<div class="res-group">${category ? '' : `<h4>${escapeHtml(name)}</h4>`}<ul>`;
			for (const f of list) {
				const tag = f.thumbnails === 'off' ? 'Thumbnails off by default: your desktop already makes them'
					: f.thumbnails === 'none' ? 'Animation: no thumbnail, played on a character' : '';
				html += `<li><span class="f-name">${highlight(f.name, words)}</span><span class="f-ext">`
					+ f.extensions.map(e => `<code>${highlight(e, words)}</code>`).join('')
					+ `</span>${tag ? `<span class="f-tag">${tag}</span>` : ''}</li>`;
			}
			html += '</ul></div>';
		}
		results.innerHTML = html;
		cards.hidden = true;
		results.hidden = false;
	}

	function reset() {
		input.value = '';
		category = null;
		render();
	}

	input.addEventListener('input', render);
	input.addEventListener('keydown', e => {
		if (e.key === 'Escape' && (input.value || category)) {
			e.preventDefault();
			reset();
		}
	});
	cards.addEventListener('click', e => {
		const card = e.target.closest('.cat');
		if (!card)
			return;
		category = card.dataset.cat;
		render();
		$('.res-back', results).focus({preventScroll: true});
	});
	results.addEventListener('click', e => {
		if (!e.target.closest('.res-back'))
			return;
		const name = category;
		reset();
		// Return the focus to the card that was opened, or to the search box.
		(name && $(`[data-cat="${CSS.escape(name)}"]`, cards) || input).focus({preventScroll: true});
	});
}
//endregion

/*┌─────────────────╮
  | Start           |
  └─────────────────╯*/
//region Start

function initHeader() {
	const header = $('.site-header');
	let queued = false;
	const update = () => {
		header.classList.toggle('scrolled', scrollY > 8);
		queued = false;
	};
	addEventListener('scroll', () => {
		if (!queued)
			queued = requestAnimationFrame(update);
	}, {passive: true});
	update();
}

initHeader();
initTabs();
initPlatform();
initCopy();
for (const root of $$('[data-compare]'))
	initCompare(root);
for (const video of $$('video[data-loop]'))
	initLoop(video);
initFormats();
//endregion
