#include "dbustypes.h"

#include <QDBusMetaType>

QDBusArgument &operator<<(QDBusArgument &arg, const AudioDevice &d)
{
    arg.beginStructure();
    arg << d.name << d.description << d.isDefault << d.direction;
    arg.endStructure();
    return arg;
}

const QDBusArgument &operator>>(const QDBusArgument &arg, AudioDevice &d)
{
    arg.beginStructure();
    arg >> d.name >> d.description >> d.isDefault >> d.direction;
    arg.endStructure();
    return arg;
}

void registerDBusTypes()
{
    qDBusRegisterMetaType<AudioDevice>();
    qDBusRegisterMetaType<AudioDeviceList>();
}
