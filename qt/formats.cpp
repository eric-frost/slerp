// Recognising Slerp's formats from their first bytes, for the Qt image
// plugin.  Every Qt program that opens an image may run these checks, so
// they look only at the bytes they are given and never allocate.  They
// mirror the loaders' own sniffing in the Rust crates closely enough that a
// file accepted here is almost always one the helper can read; the helper
// has the last word.

#include "formats.h"

#include <cstring>

namespace slerp {

// The table comes from the format table in crates/formats (Rust), which
// also writes slerp.json; `slerp formats --write` regenerates both.
// Textures and pictures no other Qt plugin reads are stills; models,
// levels, scenes and materials turn.  Heightmap-named PNGs have their own
// type in data/slerp.xml so Dolphin sends them to Slerp's thumbnailer;
// without a reader for that type Gwenview won't open them at all (it names
// the format by the extension, so Qt's own PNG reader still reads them).
const Format kFormats[] = {
#include "formats_table.inc"
};
const size_t kFormatCount = sizeof kFormats / sizeof kFormats[0];

const Format *formatForKey(std::string_view key) {
	for (const Format &f : kFormats)
		if (key == f.key) return &f;
	return nullptr;
}

namespace {

/*┌─────────────────╮
  | Byte helpers    |
  └─────────────────╯*/
//region Byte helpers

/** The bytes being checked, with bounds-checked readers. */
struct Bytes {
	const uint8_t *p;
	size_t n;

