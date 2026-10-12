// Slerp's Qt image plugin.  It makes every Qt program that reads images
// through QImageReader (Gwenview, Dolphin's Information panel, Okular's
// image backend, file dialogs' previews) read Slerp's texture formats as
// pictures, and its models, levels and materials as turntable animations,
// which QMovie plays.  Dolphin's Information panel plays any file whose MIME
// type a Qt image plugin claims with animation support, so a selected model
// turns there with no change to Dolphin.
//
// Nothing is decoded or rendered here: the plugin asks Slerp's resident
// helper over its socket (client.cpp), so no Vulkan driver, file parser or
// GPU memory ever lives inside the host.  The host's costs are a check of a
// file's first bytes when Qt asks, a round trip to the helper when it
// reads, and the frames themselves.  Any failure reads as "no image", never
// a crash or a hang: the helper's answer is awaited only until a deadline.
//
// A model's first read is one picture: the file's thumbnail from the
// freedesktop thumbnail cache, which a file manager has just put there, or
// else the same view (the first turntable frame) asked of the helper.
// Readers that want one picture, such as thumbnail generators, stop there.
// The 36-frame turntable is fetched only when a second frame is read, on a
// thread of its own, because it takes the helper 35-250 ms and QMovie reads
// on the host's GUI thread.  Until it arrives a GUI thread is handed the
// first picture again, so the model simply starts turning; other threads
// wait for the real frames.

#include "choices.h"
#include "client.h"
#include "formats.h"

#include <QBuffer>
#include <QCryptographicHash>
#include <QFile>
#include <QFileInfo>
#include <QImageIOHandler>
#include <QImageIOPlugin>
#include <QLoggingCategory>
#include <QStandardPaths>
#include <QThread>
#include <QUrl>
#include <QVariant>

#include <atomic>
#include <cerrno>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <deque>
#include <functional>
#include <memory>
#include <mutex>
#include <thread>

#include <dirent.h>
#include <fcntl.h>
#include <signal.h>
#include <sys/stat.h>
#include <unistd.h>

Q_LOGGING_CATEGORY(lcSlerp, "slerp.qimage", QtWarningMsg)

namespace {

using namespace std::chrono_literals;
using slerp::Clock;

/** Frames in one turn. */
constexpr int kTurntableFrames = 36;
/** The size a turntable reports, and renders at when the reader asks for no other. */
constexpr int kTurntableSize = 512;
/** The largest turntable frame; 36 frames of 1024 pixels squared are 151 MB. */
constexpr int kMaxTurntableSize = 1024;
/** 100 ms a frame: one turn in 3.6 s. */
constexpr int kFrameDelayMs = 100;
/** How often a QMovie showing the first picture looks again for the turntable. */
constexpr int kWaitingDelayMs = 40;
/** The largest still picture, the helper protocol's limit. */
constexpr int kMaxStillSize = 8192;
/**
 * The size asked for a picture whose header doesn't say its size (retro
 * dumps, Aseprite and the science formats): enough for a screen, while a
 * picture smaller than that comes back at its own size. */
constexpr int kUnknownStillSize = 1024;
/**
 * How long a read may block waiting for the helper, including starting it
 * and its GPU when it isn't running.  This bounds how long a broken file or
 * a hung GPU can hold up the host. */
constexpr auto kDeadline = 2500ms;
/** How long a turntable fetched on its own thread may take. */
constexpr auto kBackgroundDeadline = 6s;
/** The largest file read from a device that isn't a file, to hand to the helper as a copy. */
constexpr qint64 kMaxCopyBytes = 1 << 30;
/**
 * How many files' pictures stay cached for readers that open the same file
 * again.  One is enough for Gwenview handing a file from its loading thread
 * to its QMovie, and for selecting a file again in Dolphin, and a turntable
 * costs 38 MB of the host's memory at 512 pixels. */
constexpr size_t kCachedFiles = 1;

int longSide(QSize s) { return s.isValid() ? qMax(s.width(), s.height()) : 0; }

/*┌─────────────────╮
  | Fetches         |
  └─────────────────╯*/
//region Fetches

/** The frames of one file at one size. */
struct Pictures {
	QList<QImage> frames;
	/** The size asked for, which a later reader at another size compares with. */
	int size = 0;
};

using PicturesPtr = std::shared_ptr<const Pictures>;

/** One request to the helper, which any number of readers can wait for. */
struct Fetch {
	std::mutex mutex;
	std::condition_variable done;
	bool finished = false;
	PicturesPtr result;

