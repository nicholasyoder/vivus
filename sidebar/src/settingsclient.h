#pragma once

#include "dbustypes.h"

#include <QDBusPendingCall>
#include <QJsonObject>
#include <QObject>
#include <QVariant>

#include <functional>

class OrgVivusSettingsInterface;

// org.vivus.Settings: the daemon's config, edited by dotted path.
class SettingsClient : public QObject
{
    Q_OBJECT

public:
    explicit SettingsClient(QObject *parent = nullptr);

    // The effective config (defaults filled in), as of the last refresh.
    const QJsonObject &config() const { return m_config; }
    // e.g. value("audio.input"); null for unset options.
    QJsonValue value(const QString &path) const;
    const AudioDeviceList &devices() const { return m_devices; }

    // Re-fetches config and devices; emits configChanged and devicesChanged.
    void refresh();
    // Failures arrive as error().
    void set(const QString &path, const QVariant &value);
    void reset(const QString &path);
    // Asks for inputLevel signals for the next 5 s.
    void meterInput();
    // The input's PipeWire volume, 0 to 1 as mixers show it; negative while unknown.
    double inputGain() const { return m_inputGain; }
    void setInputGain(double gain);

    // Queries for the role pickers; `error` is empty on success.
    using ProvidersReply = std::function<void(const QStringList &providers, const QString &error)>;
    using ChoicesReply = std::function<void(const ChoiceList &choices, const QString &error)>;
    void listProviders(const QString &role, ProvidersReply done);
    // An empty provider (or model) is the role's default.
    void listModels(const QString &role, const QString &provider, ChoicesReply done);
    void listVoices(const QString &provider, const QString &model, ChoicesReply done);

signals:
    void configChanged();
    void devicesChanged();
    void inputLevel(double peak, bool clipped);
    void inputGainChanged();
    void error(const QString &path, const QString &message);

private:
    void fetchConfig();
    void watch(const QString &path, const QDBusPendingCall &call);

    OrgVivusSettingsInterface *m_iface;
    QJsonObject m_config;
    AudioDeviceList m_devices;
    double m_inputGain = -1;
};
