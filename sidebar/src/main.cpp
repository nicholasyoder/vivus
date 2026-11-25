#include "daemonclient.h"
#include "dbustypes.h"
#include "settingsclient.h"
#include "settingswindow.h"
#include "sidebarwindow.h"

#include <QApplication>
#include <QCommandLineParser>
#include <QDir>
#include <QFile>
#include <QStandardPaths>

// Built-in style, replaced by ~/.config/vivus/sidebar.qss when present.
// Under Forest, its app-wide QSS can target the same object names instead.
static QString loadStyleSheet()
{
    const QString user = QDir(QStandardPaths::writableLocation(QStandardPaths::GenericConfigLocation))
                             .filePath(QStringLiteral("vivus/sidebar.qss"));
    for (const QString &path : {user, QStringLiteral(":/vivus/sidebar.qss")}) {
        QFile f(path);
        if (f.open(QIODevice::ReadOnly))
            return QString::fromUtf8(f.readAll());
    }
    return {};
}

int main(int argc, char *argv[])
{
    QApplication app(argc, argv);
    registerDBusTypes();
    app.setApplicationName(QStringLiteral("vivus-sidebar"));
    // Also the Wayland app ID and the portal app ID the daemon registers.
    app.setDesktopFileName(QStringLiteral("org.vivus.Vivus"));

    QCommandLineParser parser;
    parser.addHelpOption();
    const QCommandLineOption showOpt(QStringLiteral("show"), QStringLiteral("Show the sidebar at startup."));
    const QCommandLineOption noStyleOpt(QStringLiteral("no-style"),
                                        QStringLiteral("Don't apply vivus' stylesheet (use the host theme)."));
    const QCommandLineOption settingsOpt(QStringLiteral("settings"), QStringLiteral("Open the settings window."));
    parser.addOptions({showOpt, noStyleOpt, settingsOpt});
    parser.process(app);

    if (!parser.isSet(noStyleOpt))
        app.setStyleSheet(loadStyleSheet());

    DaemonClient client;
    SidebarWindow window(&client);
    window.setupLayerShell();
    if (parser.isSet(showOpt))
        window.show();

    SettingsClient settingsClient;
    SettingsWindow settings(&client, &settingsClient);
    QObject::connect(&window, &SidebarWindow::settingsRequested, &settings, [&settings] {
        settings.show();
        settings.raise();
        settings.activateWindow();
    });
    if (parser.isSet(settingsOpt))
        settings.show();

    return app.exec();
}