	/** Waits until the answer is in or `until` passes, and says whether it is in. */
	bool wait(Clock::time_point until) {
		std::unique_lock lock(mutex);
		return done.wait_until(lock, until, [&] { return finished; });
	}

	/** Whether it finished without pictures, so a new request is worth making. */
	bool failed() {
		std::lock_guard lock(mutex);
		return finished && !result;
	}

	void finish(PicturesPtr pictures) {
		{
			std::lock_guard lock(mutex);
			finished = true;
			result = std::move(pictures);
		}
		done.notify_all();
	}
};

using Work = std::function<PicturesPtr()>;

/** What has been asked of the helper for one file at one size. */
struct Entry {
	/** The first frame alone. */
	std::shared_ptr<Fetch> first;
	/** Every frame. */
	std::shared_ptr<Fetch> turntable;
};

/**
 * Files asked about, most recent first.  Readers of the same file share
 * requests: Gwenview reads a file's first frames on a worker thread, then
 * plays it with a QMovie of its own on the GUI thread, and Dolphin starts a
 * new QMovie each time a file is selected again.
 *
 * It is never destroyed, nor is the plugin ever unloaded (it links with
 * `-z nodelete`), so a fetch thread still running while the host exits
 * can't find either gone. */
struct Recent {
	std::mutex mutex;
	std::deque<std::pair<QByteArray, std::shared_ptr<Entry>>> entries;
};

Recent &recent() {
	static Recent *r = new Recent;
	return *r;
}

/** The entry for `key`, made when new and moved to the front. */
std::shared_ptr<Entry> entryFor(const QByteArray &key) {
	std::lock_guard lock(recent().mutex);
	auto &list = recent().entries;
	std::shared_ptr<Entry> entry;
	for (auto it = list.begin(); it != list.end(); ++it) {
		if (it->first != key) continue;
		entry = it->second;
		list.erase(it);
		break;
	}
	if (!entry) entry = std::make_shared<Entry>();
	list.emplace_front(key, entry);
	while (list.size() > kCachedFiles) list.pop_back();
	return entry;
}

/**
 * The fetch in `slot` of `entry`, or, when it has none or the last one
 * failed, `work` started: on a thread of its own when `background` is set,
 * otherwise right here. */
std::shared_ptr<Fetch> fetch(Entry &entry, std::shared_ptr<Fetch> Entry::*slot, const Work &work, bool background) {
	std::shared_ptr<Fetch> fetch;
	{
		std::lock_guard lock(recent().mutex);
		std::shared_ptr<Fetch> &held = entry.*slot;
		if (held && !held->failed()) return held;
		held = fetch = std::make_shared<Fetch>();
	}
	if (background) {
		try {
			std::thread([fetch, work] { fetch->finish(work()); }).detach();
			return fetch;
		} catch (...) {
			// No thread to be had: do it here instead.
		}
	}
	fetch->finish(work());
	return fetch;
}

//endregion

/*┌─────────────────╮
  | Sources         |
  └─────────────────╯*/
//region Sources

/** Removes copies left in `dir` by processes that died before cleaning up after themselves. */
void sweepCopies(const std::string &dir) {
	DIR *d = opendir(dir.c_str());
	if (!d) return;
	while (dirent *e = readdir(d)) {
		long pid = 0;
		if (strncmp(e->d_name, "qt-", 3) != 0 || sscanf(e->d_name + 3, "%ld-", &pid) != 1 || pid <= 0) continue;
		if (kill(pid_t(pid), 0) != 0 && errno == ESRCH) unlinkat(dirfd(d), e->d_name, 0);
	}
	closedir(d);
}

/**
 * A file the helper can open for a device's content: the file itself when
 * the device is one, so neighbouring files (a glTF's buffers, an OBJ's
 * materials) are found, otherwise a private copy in the helper's own
 * directory, removed again when this goes. */
class Source {
public:
	Source() = default;
	Source(const Source &) = delete;
	Source &operator=(const Source &) = delete;
	~Source() {
		if (!m_copy.empty()) unlink(m_copy.c_str());
	}

