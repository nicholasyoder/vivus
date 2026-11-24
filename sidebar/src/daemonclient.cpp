#include "daemonclient.h"
#include "vivus_interface.h"

#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QDBusPendingCallWatcher>
#include <QDBusServiceWatcher>

static const QString Service = QStringLiteral("org.vivus");
static const QString Path = QStringLiteral("/org/vivus");

DaemonClient::DaemonClient(QObject *parent)
    : QObject(parent)
{
    auto bus = QDBusConnection::sessionBus();
    m_assistant = new OrgVivusAssistantInterface(Service, Path, bus, this);
    m_documents = new OrgVivusDocumentsInterface(Service, Path, bus, this);

    connect(m_assistant, &OrgVivusAssistantInterface::SidebarVisibilityRequested,
            this, &DaemonClient::visibilityRequested);
    connect(m_assistant, &OrgVivusAssistantInterface::Transcript,
            this, &DaemonClient::transcript);
    connect(m_documents, &OrgVivusDocumentsInterface::DocumentChanged,
            this, &DaemonClient::onDocumentChanged);
    bus.connect(Service, Path, QStringLiteral("org.freedesktop.DBus.Properties"),
                QStringLiteral("PropertiesChanged"), this,
                SLOT(onPropertiesChanged(QString, QVariantMap, QStringList)));

    auto *watcher = new QDBusServiceWatcher(Service, bus,
        QDBusServiceWatcher::WatchForRegistration | QDBusServiceWatcher::WatchForUnregistration, this);
    connect(watcher, &QDBusServiceWatcher::serviceRegistered, this, [this] { setConnected(true); });
    connect(watcher, &QDBusServiceWatcher::serviceUnregistered, this, [this] { setConnected(false); });

    setConnected(bus.interface()->isServiceRegistered(Service));
}

QString DaemonClient::currentMarkdown() const
{
    return m_current >= 0 && m_current < m_docs.size() ? m_docs[m_current].markdown : QString();
}

void DaemonClient::selectDocument(int index)
{
    if (index < 0 || index >= m_docs.size() || index == m_current)
        return;
    m_current = index;
    emit currentChanged();
}

void DaemonClient::ask(const QString &text)
{
    if (!text.trimmed().isEmpty())
        m_assistant->Ask(text);
}

void DaemonClient::toggleListen() { m_assistant->ToggleListen(); }
void DaemonClient::cancel() { m_assistant->Cancel(); }

void DaemonClient::quitDaemon()
{
    if (m_connected)
        m_assistant->Quit().waitForFinished();
}

void DaemonClient::onPropertiesChanged(const QString &iface, const QVariantMap &changed, const QStringList &invalidated)
{
    if (iface != QLatin1String("org.vivus.Assistant"))
        return;
    const auto key = QStringLiteral("State");
    if (changed.contains(key))
        setState(changed.value(key).toString());
    else if (invalidated.contains(key))
        setState(m_assistant->state());
}

void DaemonClient::onDocumentChanged(const QString &id, const QString &title, const QString &markdown)
{
    auto it = std::find_if(m_docs.begin(), m_docs.end(), [&](const Document &d) { return d.id == id; });
    if (it == m_docs.end()) {
        m_docs.append({id, title, markdown});
        m_current = m_docs.size() - 1;
    } else {
        it->title = title;
        it->markdown = markdown;
        m_current = int(it - m_docs.begin());
    }
    emit documentsChanged();
    emit currentChanged();
}

void DaemonClient::setConnected(bool connected)
{
    if (connected)
        refresh();
    else
        setState(QStringLiteral("idle"));
    if (m_connected == connected)
        return;
    m_connected = connected;
    emit connectedChanged(connected);
}

void DaemonClient::setState(const QString &state)
{
    if (m_state == state)
        return;
    m_state = state;
    emit stateChanged(state);
}

// Re-fetch everything, e.g. after the daemon (re)starts. History lives in the daemon.
void DaemonClient::refresh()
{
    setState(m_assistant->state());

    auto *call = new QDBusPendingCallWatcher(m_documents->List(), this);
    connect(call, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        QDBusPendingReply<QStringList> ids = *w;
        if (ids.isError())
            return;
        m_docs.clear();
        for (const auto &id : ids.value()) {
            QDBusPendingReply<QString, QString> doc = m_documents->Get(id);
            doc.waitForFinished();
            if (!doc.isError())
                m_docs.append({id, doc.argumentAt<0>(), doc.argumentAt<1>()});
        }
        m_current = m_docs.size() - 1;
        emit documentsChanged();
        emit currentChanged();
    });
}
