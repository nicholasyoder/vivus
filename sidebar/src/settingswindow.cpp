#include "settingswindow.h"
#include "daemonclient.h"
#include "settingsclient.h"

#include <QComboBox>
#include <QDesktopServices>
#include <QDir>
#include <QDoubleSpinBox>
#include <QFormLayout>
#include <QGroupBox>
#include <QHBoxLayout>
#include <QLabel>
#include <QProgressBar>
#include <QPushButton>
#include <QSlider>
#include <QStandardPaths>
#include <QStyle>
#include <QTimer>
#include <QUrl>
#include <QVBoxLayout>

#include <cmath>
#include <tuple>

static constexpr double MeterFloorDb = -60;
// Per 50 ms report, so a peak falls back over about a second.
static constexpr double MeterFallDb = 3;

static void repolish(QWidget *w)
{
    w->style()->unpolish(w);
    w->style()->polish(w);
}

static QSlider *percentSlider(const QString &name, QWidget *parent)
{
    auto *s = new QSlider(Qt::Horizontal, parent);
    s->setObjectName(name);
    s->setRange(0, 100);
    s->setPageStep(10);
    return s;
}

SettingsWindow::SettingsWindow(DaemonClient *daemon, SettingsClient *settings, QWidget *parent)
    : QWidget(parent)
    , m_daemon(daemon)
    , m_settings(settings)
{
    setObjectName(QStringLiteral("VivusSettings"));
    setWindowTitle(tr("Vivus Settings"));
    // Closing it while the sidebar is hidden mustn't quit the app.
    setAttribute(Qt::WA_QuitOnClose, false);
    resize(520, 0);

    m_form = new QWidget(this);

    // Microphone
    m_input = new QComboBox(m_form);
    m_input->setObjectName(QStringLiteral("InputDevice"));
    m_input->setSizeAdjustPolicy(QComboBox::AdjustToMinimumContentsLengthWithIcon);

    m_level = new QProgressBar(m_form);
    m_level->setObjectName(QStringLiteral("InputLevel"));
    m_level->setRange(int(MeterFloorDb * 10), 0);
    m_level->setValue(int(MeterFloorDb * 10));
    m_level->setTextVisible(false);
    m_level->setToolTip(tr("What speech recognition hears, after echo cancellation"));
    m_clip = new QLabel(tr("CLIP"), m_form);
    m_clip->setObjectName(QStringLiteral("ClipIndicator"));
    m_clip->setProperty("clipped", false);
    m_clip->setToolTip(tr("Lights up when the input hits full scale: turn the mic's gain down"));
    m_clipHold = new QTimer(this);
    m_clipHold->setSingleShot(true);
    m_clipHold->setInterval(1500);
    connect(m_clipHold, &QTimer::timeout, this, [this] {
        m_clip->setProperty("clipped", false);
        repolish(m_clip);
    });
    auto *levelRow = new QHBoxLayout;
    levelRow->addWidget(m_level, 1);
    levelRow->addWidget(m_clip);

    m_silence = new QDoubleSpinBox(m_form);
    m_silence->setObjectName(QStringLiteral("EndOfTurnSilence"));
    m_silence->setRange(0.1, 3.0);
    m_silence->setSingleStep(0.05);
    m_silence->setDecimals(2);
    m_silence->setSuffix(tr(" s"));
    m_silence->setKeyboardTracking(false);
    m_silence->setToolTip(tr("In conversation mode, how long a pause ends what you're saying"));

    auto *mic = new QGroupBox(tr("Microphone"), m_form);
    auto *micForm = new QFormLayout(mic);
    micForm->addRow(tr("Input"), m_input);
    micForm->addRow(tr("Level"), levelRow);
    micForm->addRow(tr("End of turn after"), m_silence);

    // Speech
    m_output = new QComboBox(m_form);
    m_output->setObjectName(QStringLiteral("OutputDevice"));
    m_output->setSizeAdjustPolicy(QComboBox::AdjustToMinimumContentsLengthWithIcon);

    m_volume = percentSlider(QStringLiteral("SpeechVolume"), m_form);
    m_volumeLabel = new QLabel(m_form);
    m_volumeLabel->setMinimumWidth(40);
    auto *volumeRow = new QHBoxLayout;
    volumeRow->addWidget(m_volume, 1);
    volumeRow->addWidget(m_volumeLabel);

    m_duck = percentSlider(QStringLiteral("DuckVolume"), m_form);
    m_duck->setToolTip(tr("Other apps' volume while Vivus speaks or you talk, relative to their own. "
                          "100% leaves them alone."));
    m_duckLabel = new QLabel(m_form);
    m_duckLabel->setMinimumWidth(40);
    auto *duckRow = new QHBoxLayout;
    duckRow->addWidget(m_duck, 1);
    duckRow->addWidget(m_duckLabel);

    auto *speech = new QGroupBox(tr("Speech"), m_form);
    auto *speechForm = new QFormLayout(speech);
    speechForm->addRow(tr("Output"), m_output);
    speechForm->addRow(tr("Volume"), volumeRow);
    speechForm->addRow(tr("Other apps while talking"), duckRow);

    // One label column across both groups.
    int labelWidth = 0;
    for (auto *f : {micForm, speechForm})
        for (int row = 0; row < f->rowCount(); ++row)
            if (auto *item = f->itemAt(row, QFormLayout::LabelRole))
                labelWidth = std::max(labelWidth, item->widget()->sizeHint().width());
    for (auto *f : {micForm, speechForm})
        for (int row = 0; row < f->rowCount(); ++row)
            if (auto *item = f->itemAt(row, QFormLayout::LabelRole))
                item->widget()->setMinimumWidth(labelWidth);

    auto *formLayout = new QVBoxLayout(m_form);
    formLayout->setContentsMargins(0, 0, 0, 0);
    formLayout->addWidget(mic);
    formLayout->addWidget(speech);

    m_status = new QLabel(this);
    m_status->setObjectName(QStringLiteral("SettingsStatus"));
    m_status->setWordWrap(true);
    m_status->hide();

    auto *openFile = new QPushButton(tr("Open config file"), this);
    openFile->setObjectName(QStringLiteral("OpenConfigButton"));
    openFile->setToolTip(tr("Everything else is set in ~/.config/vivus/config.toml"));
    connect(openFile, &QPushButton::clicked, this, [] {
        const QString path = QDir(QStandardPaths::writableLocation(QStandardPaths::GenericConfigLocation))
                                 .filePath(QStringLiteral("vivus/config.toml"));
        QDesktopServices::openUrl(QUrl::fromLocalFile(path));
    });
    auto *footer = new QHBoxLayout;
    footer->addStretch(1);
    footer->addWidget(openFile);

    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(16, 16, 16, 16);
    layout->setSpacing(12);
    layout->addWidget(m_form);
    layout->addWidget(m_status);
    layout->addStretch(1);
    layout->addLayout(footer);

    // Edits
    connect(m_input, &QComboBox::activated, this, [this] { onDeviceActivated(m_input, QStringLiteral("audio.input")); });
    connect(m_output, &QComboBox::activated, this, [this] { onDeviceActivated(m_output, QStringLiteral("audio.output")); });

    auto *silenceCommit = debounce([this] {
        m_settings->set(QStringLiteral("audio.end_of_turn_silence"), m_silence->value());
    });
    connect(m_silence, &QDoubleSpinBox::valueChanged, silenceCommit, qOverload<>(&QTimer::start));

    auto *volumeCommit = debounce([this] {
        m_settings->set(QStringLiteral("audio.speech_volume"), m_volume->value() / 100.0);
    });
    connect(m_volume, &QSlider::valueChanged, this, [this, volumeCommit](int v) {
        m_volumeLabel->setText(tr("%1%").arg(v));
        volumeCommit->start();
    });
    auto *duckCommit = debounce([this] {
        m_settings->set(QStringLiteral("audio.duck_volume"), m_duck->value() / 100.0);
    });
    connect(m_duck, &QSlider::valueChanged, this, [this, duckCommit](int v) {
        m_duckLabel->setText(tr("%1%").arg(v));
        duckCommit->start();
    });

    // Daemon
    connect(m_settings, &SettingsClient::configChanged, this, &SettingsWindow::updateConfig);
    connect(m_settings, &SettingsClient::devicesChanged, this, &SettingsWindow::updateDevices);
    connect(m_settings, &SettingsClient::inputLevel, this, &SettingsWindow::showLevel);
    connect(m_settings, &SettingsClient::error, this, &SettingsWindow::showError);
    connect(m_daemon, &DaemonClient::connectedChanged, this, &SettingsWindow::updateConnected);

    // Well inside the daemon's 5 s lease.
    m_meterLease = new QTimer(this);
    m_meterLease->setInterval(2000);
    connect(m_meterLease, &QTimer::timeout, m_settings, &SettingsClient::meterInput);

    updateConnected();
}