	/** Prepares `device`, whose content is of format `key`.  False when it can't. */
	bool open(QIODevice *device, const char *key) {
		// 1. A file, read from its start, as QImageReader(fileName) gives.
		if (auto *file = qobject_cast<QFile *>(device); file && device->pos() == 0) {
			QFileInfo info(file->fileName());
			if (info.isFile() && !file->fileName().startsWith(u':')) {
				m_file = info.absoluteFilePath();
				m_path = QFile::encodeName(m_file).toStdString();
				m_identity = QFile::encodeName(m_file) + '\0' + QByteArray::number(info.size()) + '\0' +
					QByteArray::number(info.lastModified().toMSecsSinceEpoch());
				return true;
			}
		}
		// 2. Anything else (Gwenview reads files into a QBuffer): its bytes.
		//    QIODevice::read(n) allocates n bytes at once, so unknown sizes
		//    are read a piece at a time.
		QByteArray data;
		if (auto *buffer = qobject_cast<QBuffer *>(device)) {
			data = buffer->data().mid(device->pos());
		} else if (!device->isSequential()) {
			if (device->size() - device->pos() > kMaxCopyBytes) return false;
			data = device->readAll();
		} else {
			while (data.size() <= kMaxCopyBytes) {
				QByteArray piece = device->read(1 << 20);
				if (piece.isEmpty()) break;
				data += piece;
			}
		}
		if (data.isEmpty() || data.size() > kMaxCopyBytes) return false;
		m_data = data;
		m_identity = "data:" + QByteArray::number(data.size()) + '\0' +
			QByteArray::number(qHashBits(data.constData(), size_t(data.size())));
		m_key = key;
		return true;
	}

	/** Identifies the content, for sharing fetches. */
	const QByteArray &identity() const { return m_identity; }

	/** The file's own absolute path, when it is a file. */
	const QString &file() const { return m_file; }

	/**
	 * The path to send, writing the copy first if there is one.  The copy
	 * is named for its format, since the helper tells text formats apart
	 * by extension.  The first picture and the turntable may both ask, on
	 * different threads. */
	std::string path() {
		std::lock_guard lock(m_mutex);
		if (!m_path.empty() || m_data.isEmpty()) return m_path;
		std::string dir = slerp::runtimeDir();
		if (dir.empty() || (mkdir(dir.c_str(), 0700) != 0 && errno != EEXIST)) return {};
		sweepCopies(dir);
		static std::atomic<unsigned> counter{0};
		std::string path = dir + "/qt-" + std::to_string(getpid()) + "-" + std::to_string(counter++) + "." + m_key;
		int fd = ::open(path.c_str(), O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0600);
		if (fd < 0) return {};
		m_copy = path;
		const char *p = m_data.constData();
		qsizetype left = m_data.size();
		while (left > 0) {
			ssize_t k = write(fd, p, size_t(left));
			if (k < 0 && errno == EINTR) continue;
			if (k <= 0) break;
			p += k, left -= k;
		}
		close(fd);
		if (left > 0) return {};
		m_path = path;
		return m_path;
	}

private:
	std::mutex m_mutex;
	QString m_file;
	std::string m_path, m_copy;
	QByteArray m_data, m_identity;
	const char *m_key = "";
};

//endregion

/*┌─────────────────╮
  | Pictures        |
  └─────────────────╯*/
//region Pictures

/** Scales `image` to fill `size`, keeping its aspect and padding with transparency. */
QImage fitted(const QImage &image, QSize size) {
	if (!size.isValid() || size.isEmpty() || image.size() == size) return image;
	QImage scaled = image.scaled(size, Qt::KeepAspectRatio, Qt::SmoothTransformation);
	if (scaled.size() == size) return scaled;
	QImage out(size, QImage::Format_RGBA8888);
	if (out.isNull()) return scaled;
	out.fill(Qt::transparent);
	const int x = (size.width() - scaled.width()) / 2, y = (size.height() - scaled.height()) / 2;
	QImage src = scaled.convertToFormat(QImage::Format_RGBA8888);
	for (int row = 0; row < src.height(); row++)
		memcpy(out.scanLine(y + row) + 4 * x, src.constScanLine(row), size_t(src.width()) * 4);
	return out;
}

/**
 * The file's thumbnail from the freedesktop thumbnail cache, the one file
 * managers keep, when one is there for the file as it is now.  The name is
 * the MD5 of the file's URI written the way KIO writes it, and the
 * thumbnail's `Thumb::MTime` must match the file's.  The size at least
 * `want`, or else the largest, is preferred. */
QImage cachedThumbnail(const QString &file, int want) {
	QFileInfo info(file);
	const QString uri = QUrl::fromLocalFile(file).adjusted(QUrl::RemovePassword | QUrl::NormalizePathSegments).url();
	const QByteArray md5 = QCryptographicHash::hash(QFile::encodeName(uri), QCryptographicHash::Md5).toHex();
	const QString name = QString::fromLatin1(md5);
	const QString mtime = QString::number(info.lastModified().toSecsSinceEpoch());
	const QString cache = QStandardPaths::writableLocation(QStandardPaths::GenericCacheLocation);
	const QString root = cache + QStringLiteral("/thumbnails/");
	struct Folder {
		const char *name;
		int size;
	};
	const Folder folders[] = {{"normal", 128}, {"large", 256}, {"x-large", 512}, {"xx-large", 1024}};
	QImage best;
	for (const Folder &f : folders) {
		// QImageReader::text() cuts keys at their first colon; the
		// decoded image keeps "Thumb::MTime" whole, which is what KIO reads.
		QImage image(root + QLatin1String(f.name) + u'/' + name + QStringLiteral(".png"), "png");
		if (image.isNull() || image.text(QStringLiteral("Thumb::MTime")) != mtime) continue;
		best = image;
		if (f.size >= want) break;
	}
	return best;
}

//endregion

/*┌─────────────────╮
  | Handler         |
  └─────────────────╯*/
//region Handler

class SlerpHandler final : public QImageIOHandler {
public:
	explicit SlerpHandler(const slerp::Format *format) : m_format(format) {}

