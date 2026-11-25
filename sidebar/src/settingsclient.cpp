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

template<typename T, typename F>
static void onReply(QObject *ctx, const QDBusPendingCall &call, F done)
{
    auto *w = new QDBusPendingCallWatcher(call, ctx);
    QObject::connect(w, &QDBusPendingCallWatcher::finished, ctx, [done](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        QDBusPendingReply<T> reply = *w;
        if (reply.isError())
            done(T(), reply.error().message());
        else
            done(reply.value(), QString());
    });
}

void SettingsClient::listProviders(const QString &role, ProvidersReply done)
{
    onReply<QStringList>(this, m_iface->ListProviders(role), std::move(done));
}

void SettingsClient::listModels(const QString &role, const QString &provider, ChoicesReply done)
{
    onReply<ChoiceList>(this, m_iface->ListModels(role, provider), std::move(done));
}

void SettingsClient::listVoices(const QString &provider, const QString &model, ChoicesReply done)
{
    onReply<ChoiceList>(this, m_iface->ListVoices(provider, model), std::move(done));
}

void SettingsClient::apiKeyStatus(const QString &provider, StatusReply done)
{
    onReply<QString>(this, m_iface->ApiKeyStatus(provider), std::move(done));
}

static void onDone(QObject *ctx, const QDBusPendingCall &call, SettingsClient::DoneReply done)
{
    auto *w = new QDBusPendingCallWatcher(call, ctx);
    QObject::connect(w, &QDBusPendingCallWatcher::finished, ctx, [done = std::move(done)](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        done(w->isError() ? w->error().message() : QString());
    });
}

void SettingsClient::setApiKey(const QString &provider, const QString &key, DoneReply done)
{
    onDone(this, m_iface->SetApiKey(provider, key), std::move(done));
}

void SettingsClient::set(const QString &path, const QVariant &value, DoneReply done)
{
    onDone(this, m_iface->Set(path, QDBusVariant(value)), std::move(done));
}

void SettingsClient::reset(const QString &path, DoneReply done)
{
    onDone(this, m_iface->Reset(path), std::move(done));
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