QTimer *SettingsWindow::debounce(std::function<void()> commit)
{
    auto *t = new QTimer(this);
    t->setSingleShot(true);
    t->setInterval(250);
    connect(t, &QTimer::timeout, this, std::move(commit));
    return t;
}

void SettingsWindow::showEvent(QShowEvent *event)
{
    QWidget::showEvent(event);
    if (m_daemon->connected()) {
        m_settings->refresh();
        m_settings->meterInput();
    }
    m_meterLease->start();
}

void SettingsWindow::hideEvent(QHideEvent *event)
{
    QWidget::hideEvent(event);
    m_meterLease->stop(); // the daemon stops metering when the lease runs out
}

void SettingsWindow::updateConnected()
{
    const bool connected = m_daemon->connected();
    m_form->setEnabled(connected);
    if (connected) {
        m_status->hide();
        if (isVisible()) {
            m_settings->refresh();
            m_settings->meterInput();
        }
    } else {
        m_status->setText(tr("The daemon isn't running, so settings can't be changed here."));
        m_status->show();
        showLevel(0, false);
    }
}

void SettingsWindow::updateConfig()
{
    const auto num = [this](const char *path) { return m_settings->value(QLatin1String(path)).toDouble(); };
    {
        const QSignalBlocker b(m_silence);
        m_silence->setValue(num("audio.end_of_turn_silence"));
    }
    // Leave a slider alone while it's being dragged or its edit is pending.
    for (auto [slider, label, path] : {std::tuple{m_volume, m_volumeLabel, "audio.speech_volume"},
                                       std::tuple{m_duck, m_duckLabel, "audio.duck_volume"}}) {
        if (slider->isSliderDown())
            continue;
        const int v = int(std::lround(num(path) * 100));
        const QSignalBlocker b(slider);
        slider->setValue(v);
        label->setText(tr("%1%").arg(v));
    }
    updateDevices();
}

