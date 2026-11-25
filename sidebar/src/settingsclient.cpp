#include "settingsclient.h"
#include "vivus_interface.h"

#include <QDBusConnection>
#include <QDBusPendingCallWatcher>
#include <QJsonDocument>

SettingsClient::SettingsClient(QObject *parent)
    : QObject(parent)
{
    m_iface = new OrgVivusSettingsInterface(QStringLiteral("org.vivus"), QStringLiteral("/org/vivus"),
                                            QDBusConnection::sessionBus(), this);
    connect(m_iface, &OrgVivusSettingsInterface::Changed, this, &SettingsClient::fetchConfig);
    connect(m_iface, &OrgVivusSettingsInterface::AudioDevicesChanged, this, [this](const AudioDeviceList &list) {
        m_devices = list;
        emit devicesChanged();
    });
    connect(m_iface, &OrgVivusSettingsInterface::InputLevel, this, &SettingsClient::inputLevel);
    connect(m_iface, &OrgVivusSettingsInterface::InputGainChanged, this, [this](double gain) {
        m_inputGain = gain;
        emit inputGainChanged();
    });
}

QJsonValue SettingsClient::value(const QString &path) const
{
    QJsonValue v = m_config;
    for (const auto &key : path.split(QLatin1Char('.')))
        v = v.toObject().value(key);
    return v;
}

void SettingsClient::refresh()
{
    fetchConfig();
    auto *w = new QDBusPendingCallWatcher(m_iface->ListAudioDevices(), this);
    connect(w, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        QDBusPendingReply<AudioDeviceList> reply = *w;
        if (reply.isError())
            return;
        m_devices = reply.value();
        emit devicesChanged();
    });
    auto *g = new QDBusPendingCallWatcher(m_iface->GetInputGain(), this);
    connect(g, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        QDBusPendingReply<double> reply = *w;
        if (reply.isError())
            return;
        m_inputGain = reply.value();
        emit inputGainChanged();
    });
}

void SettingsClient::fetchConfig()
{
    auto *w = new QDBusPendingCallWatcher(m_iface->Get(), this);
    connect(w, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        QDBusPendingReply<QString> reply = *w;
        if (reply.isError())
            return;
        m_config = QJsonDocument::fromJson(reply.value().toUtf8()).object();
        emit configChanged();
    });
}

void SettingsClient::set(const QString &path, const QVariant &value)
{
    watch(path, m_iface->Set(path, QDBusVariant(value)));
}

void SettingsClient::reset(const QString &path)
{
    watch(path, m_iface->Reset(path));
}

void SettingsClient::setInputGain(double gain)
{
    // Not a config key; a refusal still reports under a readable name.
    watch(QStringLiteral("input gain"), m_iface->SetInputGain(gain));
}

void SettingsClient::meterInput()
{
    m_iface->MeterInput();
}

void SettingsClient::watch(const QString &path, const QDBusPendingCall &call)
{
    auto *w = new QDBusPendingCallWatcher(call, this);
    connect(w, &QDBusPendingCallWatcher::finished, this, [this, path](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        if (w->isError()) {
            emit error(path, w->error().message());
            fetchConfig(); // put the widget back to what's in effect
        }
    });
}
