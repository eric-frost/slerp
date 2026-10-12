// The plugin's client for Slerp's resident helper.  It is a C++ copy of the
// client half of crates/helper, written against the documented wire format
// rather than linked from Rust, so the plugin that every Qt program loads
// brings no second language runtime, allocator or panic handler with it.
//
// A plugin lives inside someone else's process, which shapes everything
// here: sockets are non-blocking and every wait is a poll() bounded by the
// caller's deadline; sends use MSG_NOSIGNAL, because a host that doesn't
// ignore SIGPIPE would die of a helper that went away; and the helper is
// started through posix_spawn and a shell rather than by forking the host,
// so the host's threads, locks and signal handlers play no part and it is
// never left with a child to reap.

#include "client.h"

#include <QByteArray>

#include <algorithm>
#include <atomic>
#include <cerrno>
#include <csignal>
#include <cstddef>
#include <cstdlib>
#include <cstring>
#include <optional>
#include <thread>
#include <vector>

#include <fcntl.h>
#include <poll.h>
#include <spawn.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SLERP_EXECUTABLE
#define SLERP_EXECUTABLE "/usr/local/bin/slerp"
#endif

extern char **environ;

namespace slerp {

namespace {

using namespace std::chrono_literals;

/*┌─────────────────╮
  | Protocol        |
  └─────────────────╯*/
//region Protocol

// Message kinds and limits, as crates/helper/src/protocol.rs defines them.
constexpr uint8_t kThumbnail = 0x01;
constexpr uint8_t kPng = 0x41;
constexpr uint8_t kFrames = 0x42;
constexpr uint8_t kNoThumbnail = 0x43;
constexpr uint8_t kFailed = 0x44;
constexpr uint8_t kExpired = 0x45;
constexpr uint8_t kBusy = 0x46;
constexpr uint8_t kVersion = 0x47;
constexpr size_t kHeader = 8;
constexpr size_t kMaxPath = 4096;
constexpr uint32_t kMaxSize = 8192;
constexpr uint32_t kMaxFrames = 1024;
constexpr uint32_t kMaxTimeoutMs = 10 * 60 * 1000;
constexpr uint32_t kMaxPng = 64u << 20;
constexpr uint64_t kMaxFrameBytes = 256ull << 20;
constexpr uint32_t kMaxMessage = 4096;

/** How long a connect may wait for room in the helper's queue; a full queue means it is far behind. */
constexpr auto kConnectWait = 50ms;
/** How often to try connecting while a freshly started helper sets up its socket. */
constexpr auto kRetryEvery = 5ms;
/** How long to keep trying after the started helper has exited, in case another one won the start race. */
constexpr auto kGoneGrace = 200ms;

void putU32(std::vector<uint8_t> &out, uint32_t v) {
	for (int i = 0; i < 4; i++) out.push_back(uint8_t(v >> (8 * i)));
}

uint32_t getU32(const uint8_t *p) {
	return uint32_t(p[0]) | uint32_t(p[1]) << 8 | uint32_t(p[2]) << 16 | uint32_t(p[3]) << 24;
}

//endregion

/*┌─────────────────╮
  | Sockets         |
  └─────────────────╯*/
//region Sockets

/** A file descriptor that closes itself. */
class Fd {
public:
	Fd() = default;
	explicit Fd(int fd) : m_fd(fd) {}
	Fd(const Fd &) = delete;
	Fd &operator=(const Fd &) = delete;
	Fd(Fd &&o) noexcept : m_fd(o.m_fd) { o.m_fd = -1; }
	Fd &operator=(Fd &&o) noexcept {
		std::swap(m_fd, o.m_fd);
		return *this;
	}
	~Fd() {
		if (m_fd >= 0) close(m_fd);
	}
	int get() const { return m_fd; }

private:
	int m_fd = -1;
};

/** Whole milliseconds until `deadline`, rounded up so a wait never ends early; 0 once it has passed. */
int msUntil(Clock::time_point deadline) {
	auto left = deadline - Clock::now();
	if (left <= Clock::duration::zero()) return 0;
	return int(std::min<int64_t>(std::chrono::ceil<std::chrono::milliseconds>(left).count(), INT32_MAX));
}

/** Waits until `fd` is ready for `events` (or has hung up), or `deadline` passes. */
bool waitFor(int fd, short events, Clock::time_point deadline) {
	for (;;) {
		int ms = msUntil(deadline);
		if (ms <= 0) return false;
		pollfd p{fd, events, 0};
		int r = poll(&p, 1, ms);
		if (r > 0) return true;
		if (r < 0 && errno != EINTR) return false;
	}
}

bool sendAll(int fd, const uint8_t *p, size_t n, Clock::time_point deadline) {
	while (n > 0) {
		ssize_t k = send(fd, p, n, MSG_NOSIGNAL | MSG_DONTWAIT);
		if (k > 0) {
			p += k, n -= size_t(k);
		} else if (k < 0 && errno == EINTR) {
			continue;
		} else if (k < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
			if (!waitFor(fd, POLLOUT, deadline)) return false;
		} else {
			return false;
		}
	}
	return true;
}

/** Fills `p`; false when the peer closes first or the deadline passes. */
bool recvAll(int fd, uint8_t *p, size_t n, Clock::time_point deadline) {
	while (n > 0) {
		ssize_t k = recv(fd, p, n, MSG_DONTWAIT);
		if (k > 0) {
			p += k, n -= size_t(k);
		} else if (k == 0) {
			return false;
		} else if (errno == EINTR) {
			continue;
		} else if (errno == EAGAIN || errno == EWOULDBLOCK) {
			if (!waitFor(fd, POLLIN, deadline)) return false;
		} else {
			return false;
		}
	}
	return true;
}

enum class Connected { Yes, Absent, Refused };

/**
 * Connects to the helper's socket and checks it runs as this user, since
 * talking to someone else's would hand them our file names.  `Absent`
 * means nothing listens there: no socket, or a stale one. */
Connected connectTo(const std::string &path, Fd &out) {
	sockaddr_un addr{};
	addr.sun_family = AF_UNIX;
	if (path.size() >= sizeof addr.sun_path) return Connected::Refused;
	memcpy(addr.sun_path, path.data(), path.size());
	Fd fd(socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0));
	if (fd.get() < 0) return Connected::Refused;
	// Linux bounds a blocking connect to a full Unix socket queue by the send timeout.
	timeval wait{0, int(std::chrono::microseconds(kConnectWait).count())};
	setsockopt(fd.get(), SOL_SOCKET, SO_SNDTIMEO, &wait, sizeof wait);
	auto len = socklen_t(offsetof(sockaddr_un, sun_path) + path.size() + 1);
	if (connect(fd.get(), reinterpret_cast<sockaddr *>(&addr), len) != 0)
		return errno == ENOENT || errno == ECONNREFUSED ? Connected::Absent : Connected::Refused;
	ucred cred{};
	socklen_t credLen = sizeof cred;
	if (getsockopt(fd.get(), SOL_SOCKET, SO_PEERCRED, &cred, &credLen) != 0 || credLen != sizeof cred ||
		cred.uid != geteuid())
		return Connected::Refused;
	int flags = fcntl(fd.get(), F_GETFL);
	if (flags < 0 || fcntl(fd.get(), F_SETFL, flags | O_NONBLOCK) < 0) return Connected::Refused;
	out = std::move(fd);
	return Connected::Yes;
}

//endregion

/*┌─────────────────╮
  | Starting        |
  └─────────────────╯*/
//region Starting

bool executable(const std::string &path) { return !path.empty() && access(path.c_str(), X_OK) == 0; }

/** `slerp` itself: `SLERP_EXECUTABLE` when set (tests), the installed path, or the first on PATH. */
std::string helperExecutable() {
	const char *env = getenv("SLERP_EXECUTABLE");
	if (env && env[0] == '/' && executable(env)) return env;
	if (executable(SLERP_EXECUTABLE)) return SLERP_EXECUTABLE;
	const char *path = getenv("PATH");
	for (std::string_view rest = path ? path : ""; !rest.empty();) {
		size_t colon = rest.find(':');
		std::string_view dir = rest.substr(0, colon);
		rest = colon == std::string_view::npos ? std::string_view() : rest.substr(colon + 1);
		if (dir.empty() || dir[0] != '/') continue;
		std::string candidate = std::string(dir) + "/slerp";
		if (executable(candidate)) return candidate;
	}
	return {};
}

/**
 * Starts `slerp helper` as a daemon that outlives this process, as
 * crates/helper's `spawn_detached` does: in a new session, with `/` as its
 * working directory, standard streams on /dev/null (stderr appended to the
 * file `SLERP_LOG` names, when it is absolute), no inherited descriptors,
 * and the host's Vulkan driver pins left out, since the helper picks its
 * own GPU.
 *
 * Nothing forks inside the host.  posix_spawn, glibc's vfork-style clone
 * made for multithreaded programs, runs /bin/sh, which starts the helper in
 * the background, prints its pid and exits at once.  The helper is then
 * adopted by init (or the session's subreaper), and the host never has a
 * long-lived child to reap.  Returns the helper's pid, or -1 when it
 * couldn't be started. */
pid_t startHelper(const std::string &dir) {
	// 1. The private directory the socket goes in, and the program.
	if (mkdir(dir.c_str(), 0700) != 0 && errno != EEXIST) return -1;
	if (access(dir.c_str(), W_OK | X_OK) != 0) return -1;
	std::string exe = helperExecutable();
	if (exe.empty()) return -1;
	std::vector<char *> envp;
	for (char **e = environ; e && *e; e++) {
		std::string_view var(*e);
		if (var.starts_with("VK_DRIVER_FILES=") || var.starts_with("VK_ICD_FILENAMES=")) continue;
		envp.push_back(*e);
	}
	envp.push_back(nullptr);
	const char *logPath = getenv("SLERP_LOG");
	std::string log = logPath && logPath[0] == '/' ? logPath : "/dev/null";

	// 2. The shell: its output, the helper's pid, comes back on a pipe; it
	//    gets no other descriptor, default signal handling and no blocked
	//    signals, whatever the host has.
	int pipeFds[2];
	if (pipe2(pipeFds, O_CLOEXEC) != 0) return -1;
	Fd pidRead(pipeFds[0]), pidWrite(pipeFds[1]);
	posix_spawn_file_actions_t actions;
	posix_spawn_file_actions_init(&actions);
	posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0);
	posix_spawn_file_actions_adddup2(&actions, pidWrite.get(), 1);
	posix_spawn_file_actions_addopen(&actions, 2, "/dev/null", O_WRONLY, 0);
	posix_spawn_file_actions_addclosefrom_np(&actions, 3);
	posix_spawn_file_actions_addchdir_np(&actions, "/");
	posix_spawnattr_t attr;
	posix_spawnattr_init(&attr);
	sigset_t none, all;
	sigemptyset(&none);
	sigfillset(&all);
	posix_spawnattr_setsigmask(&attr, &none);
	posix_spawnattr_setsigdefault(&attr, &all);
	posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETSID | POSIX_SPAWN_SETSIGMASK | POSIX_SPAWN_SETSIGDEF);
	// $0 is the helper's program and $1 the file its errors go to.
	char sh[] = "/bin/sh", dashC[] = "-c";
	char script[] = "\"$0\" helper </dev/null >/dev/null 2>>\"$1\" & echo $!";
	char *argv[] = {sh, dashC, script, exe.data(), log.data(), nullptr};
	pid_t shell = -1;
	int rc = posix_spawn(&shell, sh, &actions, &attr, argv, envp.data());
	posix_spawn_file_actions_destroy(&actions);
	posix_spawnattr_destroy(&attr);
	pidWrite = Fd();
	if (rc != 0) return -1;

	// 3. Read the pid until the shell exits, then reap it.  It is gone
	//    within milliseconds; the bound is only for a shell that hangs.
	std::string text;
	auto until = Clock::now() + 1s;
	for (char buf[32]; waitFor(pidRead.get(), POLLIN, until);) {
		ssize_t k = read(pidRead.get(), buf, sizeof buf);
		if (k > 0 && text.size() < 64) {
			text.append(buf, size_t(k));
		} else if (k == 0 || (k < 0 && errno != EINTR)) {
			break;
		}
	}
	// A shell still there now is stuck, so it goes; the helper it started
	// carries on, since a signal to the shell alone doesn't reach it.
	if (waitpid(shell, nullptr, WNOHANG) == 0) {
		kill(shell, SIGKILL);
		while (waitpid(shell, nullptr, 0) < 0 && errno == EINTR) {}
	}
	char *end = nullptr;
	long pid = strtol(text.c_str(), &end, 10);
	return pid > 0 && end != text.c_str() ? pid_t(pid) : -1;
}