void SettingsWindow::updateDevices()
{
    fillDevices(m_input, QStringLiteral("source"), QStringLiteral("audio.input"));
    fillDevices(m_output, QStringLiteral("sink"), QStringLiteral("audio.output"));
}

// "System default" is the unset value; a configured device that's unplugged stays listed.
void SettingsWindow::fillDevices(QComboBox *combo, const QString &direction, const QString &path)
{
    const QString chosen = m_settings->value(path).toString();
    const QSignalBlocker b(combo);
    combo->clear();

    QString defaultName;
    for (const auto &d : m_settings->devices())
        if (d.direction == direction && d.isDefault)
            defaultName = d.description;
    combo->addItem(defaultName.isEmpty() ? tr("System default") : tr("System default (%1)").arg(defaultName),
                   QVariant());

    bool found = chosen.isEmpty();
    for (const auto &d : m_settings->devices()) {
        if (d.direction != direction)
            continue;
        combo->addItem(d.description, d.name);
        combo->setItemData(combo->count() - 1, d.name, Qt::ToolTipRole);
        found |= d.name == chosen;
    }
    if (!found)
        combo->addItem(tr("%1 (not connected)").arg(chosen), chosen);
    combo->setCurrentIndex(chosen.isEmpty() ? 0 : combo->findData(chosen));
}

void SettingsWindow::onDeviceActivated(QComboBox *combo, const QString &path)
{
    const QVariant name = combo->currentData();
    if (name.isNull())
        m_settings->reset(path);
    else
        m_settings->set(path, name.toString());
}

void SettingsWindow::showLevel(double peak, bool clipped)
{
    const double db = peak > 0 ? std::max(MeterFloorDb, 20 * std::log10(peak)) : MeterFloorDb;
    m_levelDb = std::max(db, m_levelDb - MeterFallDb);
    m_level->setValue(int(m_levelDb * 10));
    if (clipped) {
        m_clip->setProperty("clipped", true);
        repolish(m_clip);
        m_clipHold->start();
    }
}

void SettingsWindow::showError(const QString &path, const QString &message)
{
    m_status->setText(tr("Couldn't set %1: %2").arg(path, message));
    m_status->show();
}