	bool canRead() const override {
		if (m_pictures) return m_next < m_pictures->frames.size();
		if (m_failed) return false;
		// Waiting for a turntable: the first picture or the frames come next.
		if (!m_first.isNull()) return true;
		if (!recognised()) return false;
		setFormat(m_format->key);
		return true;
	}

	bool read(QImage *image) override {
		// A host built without exception handling must never see one; an
		// allocation that throws becomes a failed read.
		try {
			QImage picture;
			if (m_pictures || (m_first.isNull() ? readFirst() : readOn())) {
				if (m_pictures) {
					if (m_next >= m_pictures->frames.size()) return false;
					picture = m_pictures->frames[m_next++];
				} else {
					picture = m_first;
				}
			} else {
				m_failed = true;
				return false;
			}
			*image = fitted(picture, frameSize());
			return !image->isNull();
		} catch (...) {
			m_failed = true;
			return false;
		}
	}

	bool supportsOption(ImageOption option) const override {
		if (option == Size || option == ScaledSize) return true;
		return option == Animation && turntable();
	}

	QVariant option(ImageOption option) const override {
		switch (option) {
		case Size:
			return naturalSize();
		case ScaledSize:
			return m_scaledSize;
		case Animation:
			return turntable();
		default:
			return {};
		}
	}

	void setOption(ImageOption option, const QVariant &value) override {
		if (option != ScaledSize) return;
		m_scaledSize = value.toSize();
		// Frames much smaller than the new size would look blurred scaled
		// up, so the next read asks for them again at the new size.
		if (m_pictures && turntable() && longSide(m_scaledSize) > m_pictures->size * 5 / 4) {
			m_pictures.reset();
			m_entry.reset();
			m_turntable.reset();
		}
	}

	int imageCount() const override {
		if (m_pictures) return int(m_pictures->frames.size());
		return turntable() ? kTurntableFrames : 1;
	}

	int loopCount() const override { return animated() ? -1 : 0; }

	int nextImageDelay() const override {
		if (!turntable()) return 0;
		if (!m_pictures) return kWaitingDelayMs;
		return animated() ? kFrameDelayMs : 0;
	}

	int currentImageNumber() const override { return qMax(0, m_next - 1); }

	bool jumpToNextImage() override { return jumpToImage(m_next + 1); }

	bool jumpToImage(int imageNumber) override {
		if (imageNumber < 0 || imageNumber >= imageCount()) return false;
		m_next = imageNumber;
		return true;
	}

private:
	bool turntable() const { return m_format && m_format->shape == slerp::Shape::Turntable; }

