#pragma once

#include "dbustypes.h"

#include <QWidget>

#include <functional>
#include <vector>

class DaemonClient;
class SettingsClient;
class QComboBox;
class QDoubleSpinBox;
class QFormLayout;
class QGroupBox;
class QLabel;
class QLineEdit;
class QPushButton;
class QProgressBar;
class QSlider;
class QTimer;

// A normal toplevel (not layer-shell): a form wants more room than the
// sidebar has, and should stay open while you try things. Object names are
// the QSS surface; see data/sidebar.qss.
class SettingsWindow : public QWidget
{
    Q_OBJECT

public:
    SettingsWindow(DaemonClient *daemon, SettingsClient *settings, QWidget *parent = nullptr);

protected:
    void showEvent(QShowEvent *event) override;
    void hideEvent(QHideEvent *event) override;

private:
    // One [roles.*] table: provider and model pickers, plus the voice for tts.
    struct RoleRow {
        QString role;
        QComboBox *provider = nullptr;
        QComboBox *model = nullptr;
        QComboBox *voice = nullptr;
        // What the model list was fetched for, or null when it needs fetching.
        QString modelsFor;
        ChoiceList models;
        QString modelsError;
        QString voicesFor;
        ChoiceList voices;
    };

    QWidget *addRole(const QString &role, const QString &tooltip);
    void updateRoles();
    void fillProviders(RoleRow &row, const QStringList &providers);
    void syncModels(RoleRow &row);
    void fillModels(RoleRow &row);
    void syncVoices(RoleRow &row);
    void fillVoices(RoleRow &row);
    void onProviderActivated(RoleRow &row);
    void commitModel(RoleRow &row);
    void onVoiceActivated(RoleRow &row);
    QString roleValue(const RoleRow &row, const char *key) const;
    RoleRow *findRole(const QString &role);

    // One openai-compatible provider's key: write-only, so a masked field to
    // paste a new one, with where the current one lives as its placeholder.
    struct KeyRow {
        QString provider;
        QLineEdit *edit = nullptr;
        QPushButton *save = nullptr;
        QPushButton *remove = nullptr;
    };

    void updateKeys();
    void updateKeyStatus(const QString &provider);
    void saveKey(const QString &provider, const QString &key);
    KeyRow *findKey(const QString &provider);
    void alignLabels();

    void updateConnected();
    void updateConfig();
    void updateDevices();
    void fillDevices(QComboBox *combo, const QString &direction, const QString &path);
    void onDeviceActivated(QComboBox *combo, const QString &path);
    void showLevel(double peak, bool clipped);
    void updateGain();
    void showError(const QString &path, const QString &message);
    // A slider or spin box: label follows at once, the write waits for a pause.
    QTimer *debounce(std::function<void()> commit);

    DaemonClient *m_daemon;
    SettingsClient *m_settings;
    QWidget *m_form;
    QComboBox *m_input;
    QComboBox *m_output;
    QProgressBar *m_level;
    QTimer *m_clipHold;
    QSlider *m_gain;
    QLabel *m_gainLabel;
    QTimer *m_gainCommit;
    QTimer *m_meterLease;
    double m_levelDb = -60;
    QDoubleSpinBox *m_silence;
    QSlider *m_volume;
    QLabel *m_volumeLabel;
    QSlider *m_duck;
    QLabel *m_duckLabel;
    QLabel *m_status;
    std::vector<RoleRow> m_roles;
    QGroupBox *m_keysGroup;
    QFormLayout *m_keysForm;
    std::vector<KeyRow> m_keys;
    std::vector<QFormLayout *> m_forms;
};
