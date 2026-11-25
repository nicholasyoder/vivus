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

QDBusArgument &operator<<(QDBusArgument &arg, const AudioDevice &d);
const QDBusArgument &operator>>(const QDBusArgument &arg, AudioDevice &d);

// Call before using the generated interfaces.
void registerDBusTypes();

Q_DECLARE_METATYPE(AudioDevice)