/** When a helper last failed to start; later requests fail fast for a while rather than each waiting it out. */
std::atomic<int64_t> g_lastStartFailure{INT64_MIN};
constexpr auto kStartRetryAfter = 30s;

bool startFailedRecently() {
	int64_t last = g_lastStartFailure.load(std::memory_order_relaxed);
	return last != INT64_MIN && Clock::now().time_since_epoch().count() - last <
		std::chrono::duration_cast<Clock::duration>(kStartRetryAfter).count();
}

void noteStartFailure() {
	g_lastStartFailure.store(Clock::now().time_since_epoch().count(), std::memory_order_relaxed);
}

//endregion

/*┌─────────────────╮
  | Replies         |
  └─────────────────╯*/
//region Replies

Reply failure(Reply::Outcome outcome, const char *message) {
	Reply r;
	r.outcome = outcome;
	r.message = QString::fromUtf8(message);
	return r;
}

/** Why a read stopped short: the deadline, or the helper hanging up (crashed, killed). */
Reply cutShort(Clock::time_point deadline) {
	return Clock::now() >= deadline ? failure(Reply::TimedOut, "the helper didn't answer in time")
									: failure(Reply::NoHelper, "the helper closed the connection");
}

Reply readFrames(int fd, uint32_t len, Clock::time_point deadline) {
	uint8_t fixed[12];
	if (len < sizeof fixed) return failure(Reply::Failed, "frames reply too short");
	if (!recvAll(fd, fixed, sizeof fixed, deadline)) return cutShort(deadline);
	uint32_t width = getU32(fixed), height = getU32(fixed + 4), count = getU32(fixed + 8);
	if (width < 1 || width > kMaxSize || height < 1 || height > kMaxSize || count < 1 || count > kMaxFrames)
		return failure(Reply::Failed, "frame dimensions out of range");
	uint64_t frame = uint64_t(width) * height * 4, total = frame * count;
	if (total > kMaxFrameBytes || uint64_t(len) != sizeof fixed + total)
		return failure(Reply::Failed, "frames reply length doesn't match its dimensions");
	Reply r;
	r.outcome = Reply::Ok;
	r.frames.reserve(count);
	for (uint32_t i = 0; i < count; i++) {
		// Straight alpha, as the helper's PNGs and frames are.
		QImage image(int(width), int(height), QImage::Format_RGBA8888);
		if (image.isNull() || uint64_t(image.sizeInBytes()) != frame) return failure(Reply::Failed, "out of memory");
		if (!recvAll(fd, image.bits(), size_t(frame), deadline)) return cutShort(deadline);
		r.frames.append(std::move(image));
	}
	return r;
}

