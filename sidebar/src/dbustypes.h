#pragma once

#include <QDBusArgument>
#include <QList>
#include <QMetaType>
#include <QString>

// org.vivus.Settings.ListAudioDevices entry, (ssbs).
struct AudioDevice {
    QString name, description;
    bool isDefault = false;
    QString direction; // source | sink
};
using AudioDeviceList = QList<AudioDevice>;

// org.vivus.Settings.ListModels / ListVoices entry, (ss). An empty id is the default (unset).
struct Choice {
    QString id, label;
};
using ChoiceList = QList<Choice>;

QDBusArgument &operator<<(QDBusArgument &arg, const AudioDevice &d);
const QDBusArgument &operator>>(const QDBusArgument &arg, AudioDevice &d);
QDBusArgument &operator<<(QDBusArgument &arg, const Choice &c);
const QDBusArgument &operator>>(const QDBusArgument &arg, Choice &c);

// Call before using the generated interfaces.
void registerDBusTypes();

Q_DECLARE_METATYPE(AudioDevice)
Q_DECLARE_METATYPE(Choice)
