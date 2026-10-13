// Tests Slerp's Qt image plugin from the outside, as Qt programs use it.
//
//   slerp-qt-check selftest
//       The format table against slerp.json, the content checks on made-up
//       and damaged headers, and the protocol client against a fake helper
//       that misbehaves in every way it can.  No real helper needed.
//   slerp-qt-check read [--buffer] [--size N] [--save DIR] FILE...
//       Reads each file through QImageReader, from the file as Dolphin's
//       QMovie does or from memory (--buffer) as Gwenview does, and prints
//       what Qt sees and how long the first and all frames took.
//   slerp-qt-check movie [--ms N] [--size N] [--save DIR] FILE
//       Plays a file the way Dolphin's Information panel does, and reports
//       each frame and the longest time the event loop was held up.
//   slerp-qt-check bench [--count N] DIR...
//       Reads up to N (1000) PNG and JPEG files under DIR and prints the
//       time.  Run it with and without QT_PLUGIN_PATH pointing at the
//       plugin to see what having it installed costs.
//
// Point QT_PLUGIN_PATH at the build's plugins/ folder to load the plugin
// from there.  QT_LOGGING_RULES=slerp.qimage.debug=true shows why a read
// failed.

#include "choices.h"
#include "client.h"
#include "formats.h"

#include <QBuffer>
#include <QDir>
#include <QDirIterator>
#include <QElapsedTimer>
#include <QFile>
#include <QFileInfo>
#include <QGuiApplication>
#include <QImageReader>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QMimeDatabase>
#include <QMovie>
#include <QTimer>

#include <algorithm>
#include <atomic>
#include <cstdio>
#include <cstring>
#include <functional>
#include <mutex>
#include <thread>
#include <vector>

#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

