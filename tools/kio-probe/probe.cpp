// Asks KIO for a thumbnail of each argument, the way Dolphin does, and reports the result.
//
// Environment:
//   LIST=1         list every thumbnail plugin KIO knows, with its MIME types, and exit.
//   PLUGINS=a,b    use these plugins instead of Dolphin's enabled list ([PreviewSettings] Plugins in dolphinrc).
//   SAVE=<dir>     write each preview to <dir>/<file name>.png.
//   SIZE=<px>      request this thumbnail size (default 256).
// Each result line shows the milliseconds since the previous result; KIO works through the files one at a time,
// so that is roughly the time the thumbnailer took for the file.
#include <QElapsedTimer>
#include <QGuiApplication>
#include <QPixmap>
#include <KFileItem>
#include <KIO/PreviewJob>
#include <KConfigGroup>
#include <KSharedConfig>
#include <KPluginMetaData>
#include <cstdio>

int main(int argc, char **argv) {
	QGuiApplication app(argc, argv);
	if (qEnvironmentVariableIsSet("LIST")) {
		for (auto &p : KIO::PreviewJob::availableThumbnailerPlugins())
			printf("%s=%s\n", qPrintable(p.pluginId()), qPrintable(p.mimeTypes().join(",").left(200)));
		return 0;
	}
	auto cfg = KSharedConfig::openConfig(QStringLiteral("dolphinrc"));
	QStringList plugins = cfg->group(QStringLiteral("PreviewSettings")).readEntry("Plugins", QStringList());
	if (qEnvironmentVariableIsSet("PLUGINS")) plugins = qEnvironmentVariable("PLUGINS").split(',');
	int size = qEnvironmentVariableIsSet("SIZE") ? qEnvironmentVariableIntValue("SIZE") : 256;
	KFileItemList items;
	for (int i = 1; i < argc; i++) items << KFileItem(QUrl::fromLocalFile(QString::fromLocal8Bit(argv[i])));
	auto job = KIO::filePreview(items, QSize(size, size), &plugins);
	job->setScaleType(KIO::PreviewJob::Unscaled);
	QElapsedTimer timer;
	timer.start();
	QObject::connect(job, &KIO::PreviewJob::gotPreview, [&timer](const KFileItem &it, const QPixmap &pm) {
		printf("OK   %4dx%-4d %6lldms %-22s %s\n", pm.width(), pm.height(), timer.restart(), qPrintable(it.mimetype()),
			qPrintable(it.localPath()));
		if (qEnvironmentVariableIsSet("SAVE")) pm.save(qEnvironmentVariable("SAVE") + "/" + it.name() + ".png");
	});
	QObject::connect(job, &KIO::PreviewJob::failed, [&timer](const KFileItem &it) {
		printf("FAIL           %6lldms %-22s %s\n", timer.restart(), qPrintable(it.mimetype()), qPrintable(it.localPath()));
	});
	QObject::connect(job, &KJob::result, &app, &QCoreApplication::quit);
	return app.exec();
}
