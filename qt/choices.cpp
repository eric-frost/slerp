// Reading the user's choice of formats.  Slerp writes `file-types.jsonc`
// (crates/core/src/choices.rs): a comment, then JSON whose `thumbnails`
// object maps format ids to true or false where the user differs from the
// default.  Only `false` matters here.

#include "choices.h"

#include <QByteArray>
#include <QFile>
#include <QJsonDocument>
#include <QJsonObject>
#include <QSet>
#include <QString>

#include <cstdlib>
#include <mutex>
#include <sys/stat.h>

namespace slerp {

namespace {

QString choicesPath() {
	const char *config = getenv("XDG_CONFIG_HOME");
	if (config && *config) return QString::fromLocal8Bit(config) + "/slerp/file-types.jsonc";
	const char *home = getenv("HOME");
	return home ? QString::fromLocal8Bit(home) + "/.config/slerp/file-types.jsonc" : QString();
}

/** JSONC to JSON: `//` and block comments outside strings removed. */
QByteArray stripComments(const QByteArray &in) {
	QByteArray out;
	out.reserve(in.size());
	bool string = false;
	for (qsizetype i = 0; i < in.size(); i++) {
		char c = in[i];
		if (string) {
			out.append(c);
			if (c == '\\' && i + 1 < in.size()) out.append(in[++i]);
			else if (c == '"') string = false;
		} else if (c == '"') {
			string = true;
			out.append(c);
		} else if (c == '/' && i + 1 < in.size() && in[i + 1] == '/') {
			while (i < in.size() && in[i] != '\n') i++;
			out.append('\n');
		} else if (c == '/' && i + 1 < in.size() && in[i + 1] == '*') {
			i += 2;
			while (i + 1 < in.size() && !(in[i] == '*' && in[i + 1] == '/')) i++;
			i++;
		} else {
			out.append(c);
		}
	}
	return out;
}

struct Cache {
	std::mutex mutex;
	QString path;
	struct timespec mtime{};
	off_t size = -1;
	QSet<QString> off;
};

Cache &cache() {
	static Cache *c = new Cache;
	return *c;
}

}

bool switchedOff(const char *formatId) {
	if (!formatId || !*formatId) return false;
	QString path = choicesPath();
	if (path.isEmpty()) return false;
	struct stat st{};
	bool exists = stat(QFile::encodeName(path).constData(), &st) == 0;
	Cache &c = cache();
	std::lock_guard lock(c.mutex);
	bool same = c.path == path && (exists ? (c.size == st.st_size && c.mtime.tv_sec == st.st_mtim.tv_sec &&
		c.mtime.tv_nsec == st.st_mtim.tv_nsec) : c.size == -1);
	if (!same) {
		c.path = path;
		c.size = exists ? st.st_size : -1;
		c.mtime = exists ? st.st_mtim : timespec{};
		c.off.clear();
		QFile file(path);
		// A choices file is a few kilobytes; a huge one isn't Slerp's.
		if (exists && st.st_size < (1 << 20) && file.open(QIODevice::ReadOnly)) {
			QJsonObject thumbs = QJsonDocument::fromJson(stripComments(file.readAll())).object()["thumbnails"].toObject();
			for (auto it = thumbs.begin(); it != thumbs.end(); ++it)
				if (it.value().isBool() && !it.value().toBool()) c.off.insert(it.key());
		}
	}
	return c.off.contains(QString::fromUtf8(formatId));
}

}