namespace {

using namespace std::chrono_literals;

int g_failures = 0;

void check(bool ok, const QString &what) {
	if (!ok) g_failures++;
	printf("%s %s\n", ok ? "ok  " : "FAIL", qPrintable(what));
}

/** Dolphin's PixmapViewer::isAnimatedMimeType: a Qt image format for the MIME type that QMovie plays. */
bool dolphinAnimates(const QString &mime) {
	const QList<QByteArray> movies = QMovie::supportedFormats();
	for (const QByteArray &format : QImageReader::imageFormatsForMimeType(mime.toLatin1()))
		if (movies.contains(format)) return true;
	return false;
}

/*┌─────────────────╮
  | selftest        |
  └─────────────────╯*/
//region selftest

QByteArray bytes(std::initializer_list<int> values) {
	QByteArray out;
	for (int v : values) out.append(char(v));
	return out;
}

void le32(QByteArray &b, int at, uint32_t v) {
	// QByteArray::resize(size, fill) is Qt 6.4's; the packages build on 6.2.
	if (b.size() < at + 4) b.append(QByteArray(at + 4 - b.size(), '\0'));
	for (int i = 0; i < 4; i++) b[at + i] = char(v >> (8 * i));
}

const slerp::Format *sniffBytes(const QByteArray &b, int64_t size, const char *expected) {
	return slerp::sniff(reinterpret_cast<const uint8_t *>(b.constData()), size_t(b.size()), size,
		expected ? slerp::formatForKey(expected) : nullptr);
}

QString keyOf(const slerp::Format *f) { return f ? QString::fromLatin1(f->key) : QStringLiteral("none"); }

void testFormatTable() {
	QFile file(QStringLiteral(SLERP_PLUGIN_JSON));
	check(file.open(QIODevice::ReadOnly), "slerp.json opens");
	QJsonObject json = QJsonDocument::fromJson(file.readAll()).object();
	QJsonArray keys = json["Keys"].toArray(), mimes = json["MimeTypes"].toArray();
	bool same = keys.size() == qsizetype(slerp::kFormatCount) && mimes.size() == keys.size();
	for (size_t i = 0; same && i < slerp::kFormatCount; i++)
		same = keys[i].toString() == slerp::kFormats[i].key && mimes[i].toString() == slerp::kFormats[i].mime;
	check(same, "slerp.json lists exactly the formats in formats.cpp, in order");
	for (const char *taken : {"dds", "hdr", "exr", "tga", "psd", "png", "jpg", "jpeg", "gif", "webp", "svg"})
		check(!slerp::formatForKey(taken), QStringLiteral("doesn't claim %1").arg(taken));
	// Each MIME type must be one shared-mime-info or data/slerp.xml defines
	// under that very name, since Dolphin looks formats up by the file's
	// canonical type.  One only slerp.xml defines may not be installed yet.
	QMimeDatabase db;
	QFile xml(QStringLiteral(SLERP_MIME_XML));
	QByteArray ours = xml.open(QIODevice::ReadOnly) ? xml.readAll() : QByteArray();
	for (size_t i = 0; i < slerp::kFormatCount; i++) {
		QString mime = QString::fromLatin1(slerp::kFormats[i].mime);
		QMimeType type = db.mimeTypeForName(mime);
		bool installed = type.isValid() && type.name() == mime;
		bool defined = ours.contains("<mime-type type=\"" + mime.toLatin1() + "\"");
		check(installed || defined, QStringLiteral("%1 is a canonical MIME type%2").arg(mime,
			installed ? QString() : QStringLiteral(" (in data/slerp.xml, not installed yet)")));
	}
}

void testSniffing() {
	// 1. Textures and their sizes.
	QByteArray ktx2("\xABKTX 20\xBB\r\n\x1A\n", 12);
	le32(ktx2, 20, 300), le32(ktx2, 24, 200), le32(ktx2, 36, 1), le32(ktx2, 40, 1);
	check(keyOf(sniffBytes(ktx2, -1, nullptr)) == "ktx2", "KTX2 by content");
	auto size = slerp::textureSize(reinterpret_cast<const uint8_t *>(ktx2.constData()), size_t(ktx2.size()));
	check(size.width == 300 && size.height == 200, "KTX2 size");
	le32(ktx2, 24, 0);
	size = slerp::textureSize(reinterpret_cast<const uint8_t *>(ktx2.constData()), size_t(ktx2.size()));
	check(size.width == 300 && size.height == 1, "1D KTX2 is one pixel high");
	le32(ktx2, 24, 64), le32(ktx2, 20, 64), le32(ktx2, 36, 6);
	size = slerp::textureSize(reinterpret_cast<const uint8_t *>(ktx2.constData()), size_t(ktx2.size()));
	check(size.width == 256 && size.height == 192, "a KTX2 cubemap is a cross");
	le32(ktx2, 20, 0xFFFFFFFF);
	size = slerp::textureSize(reinterpret_cast<const uint8_t *>(ktx2.constData()), size_t(ktx2.size()));
	check(size.width == 0, "absurd sizes are refused");
	QByteArray ktx1("\xABKTX 11\xBB\r\n\x1A\n", 12);
	le32(ktx1, 12, 0x04030201), le32(ktx1, 36, 128), le32(ktx1, 40, 32), le32(ktx1, 52, 1);
	size = slerp::textureSize(reinterpret_cast<const uint8_t *>(ktx1.constData()), size_t(ktx1.size()));
	check(keyOf(sniffBytes(ktx1, -1, nullptr)) == "ktx" && size.width == 128 && size.height == 32, "KTX1");
	QByteArray swapped = ktx1;
	for (int at : {12, 36, 40, 52}) std::reverse(swapped.begin() + at, swapped.begin() + at + 4);
	size = slerp::textureSize(reinterpret_cast<const uint8_t *>(swapped.constData()), size_t(swapped.size()));
	check(size.width == 128 && size.height == 32, "big-endian KTX1");
	QByteArray astc = bytes({0x13, 0xAB, 0xA1, 0x5C, 6, 6, 1, 0x00, 0x04, 0, 0x80, 0, 0, 1, 0, 0});
	size = slerp::textureSize(reinterpret_cast<const uint8_t *>(astc.constData()), size_t(astc.size()));
	check(keyOf(sniffBytes(astc, -1, nullptr)) == "astc" && size.width == 1024 && size.height == 128, "ASTC");
	astc[4] = 3;
	check(!sniffBytes(astc, -1, nullptr), "ASTC with an impossible block size is refused");
	QByteArray png = QByteArray("\x89PNG\r\n\x1A\n\0\0\0\x0DIHDR\0\0\x04\0\0\0\x02\0\x10\0", 26);
	size = slerp::textureSize(reinterpret_cast<const uint8_t *>(png.constData()), size_t(png.size()));
	check(keyOf(sniffBytes(png, 1000, "heightmap")) == "heightmap" && size.width == 1024 && size.height == 512,
		"a heightmap PNG by name, with its size");
	check(!sniffBytes(png, 1000, nullptr), "PNGs are never claimed by content");

	// 2. Models by content.
	QByteArray glb("glTF", 4);
	le32(glb, 4, 2), le32(glb, 8, 1000);
	check(keyOf(sniffBytes(glb, 1000, nullptr)) == "glb", "GLB by content");
	check(!sniffBytes(glb, 999, nullptr), "GLB longer than its file is refused");
	check(keyOf(sniffBytes(glb, 1000, "vrm")) == "vrm", "VRM by name");
	le32(glb, 4, 1);
	check(!sniffBytes(glb, 1000, nullptr), "glTF 1.0 binary is refused");
	QByteArray bsp(124, '\0');
	le32(bsp, 0, 29);
	for (int i = 0; i < 15; i++) le32(bsp, 4 + 8 * i, 124 + 10 * i), le32(bsp, 8 + 8 * i, 10);
	check(keyOf(sniffBytes(bsp, 300, nullptr)) == "bsp", "BSP by content");
	check(!sniffBytes(bsp, 200, nullptr), "BSP lumps past the end are refused");
	check(!sniffBytes(bsp, -1, nullptr), "BSP isn't claimed by content without a size");
	check(!sniffBytes(bsp.left(64), 300, "bsp"), "a truncated BSP header is refused");
	le32(bsp, 4, 50);
	check(!sniffBytes(bsp, 300, nullptr), "BSP lumps inside the header are refused");
	QByteArray stl(84, ' ');
	le32(stl, 80, 2);
	check(keyOf(sniffBytes(stl, 184, nullptr)) == "stl", "binary STL whose count fits the size");
	check(!sniffBytes(stl, 185, nullptr), "binary STL whose count doesn't fit");
	check(keyOf(sniffBytes("solid cube\n  facet normal 0 0 1\n", -1, nullptr)) == "stl", "ASCII STL");
	check(keyOf(sniffBytes("ply\nformat ascii 1.0\nelement vertex 3\n", -1, nullptr)) == "ply", "PLY");
	check(!sniffBytes("ply\nformat binary_little_endian 1.0\nproperty float f_dc_0\n", -1, "ply"),
		"Gaussian splat PLYs aren't models");
	check(keyOf(sniffBytes("<?xml version=\"1.0\"?>\n<COLLADA xmlns=\"x\">", -1, nullptr)) == "dae", "Collada");
	check(keyOf(sniffBytes("<?xml version=\"1.0\"?>\n<materialx version=\"1.38\">", -1, nullptr)) == "mtlx",
		"MaterialX");
	check(keyOf(sniffBytes("[gd_resource type=\"StandardMaterial3D\" format=3]\n", -1, nullptr)) == "tres",
		"Godot material");
	check(!sniffBytes("[gd_resource type=\"Theme\" format=3]\n", -1, "tres"), "other Godot resources are refused");
	QByteArray usdz("PK\x03\x04", 4);
	usdz.append(QByteArray(30 - usdz.size(), '\0'));
	usdz[26] = 10;
	usdz.append("scene.usdc");
	check(keyOf(sniffBytes(usdz, -1, nullptr)) == "usdz", "USDZ whose first file is a layer");
	usdz.replace("scene.usdc", "scene.text");
	check(!sniffBytes(usdz, -1, nullptr), "other zips are refused");
	check(keyOf(sniffBytes(QByteArray("IDPO\x06\0\0\0", 8), -1, nullptr)) == "mdl", "Quake MDL");
	check(!sniffBytes(QByteArray("IDST\x2c\0\0\0", 8), -1, "mdl"), "Source engine MDLs are refused");
	check(keyOf(sniffBytes(QByteArray("DNA\0\x02\0\x15", 7), -1, nullptr)) == "dna", "MetaHuman DNA");
	check(!sniffBytes(QByteArray("DNA\0\x02\0\0", 7), -1, "dna"), "DNA of version 0 is refused");

	// 3. Text formats only by name; never by content.
	check(!sniffBytes("v 0 0 0\nv 1 0 0\nf 1 2 3\n", -1, nullptr), "OBJ isn't claimed by content");
	check(keyOf(sniffBytes("v 0 0 0\nv 1 0 0\nf 1 2 3\n", -1, "obj")) == "obj", "OBJ by name");
	check(!sniffBytes(bytes({0x64, 0x86, 0, 0, 0, 0}), -1, "obj"), "a COFF object named .obj is refused");
	check(!sniffBytes("{\"asset\":{\"version\":\"2.0\"}}", -1, nullptr), "glTF JSON isn't claimed by content");
	check(keyOf(sniffBytes(" {\"asset\":{\"version\":\"2.0\"}}", -1, "gltf")) == "gltf", "glTF JSON by name");

	// 4. Ordinary images are never claimed, and nothing reads past what it is given.
	check(!sniffBytes(QByteArray("\x89PNG\r\n\x1A\n\0\0\0\rIHDR", 16), 1000, nullptr), "PNG isn't claimed");
	check(!sniffBytes(QByteArray("\xFF\xD8\xFF\xE0", 4), 1000, nullptr), "JPEG isn't claimed");
	check(!sniffBytes("DDS \x7C", 1000, nullptr), "DDS isn't claimed");
	check(!sniffBytes("#?RADIANCE\n", 1000, nullptr), "Radiance HDR isn't claimed");
	check(!sniffBytes(QByteArray(), 0, nullptr), "empty input");
	int truncated = 0;
	for (const QByteArray &whole : {ktx2, ktx1, astc, glb, bsp, stl, usdz})
		for (int n = 0; n < whole.size(); n++) {
			QByteArray part = whole.left(n);
			for (size_t i = 0; i < slerp::kFormatCount; i++) sniffBytes(part, n, slerp::kFormats[i].key);
			sniffBytes(part, n, nullptr);
			slerp::textureSize(reinterpret_cast<const uint8_t *>(part.constData()), size_t(part.size()));
			truncated++;
		}
	check(truncated > 0, QStringLiteral("%1 truncated headers checked without reading past them").arg(truncated));
}

/** A header each format accepts by name, with the file size it claims. */
struct Sample {
	const char *key;
	QByteArray head;
	int64_t size;
	/** The key content detection alone gives, or null when only the name may claim it. */
	const char *detected;
};

QByteArray zeros(int n) { return QByteArray(n, '\0'); }

QByteArray aseprite() {
	QByteArray a = zeros(128);
	le32(a, 0, 1000);
	a[4] = char(0xE0), a[5] = char(0xA5), a[8] = 32, a[10] = 16;
	return a;
}

QByteArray max3ds() {
	QByteArray a = bytes({0x4D, 0x4D});
	le32(a, 2, 500);
	return a;
}

QByteArray degas() {
	QByteArray a = zeros(34);
	a[1] = 1;
	return a;
}

/** Every key of the table with a header it must accept, so no format is left without a check. */
std::vector<Sample> samples() {
	return {
		{"aseprite", aseprite(), 1000, "aseprite"},
		{"ase", aseprite(), 1000, "aseprite"},
		{"tmx", "<?xml version=\"1.0\"?>\n<map version=\"1.10\" tiledversion=\"1.10\">", -1, nullptr},
		{"tmj", "{ \"tiledversion\": \"1.10\", \"tilesets\": [] }", -1, nullptr},
		{"ldtk", "{ \"__header__\": { \"app\": \"LDtk\" } }", -1, nullptr},
		{"ldtkl", "{ \"__header__\": { \"doc\": \"LDtk Level\" } }", -1, nullptr},
		{"3mf", QByteArray("PK\x03\x04", 4) + zeros(26), 2000, nullptr},
		{"off", "OFF\n8 6 0\n", -1, nullptr},
		{"3ds", max3ds(), 500, nullptr},
		{"x", "xof 0303txt 0032\n", -1, "x"},
		{"wrl", "#VRML V2.0 utf8\n", -1, "wrl"},
		{"vrml", "#VRML V2.0 utf8\n", -1, "wrl"},
		{"wrz", bytes({0x1F, 0x8B, 8, 0}), -1, nullptr},
		{"x3d", "<?xml version=\"1.0\"?>\n<X3D profile=\"Immersive\">", -1, nullptr},
		{"x3dz", bytes({0x1F, 0x8B, 8, 0}), -1, nullptr},
		{"x3dv", "#X3D V3.3 utf8\n", -1, "x3dv"},
		{"x3dvz", bytes({0x1F, 0x8B, 8, 0}), -1, nullptr},
		{"las", "LASF\0\0", -1, "las"},
		{"laz", "LASF\0\0", -1, "las"},
		{"e57", "ASTM-E57", -1, "e57"},
		{"pcd", "# .PCD v0.7 - Point Cloud Data\nVERSION 0.7\nFIELDS x y z\n", -1, "pcd"},
		{"pts", "3\n0 0 0\n1 0 0\n0 1 0\n", -1, nullptr},
		{"ptx", "10\n10\n0 0 0\n", -1, nullptr},
		{"bbmodel", "{\"meta\":{\"format_version\":\"4.10\"}}", -1, nullptr},
		{"tscn", "[gd_scene load_steps=2 format=3]\n", -1, "tscn"},
		{"escn", "[gd_scene load_steps=2 format=2]\n", -1, "tscn"},
		{"unity", "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n", -1, "unity"},
		{"prefab", "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n", -1, "unity"},
		{"blend", "BLENDER-v300RENDH", -1, "blend"},
		{"map", "// Game: Quake\n{\n\"classname\" \"worldspawn\"\n", -1, nullptr},
		{"vmf", "versioninfo\n{\n\t\"editorversion\" \"400\"\n", -1, nullptr},
		{"xnb", "XNBw\x05\x01", -1, "xnb"},
		{"blp", "BLP2\x01\0\0\0", -1, "blp"},
		{"pvr", QByteArray("PVR\x03", 4) + zeros(48), -1, "pvr"},
		{"pkm", "PKM 10\0\0", -1, "pkm"},
		{"spr", "IDSP\x01\0\0\0", -1, "spr"},
		{"sp2", "IDS2\x02\0\0\0", -1, "sp2"},
		{"fits", "SIMPLE  =                    T", -1, "fits"},
		{"fit", "SIMPLE  =                    T", -1, "fits"},
		{"fts", "SIMPLE  =                    T", -1, "fits"},
		{"xisf", "XISF0100", -1, "xisf"},
		{"dpx", "SDPX", -1, "dpx"},
		{"cin", bytes({0x80, 0x2A, 0x5F, 0xD7}), -1, "cin"},
		{"cineon", bytes({0xD7, 0x5F, 0x2A, 0x80}), -1, "cin"},
		{"pam", "P7\nWIDTH 2\n", -1, "pam"},
		{"ff", "farbfeld\0\0\0\x02", -1, "ff"},
		{"pi1", degas(), 32034, nullptr},
		{"pi2", degas(), 32034, nullptr},
		{"pi3", degas(), 32066, nullptr},
		{"pc1", degas(), 9000, nullptr},
		{"pc2", degas(), 9000, nullptr},
		{"pc3", degas(), 9000, nullptr},
		{"neo", zeros(8), 32128, nullptr},
		{"tn1", bytes({1, 0}), 9000, nullptr},
		{"tn2", bytes({1, 0}), 9000, nullptr},
		{"tn3", bytes({1, 0}), 9000, nullptr},
		{"tny", bytes({1, 0}), 9000, nullptr},
		{"koa", bytes({0, 0x60}), 10003, nullptr},
		{"kla", bytes({0, 0x60}), 10003, nullptr},
		{"art", bytes({0, 0x20}), 9009, nullptr},
		{"aas", bytes({0, 0x20}), 9009, nullptr},
		{"dd", bytes({0, 0x5C}), 9218, nullptr},
		{"jj", bytes({0, 0x5C}), 9218, nullptr},
		{"hir", bytes({0, 0x20}), 8002, nullptr},
		{"hpi", bytes({0, 0x20}), 8002, nullptr},
		{"ocp", bytes({0, 0x20}), 10018, nullptr},
		{"sc2", bytes({0xFE, 0, 0}), 16391, nullptr},
		{"sc5", bytes({0xFE, 0, 0}), 30375, nullptr},
		{"sc6", bytes({0xFE, 0, 0}), 30375, nullptr},
		{"sc7", bytes({0xFE, 0, 0}), 54279, nullptr},
		{"sc8", bytes({0xFE, 0, 0}), 54279, nullptr},
		{"sc12", bytes({0xFE, 0, 0}), 54279, nullptr},
		{"sr5", bytes({0xFE, 0, 0}), 30375, nullptr},
		{"sr6", bytes({0xFE, 0, 0}), 30375, nullptr},
		{"sr7", bytes({0xFE, 0, 0}), 54279, nullptr},
		{"sr8", bytes({0xFE, 0, 0}), 54279, nullptr},
		{"sr12", bytes({0xFE, 0, 0}), 54279, nullptr},
		{"s12", bytes({0xFE, 0, 0}), 54279, nullptr},
		{"srs", bytes({0xFE, 0, 0}), 54279, nullptr},
		{"hgr", zeros(4), 8192, nullptr},
		{"dhr", zeros(4), 16384, nullptr},
		{"mic", zeros(4), 7680, nullptr},
		{"g15", zeros(4), 7680, nullptr},
		{"gr15", zeros(4), 7680, nullptr},
		{"gr9", zeros(4), 7680, nullptr},
		{"gr8", zeros(4), 7680, nullptr},
		{"bbc", zeros(4), 20480, nullptr},
		{"ans", "\x1B[0;1;34mHello", 16, nullptr},
		{"p8", "pico-8 cartridge // http://www.pico-8.com\nversion 41\n", -1, "p8"},
		{"tic", bytes({0x05, 0x10, 0, 0}), 4000, nullptr},
	};
}

void testNewFormats() {
	std::vector<Sample> list = samples();
	// 1. Every key of the table has a check, and a sample it accepts.
	for (size_t i = 0; i < slerp::kFormatCount; i++) {
		const char *k = slerp::kFormats[i].key;
		bool old = QByteArrayList{"heightmap", "ktx2", "ktx", "astc", "glb", "gltf", "vrm", "obj", "stl", "fbx", "dae",
			"ply", "usda", "usdc", "usd", "usdz", "vox", "bsp", "mdl", "md2", "md3", "mtlx", "tres", "dna"}.contains(k);
		if (old) continue;
		auto it = std::find_if(list.begin(), list.end(), [&](const Sample &s) { return strcmp(s.key, k) == 0; });
		if (it == list.end()) {
			check(false, QStringLiteral("a sample for %1").arg(k));
			continue;
		}
		check(keyOf(sniffBytes(it->head, it->size, k)) == k, QStringLiteral("%1 by name").arg(k));
		const slerp::Format *found = sniffBytes(it->head, it->size, nullptr);
		check(keyOf(found) == (it->detected ? it->detected : "none"),
			QStringLiteral("%1 by content alone: %2").arg(k, keyOf(found)));
	}
	// 2. A name alone isn't enough where there is something to check.
	for (const char *k : {"aseprite", "x", "las", "e57", "xnb", "blp", "pvr", "pkm", "spr", "fits", "xisf", "dpx",
			 "cin", "pam", "ff", "p8", "tscn", "unity", "map", "vmf", "neo", "pi1"})
		check(!sniffBytes("just some text\n", 15, k), QStringLiteral("%1 refuses other content").arg(k));
	check(!sniffBytes(zeros(40), 40000, "pi1"), "a DEGAS name with the wrong size is refused");
	check(keyOf(sniffBytes(QByteArray("\x01\0\0\0", 4) + zeros(12), 1 << 20, "art")) == "art", "Build tiles by name");
	// 3. Truncated headers read nothing past their end.
	int n = 0;
	for (const Sample &s : list)
		for (int cut = 0; cut < s.head.size(); cut++) {
			sniffBytes(s.head.left(cut), cut, s.key);
			sniffBytes(s.head.left(cut), cut, nullptr);
			n++;
		}
	check(n > 0, QStringLiteral("%1 truncated new-format headers checked").arg(n));
}

/** The user's choices of formats, as `slerp associate` writes them. */
void testChoices() {
	QString dir = QStringLiteral("/tmp/slerp-qt-choices-%1").arg(getpid());
	QDir().mkpath(dir + "/slerp");
	QByteArray old = qgetenv("XDG_CONFIG_HOME");
	qputenv("XDG_CONFIG_HOME", dir.toLocal8Bit());
	check(!slerp::switchedOff("glb"), "no choices file: everything on");
	QFile file(dir + "/slerp/file-types.jsonc");
	auto rewrite = [&](const QByteArray &text) {
		check(file.open(QIODevice::WriteOnly), "the choices file opens");
		file.write(text);
		file.close();
	};
	rewrite("// Which file types Slerp makes thumbnails for, \"quoted\"\n{\n  \"thumbnails\": { \"glb\": false, \"gif\": true },\n"
		"  /* block */ \"openByDefault\": {}\n}\n");
	check(slerp::switchedOff("glb") && !slerp::switchedOff("gif") && !slerp::switchedOff("ktx2"), "glb switched off");
	// Rewritten: read again (a different size, so even a coarse clock sees it).
	rewrite("{ \"thumbnails\": { \"ktx2\": false } }\n");
	check(!slerp::switchedOff("glb") && slerp::switchedOff("ktx2"), "a changed file is read again");
	rewrite("{ not json");
	check(!slerp::switchedOff("ktx2"), "a broken file means the defaults");
	QDir(dir).removeRecursively();
	if (old.isEmpty()) qunsetenv("XDG_CONFIG_HOME"); else qputenv("XDG_CONFIG_HOME", old);
	// Every key carries its format id.
	bool ids = true;
	for (size_t i = 0; i < slerp::kFormatCount; i++) ids = ids && slerp::kFormats[i].id && *slerp::kFormats[i].id;
	check(ids, "every key names its format");
}

/** A helper stand-in that answers each connection with `answer`, for the client tests. */
class FakeHelper {
public:
	using Answer = std::function<void(int fd)>;

