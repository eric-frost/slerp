// The Qt image plugin's side of Slerp's helper protocol (see
// crates/helper/src/protocol.rs): one request on a Unix socket, one reply,
// every step bounded by a deadline.

#pragma once

#include <QImage>
#include <QList>
#include <QString>

#include <chrono>
#include <cstdint>
#include <string>

namespace slerp {

/**
 * The protocol version of crates/helper's `PROTOCOL_VERSION`.  A test in
 * the `slerp` crate checks that the two agree, so a protocol change can't
 * leave the plugin behind unnoticed. */
constexpr uint8_t kProtocolVersion = 1;

using Clock = std::chrono::steady_clock;

/** What came back from the helper. */
struct Reply {
	enum Outcome {
		/** `frames` holds one or more pictures, all the same size. */
		Ok,
		/** The helper read the file and has nothing to show for it. */
		NoPicture,
		/** The helper couldn't be reached, started, or understood. */
		NoHelper,
		/** The deadline passed. */
		TimedOut,
		/** The helper tried and failed; `message` says why. */
		Failed,
	} outcome = NoHelper;
	QList<QImage> frames;
	QString message;
};

/**
 * Asks this user's helper (`$XDG_RUNTIME_DIR/slerp/socket`) for `frames`
 * pictures of the file at absolute path `path`, `size` pixels on their
 * longer side, waiting no later than `deadline`.  When no helper is
 * running it starts `slerp helper` and waits for it, since the plugin has
 * nothing to fall back on.  It never raises SIGPIPE and never blocks past
 * the deadline. */
Reply request(const std::string &path, uint32_t size, uint32_t frames, Clock::time_point deadline);

/** `$XDG_RUNTIME_DIR/slerp`, where the socket lives, or empty when there is no runtime directory. */
std::string runtimeDir();

}
