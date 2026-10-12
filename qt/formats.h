// The formats the Qt image plugin claims, and the cheap content checks that
// decide whether a file really is one of them.  No Qt here, so the checks
// can be tested on plain byte buffers.

#pragma once

#include <cstddef>
#include <cstdint>
#include <string_view>

namespace slerp {

/** How a format is shown: as one picture, or as a model turning around. */
enum class Shape { Still, Turntable };

/**
 * One Qt format key and the MIME type it stands for.  Qt pairs the `Keys`
 * and `MimeTypes` arrays of `slerp.json` by position, so a format with two
 * MIME types (Quake and Half-Life `.mdl`) has two entries with one key. */
struct Format {
	const char *key;
	const char *mime;
	Shape shape;
	/** The format's id in Slerp's format table, which the user's choices name. */
	const char *id;
};

/** Every format claimed, in the order of `slerp.json`. */
extern const Format kFormats[];
extern const size_t kFormatCount;

/** How many bytes from the start of a file the checks look at. */
constexpr size_t kSniffBytes = 4096;

/** The format with Qt key `key`, or null when it isn't one of ours. */
const Format *formatForKey(std::string_view key);

/**
 * Recognises a file from its first bytes, `head` (up to kSniffBytes), and
 * its size in bytes (-1 when the device can't tell).
 *
 * With `expected` null this is Qt's content detection, which only runs for
 * files nothing else recognised, so it claims only strong signatures: magic
 * numbers with plausible headers, or an XML root element.  Formats without
 * one (OBJ, glTF JSON) are only ever read by name.
 *
 * With `expected` set (the file's extension, or the caller named the
 * format), it checks that the content fits that format and returns it, or
 * null when the file is something else. */
const Format *sniff(const uint8_t *head, size_t len, int64_t size, const Format *expected);

/** A picture's size in pixels. */
struct Size {
	uint32_t width = 0, height = 0;
};

/**
 * The size a texture's picture comes out at, from its header: the largest
 * level, and for cubemaps the cross of six faces (four faces wide, three
 * high) Slerp lays them out in; or an Aseprite canvas, a farbfeld, BLP2 or
 * PVR3 picture's.  Zero when the header can't tell. */
Size textureSize(const uint8_t *head, size_t len);

}