	explicit FakeHelper(const QString &runtime) {
		QDir().mkpath(runtime + "/slerp");
		m_path = (runtime + "/slerp/socket").toStdString();
		unlink(m_path.c_str());
		m_listen = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
		sockaddr_un addr{};
		addr.sun_family = AF_UNIX;
		strncpy(addr.sun_path, m_path.c_str(), sizeof addr.sun_path - 1);
		if (bind(m_listen, reinterpret_cast<sockaddr *>(&addr), sizeof addr) != 0 || listen(m_listen, 8) != 0)
			perror("fake helper");
		m_thread = std::thread([this] { serve(); });
	}

	~FakeHelper() {
		m_stop = true;
		shutdown(m_listen, SHUT_RDWR);
		close(m_listen);
		m_thread.join();
		unlink(m_path.c_str());
	}

	void answer(Answer a) {
		std::lock_guard lock(m_mutex);
		m_answer = std::move(a);
	}

private:
	void serve() {
		while (!m_stop) {
			int fd = accept4(m_listen, nullptr, nullptr, SOCK_CLOEXEC);
			if (fd < 0) return;
			// Read the request header and payload first, as a helper does.
			char head[8];
			if (recv(fd, head, 8, MSG_WAITALL) == 8) {
				uint32_t len = uint8_t(head[4]) | uint8_t(head[5]) << 8 | uint8_t(head[6]) << 16 | uint32_t(uint8_t(head[7])) << 24;
				std::vector<char> payload(len);
				recv(fd, payload.data(), len, MSG_WAITALL);
			}
			Answer a;
			{
				std::lock_guard lock(m_mutex);
				a = m_answer;
			}
			if (a) a(fd);
			close(fd);
		}
	}

