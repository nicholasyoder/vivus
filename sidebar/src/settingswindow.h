#pragma once

#include <QWidget>

#include <functional>

class DaemonClient;
class SettingsClient;
class QComboBox;
class QDoubleSpinBox;
class QLabel;
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
};
