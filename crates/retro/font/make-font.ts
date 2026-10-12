// Builds cp437.bin, the 8x16 IBM PC character set the ANSI art renderer
// draws with: 256 glyphs of 16 bytes (one byte per row, the high bit is the
// leftmost pixel), in code page 437 order.  The glyphs come from Unscii 1.1's
// `unscii-16.hex` (viznut.fi/unscii), whose own glyphs are in the Public
// Domain; only the "-full" variant, which merges in GPL-licensed Unifont,
// is not, and it is not used here.
//
//   deno run -A make-font.ts [path/to/unscii-16.hex] > ../src/cp437.bin
//
// Without an argument the hex file is fetched from viznut.fi.

// Code page 437 as Unicode: the control range shows pictures, as on a PC's
// screen, rather than control characters.
const LOW = "\u0000☺☻♥♦♣♠•◘○◙♂♀♪♫☼" +
  "►◄↕‼¶§▬↨↑↓→←∟↔▲▼";
const HIGH = "ÇüéâäàåçêëèïîìÄÅ" +
  "ÉæÆôöòûùÿÖÜ¢£¥₧ƒ" +
  "áíóúñÑªº¿⌐¬½¼¡«»" +
  "░▒▓│┤╡╢╖╕╣║╗╝╜╛┐" +
  "└┴┬├─┼╞╟╚╔╩╦╠═╬╧" +
  "╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀" +
  "αßΓπΣσµτΦΘΩδ∞φε∩" +
  "≡±≥≤⌠⌡÷≈°∙·√ⁿ²■ ";

const cp437: number[] = [];
for (let i = 0; i < 128; i++) cp437.push(i < 32 ? LOW.codePointAt(i)! : i == 127 ? 0x2302 : i);
for (const ch of HIGH) cp437.push(ch.codePointAt(0)!);
if (cp437.length != 256) throw new Error("table is " + cp437.length);

const path = Deno.args[0];
const hex = path ? await Deno.readTextFile(path)
  : await (await fetch("http://viznut.fi/unscii/unscii-16.hex")).text();
const glyphs = new Map<number, Uint8Array>();
for (const line of hex.split("\n")) {
  const m = line.match(/^([0-9A-Fa-f]+):([0-9A-Fa-f]{32})$/);
  if (!m) continue;
  const bytes = new Uint8Array(16);
  for (let i = 0; i < 16; i++) bytes[i] = parseInt(m[2].substr(i * 2, 2), 16);
  glyphs.set(parseInt(m[1], 16), bytes);
}
// Unscii lacks four of the 256: the sun, the house, the reversed not sign and
// the bullet operator.  The sun and bullet borrow the nearest Unscii glyphs
// (the circled dot and the dot operator), the reversed not sign mirrors the
// not sign, and the house is drawn here.
glyphs.set(0x263c, glyphs.get(0x2609)!);
glyphs.set(0x2219, glyphs.get(0x22c5)!);
glyphs.set(0x2310, glyphs.get(0xac)!.map((b) => parseInt(b.toString(2).padStart(8, "0").split("").reverse().join(""), 2)));
glyphs.set(0x2302, Uint8Array.of(0, 0, 0, 0, 0x18, 0x3c, 0x7e, 0xe7, 0xc3, 0xc3, 0xc3, 0xff, 0, 0, 0, 0));
const out = new Uint8Array(256 * 16);
const missing: number[] = [];
cp437.forEach((cp, i) => {
  const g = glyphs.get(cp);
  if (g) out.set(g, i * 16); else if (cp != 0 && cp != 0xa0) missing.push(i);
});
if (missing.length) console.error("missing glyphs for codes", missing);
await Deno.stdout.write(out);