	bool startsWith(std::string_view s, size_t at = 0) const {
		return at <= n && n - at >= s.size() && memcmp(p + at, s.data(), s.size()) == 0;
	}
	/** A little-endian unsigned integer of `width` bytes at `at`, or -1 past the end. */
	int64_t le(size_t at, size_t width) const {
		if (at > n || n - at < width) return -1;
		uint64_t v = 0;
		for (size_t i = 0; i < width; i++) v |= uint64_t(p[at + i]) << (8 * i);
		return int64_t(v);
	}
	int64_t u32(size_t at) const { return le(at, 4); }
	int64_t u16(size_t at) const { return le(at, 2); }
	/** A big-endian 16-bit unsigned integer at `at`, or -1 past the end. */
	int64_t be16(size_t at) const {
		if (at > n || n - at < 2) return -1;
		return int64_t(p[at]) << 8 | p[at + 1];
	}
	/** A big-endian 32-bit unsigned integer at `at`, or -1 past the end. */
	int64_t be32(size_t at) const {
		if (at > n || n - at < 4) return -1;
		return int64_t(p[at]) << 24 | int64_t(p[at + 1]) << 16 | int64_t(p[at + 2]) << 8 | p[at + 3];
	}
	bool contains(std::string_view s) const {
		if (s.empty() || s.size() > n) return false;
		for (size_t i = 0; i + s.size() <= n; i++)
			if (memcmp(p + i, s.data(), s.size()) == 0) return true;
		return false;
	}
	/** The bytes after a UTF-8 byte order mark and leading white space. */
	Bytes text() const {
		size_t i = startsWith("\xEF\xBB\xBF") ? 3 : 0;
		while (i < n && (p[i] == ' ' || p[i] == '\t' || p[i] == '\r' || p[i] == '\n')) i++;
		return {p + i, n - i};
	}
	/** The first line, without its end. */
	Bytes firstLine() const {
		size_t i = 0;
		while (i < n && p[i] != '\n' && p[i] != '\r') i++;
		return {p, i};
	}
	/** True when nothing looks binary: no NUL and few control characters. */
	bool looksLikeText() const {
		size_t odd = 0;
		for (size_t i = 0; i < n; i++) {
			uint8_t c = p[i];
			if (c == 0) return false;
			if (c < 32 && c != '\t' && c != '\n' && c != '\r' && c != '\f') odd++;
		}
		return odd * 100 <= n;
	}
	/** UTF-16 text starts with a byte order mark (Collada files from some exporters). */
	bool utf16() const { return startsWith("\xFF\xFE") || startsWith("\xFE\xFF"); }
};

//endregion

/*┌─────────────────╮
  | Checks          |
  └─────────────────╯*/
//region Checks

const char kKtx2[] = "\xABKTX 20\xBB\r\n\x1A\n";
const char kKtx1[] = "\xABKTX 11\xBB\r\n\x1A\n";
const char kAstc[] = "\x13\xAB\xA1\x5C";
const char kFbxBinary[] = "Kaydara FBX Binary  ";

const char kPng[] = "\x89PNG\r\n\x1A\n";

bool isKtx2(const Bytes &b) { return b.startsWith({kKtx2, 12}); }

/** A PNG with its header chunk first, as the format requires. */
bool isPng(const Bytes &b) { return b.startsWith({kPng, 8}) && b.startsWith("IHDR", 12); }
bool isKtx1(const Bytes &b) { return b.startsWith({kKtx1, 12}); }

/** Raw ASTC: magic, then block footprints ASTC defines (4-12 by 4-12, 1-6 deep). */
bool isAstc(const Bytes &b) {
	if (!b.startsWith({kAstc, 4}) || b.n < 16) return false;
	uint8_t x = b.p[4], y = b.p[5], z = b.p[6];
	return x >= 4 && x <= 12 && y >= 4 && y <= 12 && z >= 1 && z <= 6;
}

/** Binary glTF: magic, version 2, and a total length that matches the file when its size is known. */
bool isGlb(const Bytes &b, int64_t size) {
	if (!b.startsWith("glTF") || b.u32(4) != 2) return false;
	int64_t length = b.u32(8);
	return length >= 12 && (size < 0 || length <= size);
}

bool isFbx(const Bytes &b) {
	if (b.startsWith({kFbxBinary, 20})) return true;
	Bytes t = b.text();
	return t.startsWith("; FBX") || (t.startsWith(";") && b.contains("FBXHeaderExtension"));
}

/** PLY meshes; Gaussian splat PLYs (`f_dc_0`) aren't models. */
bool isPly(const Bytes &b) {
	return (b.startsWith("ply\n") || b.startsWith("ply\r\n")) && !b.contains("f_dc_0");
}

/** Binary STL: an 80-byte header, then a triangle count that accounts for the whole file. */
bool isBinaryStl(const Bytes &b, int64_t size) {
	int64_t count = b.u32(80);
	return size >= 84 && count >= 0 && size == 84 + 50 * count;
}

bool isAsciiStl(const Bytes &b) {
	Bytes t = b.text();
	return t.startsWith("solid") && t.looksLikeText() && (t.contains("facet") || t.contains("endsolid"));
}

bool isCollada(const Bytes &b) {
	if (b.utf16()) return true;  // Can't search UTF-16 cheaply; the loader decides.
	return b.text().startsWith("<") && b.contains("<COLLADA");
}

bool isUsdc(const Bytes &b) { return b.startsWith("PXR-USDC"); }
bool isUsda(const Bytes &b) { return b.startsWith("#usda ") || b.startsWith("#usda\t"); }

/** USDZ: a zip whose first entry is a USD layer, as the format requires. */
bool isUsdz(const Bytes &b) {
	if (!b.startsWith("PK\x03\x04")) return false;
	int64_t nameLen = b.le(26, 2);
	if (nameLen < 5 || size_t(30 + nameLen) > b.n) return false;
	std::string_view name(reinterpret_cast<const char *>(b.p + 30), size_t(nameLen));
	auto endsWith = [&](std::string_view s) {
		if (name.size() < s.size()) return false;
		for (size_t i = 0; i < s.size(); i++) {
			char c = name[name.size() - s.size() + i];
			if (c >= 'A' && c <= 'Z') c = char(c - 'A' + 'a');
			if (c != s[i]) return false;
		}
		return true;
	};
	return endsWith(".usdc") || endsWith(".usda") || endsWith(".usd");
}

bool isVox(const Bytes &b) { return b.startsWith("VOX ") && b.u32(4) > 0 && b.u32(4) < 1000; }

/**
 * Quake and Half-Life BSP: a version number (or `BSP2`/`2PSB`), then 15
 * lumps whose offsets lie past the header and, when the size is known,
 * inside the file.  The version alone is too weak: 29 or 30 starts plenty
 * of other files. */
bool isBsp(const Bytes &b, int64_t size) {
	bool tagged = b.startsWith("BSP2") || b.startsWith("2PSB");
	int64_t version = b.u32(0);
	if (!tagged && version != 29 && version != 30) return false;
	constexpr int64_t kHeader = 4 + 15 * 8;
	if (int64_t(b.n) < kHeader) return false;
	for (int i = 0; i < 15; i++) {
		int64_t ofs = b.u32(4 + 8 * i), len = b.u32(8 + 8 * i);
		if (ofs < 0 || len < 0 || ofs > INT32_MAX || len > INT32_MAX) return false;
		if (len == 0) continue;
		if (ofs < kHeader) return false;
		if (size >= 0 && ofs + len > size) return false;
	}
	return true;
}

bool isQuakeMdl(const Bytes &b) { return b.startsWith("IDPO") && b.u32(4) == 6; }
bool isHalfLifeMdl(const Bytes &b) { return b.startsWith("IDST") && b.u32(4) == 10; }
bool isMd2(const Bytes &b) { return b.startsWith("IDP2") && b.u32(4) == 8; }
bool isMd3(const Bytes &b) { return b.startsWith("IDP3") && b.u32(4) == 15; }

bool isMaterialX(const Bytes &b) { return b.text().startsWith("<") && b.contains("<materialx"); }

/** A Godot resource whose first line says it is one of the materials Slerp reads. */
bool isGodotMaterial(const Bytes &b) {
	if (!b.startsWith("[gd_resource")) return false;
	Bytes line = b.firstLine();
	return line.contains("type=\"StandardMaterial3D\"") || line.contains("type=\"ORMMaterial3D\"") ||
		line.contains("type=\"SpatialMaterial\"");
}

/** MetaHuman DNA: `DNA`, generation 2, and a version openriglogic could have written. */
bool isDna(const Bytes &b) {
	int64_t version = b.n >= 7 ? int64_t(b.p[5]) << 8 | b.p[6] : -1;
	return b.startsWith({"DNA\0\x02", 5}) && version >= 1 && version <= 64;
}

/** glTF JSON: an object, plausibly text.  Only trusted with the `.gltf` name. */
bool isGltfJson(const Bytes &b) { return b.text().startsWith("{") && b.looksLikeText(); }

/**
 * Wavefront OBJ: text, nothing more is certain.  Only trusted with the
 * `.obj` name; it keeps out Windows COFF objects, which are binary. */
bool isObj(const Bytes &b) { return b.n > 0 && b.looksLikeText(); }

/*┌─────────────────╮
  | More formats    |
  └─────────────────╯*/
// Formats added with the format table.  Those with a signature are checked
// for it; raw dumps (the retro pictures) and text formats have none, so with
// their name they need only a plausible size or a text header, and the
// helper, which reads them whole, has the last word.

/** A gzip stream (`.wrz`, `.x3dz`, gzipped Blender files). */
bool isGzip(const Bytes &b) { return b.startsWith("\x1F\x8B"); }
bool isZip(const Bytes &b) { return b.startsWith("PK\x03\x04"); }

/** Aseprite: the file size, then the magic 0xA5E0, and a canvas size. */
bool isAseprite(const Bytes &b, int64_t size) {
	int64_t declared = b.u32(0);
	return b.u16(4) == 0xA5E0 && b.u16(8) > 0 && b.u16(10) > 0 && declared >= 128 && (size < 0 || declared == size);
}

bool isTmx(const Bytes &b) { return b.text().startsWith("<") && b.contains("<map"); }
bool isTiledJson(const Bytes &b) {
	return b.text().startsWith("{") && (b.contains("\"tiledversion\"") || b.contains("\"tilesets\""));
}
bool isLdtk(const Bytes &b) { return b.text().startsWith("{") && b.contains("LDtk"); }
bool isBlockbench(const Bytes &b) { return b.text().startsWith("{") && b.contains("\"meta\""); }

/** OFF meshes: the keyword, with the optional prefixes the format allows. */
bool isOff(const Bytes &b) {
	Bytes t = b.text();
	size_t i = 0;
	for (const char *prefix : {"ST", "C", "N", "4", "n"})
		if (t.startsWith(prefix, i)) i += strlen(prefix);
	return t.startsWith("OFF", i) && t.n > i + 3 && (t.p[i + 3] == ' ' || t.p[i + 3] == '\n' || t.p[i + 3] == '\r');
}

/** 3D Studio: the main chunk 0x4D4D, whose length is the file's. */
bool is3ds(const Bytes &b, int64_t size) {
	int64_t len = b.u32(2);
	return b.u16(0) == 0x4D4D && len >= 6 && (size < 0 || len <= size);
}

bool isDirectX(const Bytes &b) { return b.startsWith("xof 03"); }
bool isVrml(const Bytes &b) { return b.startsWith("#VRML V2.0") || b.startsWith("#VRML V1.0"); }
bool isX3dXml(const Bytes &b) { return b.text().startsWith("<") && b.contains("<X3D"); }
bool isX3dClassic(const Bytes &b) { return b.startsWith("#X3D V"); }
bool isLas(const Bytes &b) { return b.startsWith("LASF"); }
bool isE57(const Bytes &b) { return b.startsWith("ASTM-E57"); }
bool isPcd(const Bytes &b) {
	Bytes t = b.text();
	return t.startsWith("# .PCD") || (t.startsWith("VERSION") && t.contains("FIELDS"));
}
bool isGodotScene(const Bytes &b) { return b.startsWith("[gd_scene"); }
bool isUnityYaml(const Bytes &b) { return b.startsWith("%YAML") && b.contains("!u!"); }
bool isBlend(const Bytes &b) {
	return b.startsWith("BLENDER") || isGzip(b) || b.startsWith("\x28\xB5\x2F\xFD");
}
/** A Quake-family map source: TrenchBroom's comment or the world entity. */
bool isQuakeMap(const Bytes &b) {
	return b.looksLikeText() && (b.startsWith("// Game:") || b.contains("\"worldspawn\""));
}
bool isVmf(const Bytes &b) { return b.looksLikeText() && b.contains("versioninfo"); }

bool isXnb(const Bytes &b) {
	return b.startsWith("XNB") && b.n > 3 && strchr("wmxaiAgdp", char(b.p[3])) != nullptr;
}
bool isBlp(const Bytes &b) { return b.startsWith("BLP1") || b.startsWith("BLP2"); }
bool isPvr(const Bytes &b) { return b.startsWith("PVR\x03") || b.startsWith("PVR!", 44); }
bool isPkm(const Bytes &b) { return b.startsWith("PKM 10") || b.startsWith("PKM 20"); }
bool isQuakeSprite(const Bytes &b) { return b.startsWith("IDSP"); }
bool isQuake2Sprite(const Bytes &b) { return b.startsWith("IDS2"); }
bool isFits(const Bytes &b) { return b.startsWith("SIMPLE  ="); }
bool isXisf(const Bytes &b) { return b.startsWith("XISF0100"); }
bool isDpx(const Bytes &b) { return b.startsWith("SDPX") || b.startsWith("XPDS"); }
bool isCineon(const Bytes &b) { return b.startsWith("\x80\x2A\x5F\xD7") || b.startsWith("\xD7\x5F\x2A\x80"); }
bool isPam(const Bytes &b) { return b.startsWith("P7\n"); }
bool isFarbfeld(const Bytes &b) { return b.startsWith("farbfeld"); }
bool isPico8(const Bytes &b) { return b.startsWith("pico-8 cartridge"); }
/** Build engine tiles: version 1, little-endian. */
bool isBuildArt(const Bytes &b) { return b.startsWith({"\x01\0\0\0", 4}) || b.startsWith("BUILDART"); }

/** A raw screen dump: something there, and no bigger than a few screens. */
bool plausibleDump(int64_t size, int64_t most) { return size != 0 && (size < 0 || size <= most); }

//endregion

const Format *key(const char *k) { return formatForKey(k); }

/** Content detection without a name: strong signatures only. */
const Format *detect(const Bytes &b, int64_t size) {
	if (isKtx2(b)) return key("ktx2");
	if (isKtx1(b)) return key("ktx");
	if (isAstc(b)) return key("astc");
	if (isGlb(b, size)) return key("glb");
	if (b.startsWith({kFbxBinary, 20})) return key("fbx");
	if (isUsdc(b)) return key("usdc");
	if (isUsda(b)) return key("usda");
	if (isUsdz(b)) return key("usdz");
	if (isVox(b)) return key("vox");
	if (isQuakeMdl(b) || isHalfLifeMdl(b)) return key("mdl");
	if (isMd2(b)) return key("md2");
	if (isMd3(b)) return key("md3");
	if (isDna(b)) return key("dna");
	if (isBsp(b, size) && size >= 0) return key("bsp");
	if (isPly(b) && b.contains("\nformat ")) return key("ply");
	if (isGodotMaterial(b)) return key("tres");
	if (!b.utf16() && isCollada(b)) return key("dae");
	if (isMaterialX(b)) return key("mtlx");
	if (isBinaryStl(b, size) || isAsciiStl(b)) return key("stl");
	if (isAseprite(b, size) && size >= 0) return key("aseprite");
	if (isDirectX(b)) return key("x");
	if (isVrml(b)) return key("wrl");
	if (isX3dClassic(b)) return key("x3dv");
	if (isLas(b)) return key("las");
	if (isE57(b)) return key("e57");
	if (b.startsWith("# .PCD")) return key("pcd");
	if (isGodotScene(b)) return key("tscn");
	if (isUnityYaml(b)) return key("unity");
	if (b.startsWith("BLENDER")) return key("blend");
	if (isXnb(b)) return key("xnb");
	if (isBlp(b)) return key("blp");
	if (b.startsWith("PVR\x03")) return key("pvr");
	if (isPkm(b)) return key("pkm");
	if (isQuakeSprite(b)) return key("spr");
	if (isQuake2Sprite(b)) return key("sp2");
	if (isFits(b)) return key("fits");
	if (isXisf(b)) return key("xisf");
	if (isDpx(b)) return key("dpx");
	if (isCineon(b)) return key("cin");
	if (isPam(b)) return key("pam");
	if (isFarbfeld(b)) return key("ff");
	if (isPico8(b)) return key("p8");
	return nullptr;
}

/** Whether the content fits the format its name gives. */
bool fits(const Format *f, const Bytes &b, int64_t size) {
	std::string_view k = f->key;
	if (k == "ktx2") return isKtx2(b);
	if (k == "ktx") return isKtx1(b);
	if (k == "astc") return isAstc(b);
	if (k == "heightmap") return isPng(b);
	if (k == "glb" || k == "vrm") return isGlb(b, size);
	if (k == "gltf") return isGlb(b, size) || isGltfJson(b);
	if (k == "fbx") return isFbx(b);
	if (k == "obj") return isObj(b);
	if (k == "stl") return isBinaryStl(b, size) || isAsciiStl(b);
	if (k == "ply") return isPly(b);
	if (k == "dae") return isCollada(b);
	if (k == "usd") return isUsdc(b) || isUsda(b);
	if (k == "usda") return isUsda(b);
	if (k == "usdc") return isUsdc(b);
	if (k == "usdz") return isUsdz(b);
	if (k == "vox") return isVox(b);
	if (k == "bsp") return isBsp(b, size);
	if (k == "mdl") return isQuakeMdl(b) || isHalfLifeMdl(b);
	if (k == "md2") return isMd2(b);
	if (k == "md3") return isMd3(b);
	if (k == "mtlx") return isMaterialX(b);
	if (k == "tres") return isGodotMaterial(b);
	if (k == "dna") return isDna(b);
	// Sprites, maps and more models.
	if (k == "aseprite" || k == "ase") return isAseprite(b, size);
	if (k == "tmx") return isTmx(b);
	if (k == "tmj") return isTiledJson(b);
	if (k == "ldtk" || k == "ldtkl") return isLdtk(b);
	if (k == "3mf") return isZip(b);
	if (k == "off") return isOff(b);
	if (k == "3ds") return is3ds(b, size);
	if (k == "x") return isDirectX(b);
	if (k == "wrl" || k == "vrml") return isVrml(b) || isGzip(b);
	if (k == "wrz" || k == "x3dz" || k == "x3dvz") return isGzip(b);
	if (k == "x3d") return isX3dXml(b);
	if (k == "x3dv") return isX3dClassic(b);
	if (k == "las" || k == "laz") return isLas(b);
	if (k == "e57") return isE57(b);
	if (k == "pcd") return isPcd(b);
	if (k == "pts" || k == "ptx") return b.n > 0 && b.looksLikeText();
	if (k == "bbmodel") return isBlockbench(b);
	if (k == "tscn" || k == "escn") return isGodotScene(b);
	if (k == "unity" || k == "prefab") return isUnityYaml(b);
	if (k == "blend") return isBlend(b);
	if (k == "map") return isQuakeMap(b);
	if (k == "vmf") return isVmf(b);
	// Game textures and sprites.
	if (k == "xnb") return isXnb(b);
	if (k == "blp") return isBlp(b);
	if (k == "pvr") return isPvr(b);
	if (k == "pkm") return isPkm(b);
	if (k == "spr") return isQuakeSprite(b);
	if (k == "sp2") return isQuake2Sprite(b);
	// Science and film.
	if (k == "fits" || k == "fit" || k == "fts") return isFits(b);
	if (k == "xisf") return isXisf(b);
	if (k == "dpx") return isDpx(b);
	if (k == "cin" || k == "cineon") return isCineon(b);
	if (k == "pam") return isPam(b);
	if (k == "ff") return isFarbfeld(b);
	// Retro pictures, which are raw dumps of a computer's screen memory.
	if (k == "pi1" || k == "pi2" || k == "pi3") return size >= 32034 && size <= 32100 && b.be16(0) >= 0 && b.be16(0) <= 2;
	if (k == "pc1" || k == "pc2" || k == "pc3") {
		int64_t res = b.be16(0);
		return plausibleDump(size, 64000) && ((res >= 0 && res <= 2) || (res >= 0x8000 && res <= 0x8002));
	}
	if (k == "neo") return size == 32128 && b.be16(0) == 0 && b.be16(2) >= 0 && b.be16(2) <= 2;
	if (k == "tn1" || k == "tn2" || k == "tn3" || k == "tny") return plausibleDump(size, 64000) && b.n > 0 && b.p[0] <= 5;
	// `.art` is the Build engine's tile sets too.
	if (k == "art") return plausibleDump(size, 1 << 26) && (isBuildArt(b) || size <= 65536);
	if (k == "koa" || k == "kla" || k == "aas" || k == "dd" || k == "jj" || k == "hir" || k == "hpi" || k == "ocp")
		return plausibleDump(size, 65536);
	if (k == "hgr" || k == "dhr" || k == "mic" || k == "g15" || k == "gr15" || k == "gr9" || k == "gr8" || k == "bbc")
		return plausibleDump(size, 65536);
	if (k == "sc2" || k == "sc5" || k == "sc6" || k == "sc7" || k == "sc8" || k == "sc12" || k == "sr5" || k == "sr6" ||
		k == "sr7" || k == "sr8" || k == "sr12" || k == "s12" || k == "srs")
		return plausibleDump(size, 262144);
	if (k == "ans") return plausibleDump(size, 16 << 20) && !b.startsWith("\x7F" "ELF");
	if (k == "p8") return isPico8(b);
	if (k == "tic") return plausibleDump(size, 1 << 20);
	return false;
}

}

const Format *sniff(const uint8_t *head, size_t len, int64_t size, const Format *expected) {
	if (!head || len == 0) return nullptr;
	Bytes b{head, len < kSniffBytes ? len : kSniffBytes};
	if (!expected) return detect(b, size);
	return fits(expected, b, size) ? expected : nullptr;
}

Size textureSize(const uint8_t *head, size_t len) {
	Bytes b{head, len};
	int64_t w = -1, h = -1, faces = 1;
	if (isKtx2(b)) {
		w = b.u32(20), h = b.u32(24), faces = b.u32(36);
	} else if (isKtx1(b)) {
		// KTX1 files say which way round their integers are.
		bool swap = b.u32(12) == 0x01020304;
		auto word = [&](size_t at) -> int64_t {
			int64_t v = b.u32(at);
			if (v < 0 || !swap) return v;
			uint32_t u = uint32_t(v);
			return int64_t((u >> 24) | ((u >> 8) & 0xFF00) | ((u << 8) & 0xFF0000) | (u << 24));
		};
		w = word(36), h = word(40), faces = word(52);
	} else if (isAstc(b)) {
		w = b.le(7, 3), h = b.le(10, 3);
	} else if (isPng(b)) {
		w = b.be32(16), h = b.be32(20);
	} else if (b.u16(4) == 0xA5E0) {
		// Aseprite's canvas.
		w = b.u16(8), h = b.u16(10);
	} else if (isFarbfeld(b)) {
		w = b.be32(8), h = b.be32(12);
	} else if (b.startsWith("BLP2")) {
		w = b.u32(12), h = b.u32(16);
	} else if (b.startsWith("PVR\x03")) {
		h = b.u32(24), w = b.u32(28);
	}
	// Nothing real is anywhere near 16 million texels across.
	if (w <= 0 || h < 0 || faces < 0 || w > (1 << 24) || h > (1 << 24)) return {};
	if (h == 0) h = 1;  // One-dimensional textures.
	if (faces == 6) return {uint32_t(w * 4), uint32_t(w * 3)};
	return {uint32_t(w), uint32_t(h)};
}

}