Reply readReply(int fd, Clock::time_point deadline) {
	uint8_t head[kHeader];
	if (!recvAll(fd, head, sizeof head, deadline)) return cutShort(deadline);
	if (head[0] != 'P' || head[1] != 'T') return failure(Reply::Failed, "the helper's reply isn't Slerp's protocol");
	if (head[2] != kProtocolVersion) {
		// An older helper exits after this, so the next request starts the new one.
		return failure(Reply::NoHelper, "the helper speaks another protocol version");
	}
	uint32_t len = getU32(head + 4);
	switch (head[3]) {
	case kFrames:
		return readFrames(fd, len, deadline);
	case kPng: {
		// A helper from before the plugin answers with its thumbnail.
		if (len > kMaxPng) return failure(Reply::Failed, "PNG reply too large");
		QByteArray png(qsizetype(len), Qt::Uninitialized);
		if (!recvAll(fd, reinterpret_cast<uint8_t *>(png.data()), len, deadline)) return cutShort(deadline);
		QImage image;
		if (!image.loadFromData(png, "PNG")) return failure(Reply::Failed, "unreadable PNG reply");
		Reply r;
		r.outcome = Reply::Ok;
		r.frames.append(image.convertToFormat(QImage::Format_RGBA8888));
		return r;
	}
	case kNoThumbnail:
		return failure(Reply::NoPicture, "nothing to show");
	case kFailed: {
		if (len > kMaxMessage) return failure(Reply::Failed, "failure message too long");
		QByteArray text(qsizetype(len), Qt::Uninitialized);
		if (!recvAll(fd, reinterpret_cast<uint8_t *>(text.data()), len, deadline)) return cutShort(deadline);
		Reply r = failure(Reply::Failed, "");
		r.message = QString::fromUtf8(text);
		return r;
	}
	case kExpired:
		return failure(Reply::TimedOut, "the helper gave up at the deadline");
	case kBusy:
		return failure(Reply::NoHelper, "the helper is busy");
	case kVersion:
		return failure(Reply::NoHelper, "the helper speaks another protocol version");
	default:
		return failure(Reply::Failed, "unexpected reply");
	}
}