	/** A turntable of more than one frame; a scene with nothing to turn may come back as one. */
	bool animated() const { return turntable() && imageCount() > 1; }

	/** The size frames are handed out at: the reader's, or the natural size. */
	QSize frameSize() const { return m_scaledSize.isValid() ? m_scaledSize : naturalSize(); }

	/** The device's first bytes and the bytes left in it (-1 when it can't tell). */
	QByteArray head(qint64 *size) const {
		QIODevice *dev = device();
		if (!dev || !dev->isOpen() || !dev->isReadable()) return {};
		*size = dev->isSequential() ? -1 : dev->size() - dev->pos();
		return dev->peek(qint64(slerp::kSniffBytes));
	}

	/** Whether the device's content fits the format; a name alone is never trusted. */
	bool recognised() const {
		if (!m_format) return false;
		qint64 size = -1;
		QByteArray bytes = head(&size);
		auto *p = reinterpret_cast<const uint8_t *>(bytes.constData());
		return slerp::sniff(p, size_t(bytes.size()), size, m_format) != nullptr;
	}

	/**
	 * The picture's size before any scaling: a texture's own size from its
	 * header (within the helper's limit), a turntable's fixed one. */
	QSize naturalSize() const {
		if (m_natural.isValid()) return m_natural;
		if (turntable()) {
			m_natural = QSize(kTurntableSize, kTurntableSize);
		} else {
			qint64 size = -1;
			QByteArray bytes = head(&size);
			slerp::Size s = slerp::textureSize(reinterpret_cast<const uint8_t *>(bytes.constData()), size_t(bytes.size()));
			QSize q(int(qMin<uint32_t>(s.width, 1u << 24)), int(qMin<uint32_t>(s.height, 1u << 24)));
			if (q.isEmpty()) return {};
			if (longSide(q) > kMaxStillSize) q = q.scaled(kMaxStillSize, kMaxStillSize, Qt::KeepAspectRatio);
			m_natural = q.expandedTo(QSize(1, 1));
		}
		return m_natural;
	}

	/** The size to ask the helper for: what the reader wants, else the natural size. */
	int requestSize() const {
		int want = longSide(frameSize());
		if (turntable()) return qBound(16, want > 0 ? want : kTurntableSize, kMaxTurntableSize);
		return qBound(1, want > 0 ? want : kUnknownStillSize, kMaxStillSize);
	}

	/** What asking the helper for `frames` frames of the source at `size` does. */
	Work work(uint32_t frames, int size, Clock::duration within) const {
		auto source = m_source;
		const auto deadline = Clock::now() + within;
		return [source, frames, size, deadline]() -> PicturesPtr {
			std::string path = source->path();
			if (path.empty()) return nullptr;
			slerp::Reply reply = slerp::request(path, uint32_t(size), frames, deadline);
			if (reply.outcome != slerp::Reply::Ok || reply.frames.isEmpty()) {
				qCDebug(lcSlerp) << "no picture for" << QString::fromStdString(path) << reply.outcome << reply.message;
				return nullptr;
			}
			auto pictures = std::make_shared<Pictures>();
			pictures->frames = std::move(reply.frames);
			pictures->size = size;
			return pictures;
		};
	}

	/** Makes the device's content something the helper can open, once. */
	bool prepareSource() {
		if (m_source) return true;
		if (!recognised()) return false;
		auto source = std::make_shared<Source>();
		if (!source->open(device(), m_format->key)) {
			qCDebug(lcSlerp) << "can't hand this device to the helper";
			return false;
		}
		m_source = std::move(source);
		return true;
	}

	/** Prepares the source and finds what has been asked about it at this size. */
	bool prepare() {
		if (m_entry) return true;
		if (!prepareSource()) return false;
		m_entry = entryFor(m_source->identity() + '\0' + QByteArray::number(requestSize()));
		return true;
	}