	std::string m_path;
	int m_listen = -1;
	std::thread m_thread;
	std::mutex m_mutex;
	Answer m_answer;
	std::atomic<bool> m_stop{false};
};

void sendBytes(int fd, const QByteArray &b) { send(fd, b.constData(), size_t(b.size()), MSG_NOSIGNAL); }

QByteArray header(uint8_t kind, uint32_t len, uint8_t version = slerp::kProtocolVersion) {
	QByteArray h("PT", 2);
	h.append(char(version)).append(char(kind));
	le32(h, 4, len);
	return h;
}

QByteArray framesReply(uint32_t w, uint32_t h, uint32_t count, uint32_t claimedLen = 0) {
	QByteArray body(12, '\0');
	le32(body, 0, w), le32(body, 4, h), le32(body, 8, count);
	QByteArray pixels(int(w * h * 4 * count), '\0');
	for (int i = 0; i < pixels.size(); i++) pixels[i] = char(i * 7);
	return header(0x42, claimedLen ? claimedLen : uint32_t(12 + pixels.size())) + body + pixels;
}

void testClient() {
	QString runtime = QStringLiteral("/tmp/slerp-qt-check-%1").arg(getpid());
	QDir().mkpath(runtime);
	chmod(runtime.toLocal8Bit().constData(), 0700);
	qputenv("XDG_RUNTIME_DIR", runtime.toLocal8Bit());
	// The helper a failed connect would start: one that exits at once.
	qputenv("SLERP_EXECUTABLE", "/bin/false");
	std::string file = "/tmp/model.glb";
	auto ask = [&](std::chrono::milliseconds wait, int *ms) {
		QElapsedTimer t;
		t.start();
		slerp::Reply r = slerp::request(file, 64, 2, slerp::Clock::now() + wait);
		*ms = int(t.elapsed());
		return r;
	};
	int ms = 0;
	{
		FakeHelper helper(runtime);
		helper.answer([](int fd) { sendBytes(fd, framesReply(2, 3, 2)); });
		slerp::Reply r = ask(2s, &ms);
		bool pixels = r.frames.size() == 2 && r.frames[1].size() == QSize(2, 3) &&
			uint8_t(r.frames[1].constBits()[0]) == uint8_t(24 * 7) &&
			r.frames[0].format() == QImage::Format_RGBA8888;
		check(r.outcome == slerp::Reply::Ok && pixels, "a good frames reply arrives intact");

		QImage png(4, 4, QImage::Format_RGBA8888);
		png.fill(Qt::red);
		QByteArray encoded;
		QBuffer buf(&encoded);
		png.save(&buf, "PNG");
		helper.answer([&](int fd) { sendBytes(fd, header(0x41, uint32_t(encoded.size())) + encoded); });
		r = ask(2s, &ms);
		check(r.outcome == slerp::Reply::Ok && r.frames.size() == 1 && r.frames[0].pixelColor(1, 1) == Qt::red,
			"an older helper's PNG reply becomes one frame");

		struct Case {
			const char *what;
			FakeHelper::Answer answer;
			slerp::Reply::Outcome expect;
		};
		const Case cases[] = {
			{"a helper that hangs up at once", [](int) {}, slerp::Reply::NoHelper},
			{"garbage", [](int fd) { sendBytes(fd, "HTTP/1.1 400 Bad Request\r\n\r\n"); }, slerp::Reply::Failed},
			{"another protocol version", [](int fd) { sendBytes(fd, header(0x47, 0, 9)); }, slerp::Reply::NoHelper},
			{"no picture", [](int fd) { sendBytes(fd, header(0x43, 0)); }, slerp::Reply::NoPicture},
			{"a failure message", [](int fd) { sendBytes(fd, header(0x44, 4) + "oops"); }, slerp::Reply::Failed},
			{"a too long failure message", [](int fd) { sendBytes(fd, header(0x44, 1 << 20)); }, slerp::Reply::Failed},
			{"frames of absurd size", [](int fd) { sendBytes(fd, framesReply(1, 1, 1).replace(8, 4, QByteArray("\xFF\xFF\0\0", 4))); },
				slerp::Reply::Failed},
			{"frames whose length lies", [](int fd) { sendBytes(fd, framesReply(2, 2, 1, 1000)); }, slerp::Reply::Failed},
			{"frames cut off", [](int fd) { sendBytes(fd, framesReply(64, 64, 2).left(5000)); }, slerp::Reply::NoHelper},
			{"a PNG that isn't one", [](int fd) { sendBytes(fd, header(0x41, 4) + "abcd"); }, slerp::Reply::Failed},
			{"a busy helper", [](int fd) { sendBytes(fd, header(0x46, 0)); }, slerp::Reply::NoHelper},
			{"an expired job", [](int fd) { sendBytes(fd, header(0x45, 0)); }, slerp::Reply::TimedOut},
		};
		for (const Case &c : cases) {
			helper.answer(c.answer);
			r = ask(2s, &ms);
			check(r.outcome == c.expect && r.frames.isEmpty() && ms < 500,
				QStringLiteral("%1: outcome %2 in %3 ms").arg(c.what).arg(r.outcome).arg(ms));
		}
		helper.answer([](int fd) {
			std::this_thread::sleep_for(1500ms);
			sendBytes(fd, framesReply(1, 1, 1));
		});
		r = ask(300ms, &ms);
		check(r.outcome == slerp::Reply::TimedOut && ms >= 290 && ms < 450,
			QStringLiteral("a helper too slow for the deadline: given up after %1 ms").arg(ms));
	}
	// No helper listening: it starts one (here /bin/false, which exits) and
	// gives up quickly rather than waiting out the deadline.
	slerp::Reply r = ask(3s, &ms);
	check(r.outcome == slerp::Reply::NoHelper && ms < 800,
		QStringLiteral("a helper that exits as it starts: given up after %1 ms (%2)").arg(ms).arg(r.message));
	r = ask(3s, &ms);
	check(r.outcome == slerp::Reply::NoHelper && ms < 20,
		QStringLiteral("and the next request doesn't try again at once: %1 ms").arg(ms));
	qputenv("XDG_RUNTIME_DIR", "/nonexistent/runtime");
	r = ask(3s, &ms);
	check(r.outcome == slerp::Reply::NoHelper && ms < 20, "no runtime directory fails at once");
	QDir(runtime).removeRecursively();
}

int selftest() {
	testFormatTable();
	testSniffing();
	testNewFormats();
	testChoices();
	testClient();
	printf("%s: %d failed\n", g_failures ? "FAILED" : "passed", g_failures);
	return g_failures ? 1 : 0;
}

//endregion

/*┌─────────────────╮
  | read, movie     |
  └─────────────────╯*/
//region read, movie

struct Options {
	QString save;
	int size = 0;
	int ms = 4000;
	int count = 1000;
	bool buffer = false;
	QStringList files;
};

void saveFrame(const Options &o, const QString &file, int frame, const QImage &image) {
	if (o.save.isEmpty()) return;
	QDir().mkpath(o.save);
	image.save(QStringLiteral("%1/%2-f%3.png").arg(o.save, QFileInfo(file).fileName()).arg(frame, 2, 10, QChar('0')));
}

int readFilesHere(const Options &o);

/**
 * Reads every file on a worker thread, as a program reading images in the
 * background does; a GUI thread would be handed a model's first picture
 * again while its turntable is fetched (see `movie`). */
int readFiles(const Options &o) {
	int result = 0;
	std::thread worker([&] { result = readFilesHere(o); });
	worker.join();
	return result;
}

int readFilesHere(const Options &o) {
	QMimeDatabase db;
	int failed = 0;
	for (const QString &file : o.files) {
		QFile f(file);
		QByteArray data;
		QBuffer buffer(&data);
		QImageReader reader;
		if (o.buffer) {
			if (!f.open(QIODevice::ReadOnly)) {
				printf("FAIL %s: can't open\n", qPrintable(file));
				failed++;
				continue;
			}
			data = f.readAll();
			buffer.open(QIODevice::ReadOnly);
			// Gwenview hints the format with the extension.
			reader.setDevice(&buffer);
			reader.setFormat(QFileInfo(file).suffix().toLower().toLatin1());
		} else {
			reader.setFileName(file);
		}
		if (o.size > 0) reader.setScaledSize(QSize(o.size, o.size));
		QString mime = db.mimeTypeForFile(file).name();
		QElapsedTimer t;
		t.start();
		bool can = reader.canRead();
		QByteArray format = reader.format();
		QSize size = reader.size();
		bool animated = reader.supportsAnimation();
		QImage first = reader.read();
		qint64 firstMs = t.elapsed();
		int frames = first.isNull() ? 0 : 1;
		saveFrame(o, file, 0, first);
		while (!first.isNull() && reader.canRead()) {
			QImage next = reader.read();
			if (next.isNull()) break;
			if (frames % 9 == 0) saveFrame(o, file, frames, next);
			frames++;
		}
		qint64 allMs = t.elapsed();
		bool ok = !first.isNull();
		failed += !ok;
		printf("%s %-26s fmt=%-5s canRead=%d size=%dx%d anim=%d count=%d dolphinAnimates=%d frames=%d %dx%d "
			   "first=%lldms all=%lldms %s\n",
			ok ? "OK  " : "FAIL", qPrintable(mime), format.constData(), can, size.width(), size.height(), animated,
			reader.imageCount(), dolphinAnimates(mime), frames, first.width(), first.height(), firstMs, allMs,
			qPrintable(file));
		if (!ok) printf("     error: %s\n", qPrintable(reader.errorString()));
	}
	return failed ? 1 : 0;
}

int playMovie(const Options &o) {
	if (o.files.size() != 1) return fprintf(stderr, "movie takes one file\n"), 2;
	const QString file = o.files[0];
	QString mime = QMimeDatabase().mimeTypeForFile(file).name();
	printf("mime %s, Dolphin animates it: %s\n", qPrintable(mime), dolphinAnimates(mime) ? "yes" : "no");
	QElapsedTimer clock;
	clock.start();
	// As PixmapViewer::setAnimatedImageFileName does.
	QMovie movie;
	movie.setFileName(file);
	if (o.size > 0) movie.setScaledSize(QSize(o.size, o.size));
	bool valid = movie.isValid();
	int count = movie.frameCount();
	printf("%6lld ms  valid=%d frameCount=%d\n", clock.elapsed(), valid, count);
	if (!valid || count <= 1) return 1;
	int shown = 0;
	QObject::connect(&movie, &QMovie::frameChanged, [&](int n) {
		if (shown < 3 || n % 9 == 0) printf("%6lld ms  frame %d\n", clock.elapsed(), n);
		if (shown < 40 && n % 9 == 0) saveFrame(o, file, n, movie.currentImage());
		shown++;
	});
	QObject::connect(&movie, &QMovie::stateChanged,
		[&](QMovie::MovieState s) { printf("%6lld ms  state %d\n", clock.elapsed(), int(s)); });
	QObject::connect(&movie, &QMovie::error,
		[&](QImageReader::ImageReaderError e) { printf("%6lld ms  error %d\n", clock.elapsed(), int(e)); });
	// The longest the event loop went without running a 10 ms timer is
	// how long the GUI of a program like Dolphin would have been frozen.
	qint64 last = clock.elapsed(), longest = 0;
	QTimer tick;
	QObject::connect(&tick, &QTimer::timeout, [&] {
		qint64 now = clock.elapsed();
		longest = qMax(longest, now - last);
		last = now;
	});
	tick.start(10);
	qint64 before = clock.elapsed();
	movie.start();
	qint64 startMs = clock.elapsed() - before;
	QTimer::singleShot(o.ms, qApp, &QCoreApplication::quit);
	qApp->exec();
	printf("start() took %lld ms; %d frames shown in %d ms; longest event-loop stall %lld ms; final state %d\n",
		startMs, shown, o.ms, qMax(longest, startMs), int(movie.state()));
	return shown > 0 ? 0 : 1;
}

//endregion

/*┌─────────────────╮
  | bench           |
  └─────────────────╯*/
//region bench

int bench(const Options &o) {
	QStringList files;
	for (const QString &dir : o.files) {
		QDirIterator it(dir, {"*.png", "*.jpg", "*.jpeg", "*.PNG", "*.JPG"}, QDir::Files, QDirIterator::Subdirectories);
		while (it.hasNext() && files.size() < o.count) files << it.next();
	}
	std::sort(files.begin(), files.end());
	// Read every file once so the page cache holds them all, then time
	// three rounds of QImageReader.
	qint64 bytes = 0;
	for (const QString &f : files) {
		QFile file(f);
		if (file.open(QIODevice::ReadOnly)) bytes += file.readAll().size();
	}
	QElapsedTimer t;
	t.start();
	QImage firstImage = QImageReader(files.value(0)).read();
	qint64 firstUs = t.nsecsElapsed() / 1000;
	printf("%lld files, %.1f MB; first read (loads the image plugins) %lld us\n", qint64(files.size()), bytes / 1e6,
		firstUs);
	for (int round = 0; round < 3; round++) {
		int ok = 0;
		qint64 canReadNs = 0;
		t.restart();
		for (const QString &f : files) {
			QElapsedTimer c;
			c.start();
			QImageReader reader(f);
			bool can = reader.canRead();
			canReadNs += c.nsecsElapsed();
			ok += can && !reader.read().isNull();
		}
		qint64 ns = t.nsecsElapsed();
		printf("round %d: %d/%lld read in %.1f ms, %.1f us per image, of which opening and canRead %.1f us\n", round + 1,
			ok, qint64(files.size()), ns / 1e6, ns / 1e3 / qMax<qsizetype>(1, files.size()),
			canReadNs / 1e3 / qMax<qsizetype>(1, files.size()));
	}
	return 0;
}

//endregion

}

int main(int argc, char **argv) {
	QGuiApplication app(argc, argv);
	QStringList args = app.arguments().mid(1);
	if (args.isEmpty()) return fprintf(stderr, "usage: slerp-qt-check selftest | read | movie | bench ...\n"), 2;
	QString command = args.takeFirst();
	Options o;
	while (!args.isEmpty()) {
		QString a = args.takeFirst();
		if (a == "--save" && !args.isEmpty()) o.save = args.takeFirst();
		else if (a == "--size" && !args.isEmpty()) o.size = args.takeFirst().toInt();
		else if (a == "--ms" && !args.isEmpty()) o.ms = args.takeFirst().toInt();
		else if (a == "--count" && !args.isEmpty()) o.count = args.takeFirst().toInt();
		else if (a == "--buffer") o.buffer = true;
		else o.files << a;
	}
	if (command == "selftest") return selftest();
	if (command == "read") return readFiles(o);
	if (command == "movie") return playMovie(o);
	if (command == "bench") return bench(o);
	return fprintf(stderr, "unknown command %s\n", qPrintable(command)), 2;
}
