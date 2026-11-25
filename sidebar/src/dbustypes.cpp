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

QDBusArgument &operator<<(QDBusArgument &arg, const Choice &c)
{
    arg.beginStructure();
    arg << c.id << c.label;
    arg.endStructure();
    return arg;
}

const QDBusArgument &operator>>(const QDBusArgument &arg, Choice &c)
{
    arg.beginStructure();
    arg >> c.id >> c.label;
    arg.endStructure();
    return arg;
}

void registerDBusTypes()
{
    qDBusRegisterMetaType<AudioDevice>();
    qDBusRegisterMetaType<AudioDeviceList>();
    qDBusRegisterMetaType<Choice>();
    qDBusRegisterMetaType<ChoiceList>();
}