	/**
	 * The first read.  A texture is fetched whole, and not kept: it can be
	 * hundreds of megabytes, and nobody reads it twice.  A model's turntable
	 * is used when one is already in; otherwise the first picture is the
	 * file's cached thumbnail, or the first frame alone from the helper. */
	bool readFirst() {
		if (!turntable()) {
			if (!prepareSource()) return false;
			m_pictures = work(1, requestSize(), kDeadline)();
			return m_pictures != nullptr;
		}
		if (!prepare()) return false;
		const auto until = Clock::now() + kDeadline;
		std::shared_ptr<Fetch> spin;
		{
			std::lock_guard lock(recent().mutex);
			spin = m_entry->turntable;
		}
		if (spin && spin->wait(Clock::now()) && spin->result) {
			m_pictures = spin->result;
			return true;
		}
		if (!m_source->file().isEmpty()) m_first = cachedThumbnail(m_source->file(), requestSize());
		if (!m_first.isNull()) return true;
		auto first = fetch(*m_entry, &Entry::first, work(1, requestSize(), kDeadline), false);
		if (!first->wait(until) || !first->result) return false;
		m_first = first->result->frames.value(0);
		return !m_first.isNull();
	}

	/**
	 * Later reads of a model: the turntable, asked for on its own thread.  A
	 * GUI thread gets the first picture again until it is in, so a QMovie
	 * never freezes its program; any other thread waits for it. */
	bool readOn() {
		if (!prepare()) return false;
		if (!m_turntable) {
			m_turntable = fetch(*m_entry, &Entry::turntable, work(kTurntableFrames, requestSize(), kBackgroundDeadline), true);
			m_waitSince = Clock::now();
		}
		const bool gui = QThread::isMainThread();
		if (!m_turntable->wait(gui ? Clock::now() : m_waitSince + kDeadline)) {
			return gui && Clock::now() < m_waitSince + kBackgroundDeadline;
		}
		if (!m_turntable->result) return false;
		m_pictures = m_turntable->result;
		// The first picture stood for frame 0, so the turn carries on from 1.
		if (m_next == 0 && m_pictures->frames.size() > 1) m_next = 1;
		return true;
	}

	const slerp::Format *m_format;
	QSize m_scaledSize;
	mutable QSize m_natural;
	std::shared_ptr<Source> m_source;
	std::shared_ptr<Entry> m_entry;
	std::shared_ptr<Fetch> m_turntable;
	/** The frames, once in: a texture's one, or a turntable's. */
	PicturesPtr m_pictures;
	/** A model's first picture, handed out until the turntable is in. */
	QImage m_first;
	Clock::time_point m_waitSince;
	bool m_failed = false;
	/** The frame the next read returns. */
	int m_next = 0;
};

//endregion

/*┌─────────────────╮
  | Plugin          |
  └─────────────────╯*/
//region Plugin

const slerp::Format *formatFor(const QByteArray &key) {
	return slerp::formatForKey(std::string_view(key.constData(), size_t(key.size())));
}

/** The format a device's content shows, by strong signatures only. */
const slerp::Format *detect(QIODevice *device) {
	if (!device || !device->isOpen() || !device->isReadable()) return nullptr;
	QByteArray bytes = device->peek(qint64(slerp::kSniffBytes));
	qint64 size = device->isSequential() ? -1 : device->size() - device->pos();
	return slerp::sniff(reinterpret_cast<const uint8_t *>(bytes.constData()), size_t(bytes.size()), size, nullptr);
}

}

class SlerpPlugin final : public QImageIOPlugin {
	Q_OBJECT
	Q_PLUGIN_METADATA(IID "org.qt-project.Qt.QImageIOHandlerFactoryInterface" FILE "slerp.json")

public:
	/**
	 * Qt asks this for every image it opens: with the format a file's
	 * extension or the caller gives, which costs a string comparison, or,
	 * when nothing recognised the file that way, with an empty format and
	 * the device to look at. */
	Capabilities capabilities(QIODevice *device, const QByteArray &format) const override {
		const slerp::Format *f = format.isEmpty() ? detect(device) : formatFor(format);
		return f && !slerp::switchedOff(f->id) ? CanRead : Capabilities();
	}

	QImageIOHandler *create(QIODevice *device, const QByteArray &format) const override {
		const slerp::Format *f = format.isEmpty() ? detect(device) : formatFor(format);
		// Formats the user switched off in Slerp's file types are left to
		// other plugins, as the file manager's thumbnails are.
		if (f && slerp::switchedOff(f->id)) f = nullptr;
		auto *handler = new SlerpHandler(f);
		handler->setDevice(device);
		handler->setFormat(f ? QByteArray(f->key) : format);
		return handler;
	}
};

#include "plugin.moc"