//endregion

}

std::string runtimeDir() {
	const char *dir = getenv("XDG_RUNTIME_DIR");
	struct stat st {};
	if (!dir || dir[0] != '/' || stat(dir, &st) != 0 || !S_ISDIR(st.st_mode)) return {};
	return std::string(dir) + "/slerp";
}

Reply request(const std::string &path, uint32_t size, uint32_t frames, Clock::time_point deadline) {
	// 1. Encode first, so a bad request fails before anything starts.
	if (path.empty() || path[0] != '/' || path.size() > kMaxPath || path.find('\0') != std::string::npos)
		return failure(Reply::Failed, "the path can't be sent");
	std::string dir = runtimeDir();
	if (dir.empty()) return failure(Reply::NoHelper, "no runtime directory");
	std::string socketPath = dir + "/socket";

	// 2. Connect, starting a helper when none listens.  It binds its socket
	//    before starting the GPU, so it answers connects within milliseconds.
	Fd fd;
	Connected c = connectTo(socketPath, fd);
	if (c == Connected::Absent) {
		if (startFailedRecently()) return failure(Reply::NoHelper, "the helper failed to start a moment ago");
		pid_t helper = startHelper(dir);
		if (helper <= 0) {
			noteStartFailure();
			return failure(Reply::NoHelper, "couldn't start the helper");
		}
		// A helper that loses a start race exits while the winner binds, so
		// one that has gone gets a moment's grace before this gives up.
		std::optional<Clock::time_point> gone;
		while ((c = connectTo(socketPath, fd)) == Connected::Absent) {
			auto now = Clock::now();
			if (now >= deadline) break;
			if (!gone && kill(helper, 0) != 0 && errno == ESRCH) gone = now;
			if (gone && now - *gone > kGoneGrace) {
				noteStartFailure();
				return failure(Reply::NoHelper, "the helper exited as it started");
			}
			std::this_thread::sleep_for(std::min<Clock::duration>(kRetryEvery, deadline - now));
		}
	}
	if (c != Connected::Yes) {
		return Clock::now() >= deadline ? failure(Reply::TimedOut, "the helper didn't start in time")
										: failure(Reply::NoHelper, "the helper can't be reached");
	}

	// 3. Send.  The deadline travels as the time left.
	int ms = msUntil(deadline);
	if (ms <= 0) return failure(Reply::TimedOut, "the deadline passed");
	std::vector<uint8_t> message{'P', 'T', kProtocolVersion, kThumbnail};
	putU32(message, uint32_t(12 + path.size()));
	putU32(message, std::clamp<uint32_t>(size, 1, kMaxSize));
	putU32(message, std::min(frames, kMaxFrames));
	putU32(message, std::min<uint32_t>(uint32_t(ms), kMaxTimeoutMs));
	message.insert(message.end(), path.begin(), path.end());
	// A helper that refuses the request (busy, another version) may answer
	// and hang up before it is all written, so a failed send still reads
	// whatever answer is waiting.
	bool sent = sendAll(fd.get(), message.data(), message.size(), deadline);
	Reply reply = readReply(fd.get(), deadline);
	if (!sent && reply.outcome == Reply::Ok) return failure(Reply::Failed, "reply to an unsent request");
	return reply;
}

}
