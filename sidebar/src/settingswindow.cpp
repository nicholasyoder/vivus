#include "settingswindow.h"
#include "daemonclient.h"
#include "settingsclient.h"

#include <QComboBox>
#include <QCompleter>
#include <QLineEdit>
#include <QDesktopServices>
#include <QDir>
#include <QDoubleSpinBox>
#include <QFormLayout>
#include <QGroupBox>
#include <QJsonObject>
#include <QHBoxLayout>
#include <QLabel>
#include <QMessageBox>
#include <QRegularExpression>
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
    resize(560, 0);

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
    m_level->setToolTip(tr("What speech recognition hears, after echo cancellation. "
                           "Turns red when it clips: turn the gain down."));
    m_level->setProperty("clipped", false);
    m_clipHold = new QTimer(this);
    m_clipHold->setSingleShot(true);
    m_clipHold->setInterval(1500);
    connect(m_clipHold, &QTimer::timeout, this, [this] {
        m_level->setProperty("clipped", false);
        repolish(m_level);
    });

    m_gain = percentSlider(QStringLiteral("InputGain"), m_form);
    m_gain->setToolTip(tr("The microphone's volume in PipeWire, the same one system mixers show. "
                          "Saved per device by the system, not in Vivus's config."));
    m_gainLabel = new QLabel(m_form);
    m_gainLabel->setMinimumWidth(40);
    auto *gainRow = new QHBoxLayout;
    gainRow->addWidget(m_gain, 1);
    gainRow->addWidget(m_gainLabel);

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
    micForm->addRow(tr("Level"), m_level);
    micForm->addRow(tr("Gain"), gainRow);
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

    // Roles: speech models, then the LLM roles.
    m_roles.reserve(6);
    auto *models = new QGroupBox(tr("Models"), m_form);
    auto *modelsForm = new QFormLayout(models);
    modelsForm->addRow(tr("Speech recognition"), addRole(QStringLiteral("stt"), tr("Turns what you say into text")));
    modelsForm->addRow(tr("Speech output"), addRole(QStringLiteral("tts"), tr("Speaks replies")));
    {
        auto &tts = m_roles.back();
        tts.voice = new QComboBox(m_form);
        tts.voice->setObjectName(QStringLiteral("TtsVoice"));
        tts.voice->setSizeAdjustPolicy(QComboBox::AdjustToMinimumContentsLengthWithIcon);
        connect(tts.voice, &QComboBox::activated, this, [this] { onVoiceActivated(*findRole(QStringLiteral("tts"))); });
        modelsForm->addRow(tr("Voice"), tts.voice);
    }
    modelsForm->addRow(tr("Quick replies"),
                       addRole(QStringLiteral("router"), tr("Answers every request first and decides when to hand off; "
                                                            "a fast model with tool calls")));
    modelsForm->addRow(tr("Longer answers"),
                       addRole(QStringLiteral("answer"), tr("Takes questions that need more thought or research")));
    modelsForm->addRow(tr("Tasks"),
                       addRole(QStringLiteral("agent"), tr("Multi-step work; uses the longer-answers model when unset")));
    modelsForm->addRow(tr("Vision"), addRole(QStringLiteral("vision"), tr("Not used yet: screen awareness comes later")));

    // Providers: one row each, then a row to add one (presets fill it in).
    m_keysGroup = new QGroupBox(tr("Providers"), m_form);
    m_keysGroup->setObjectName(QStringLiteral("Providers"));
    auto *providersLayout = new QVBoxLayout(m_keysGroup);
    m_keysForm = new QFormLayout;
    providersLayout->addLayout(m_keysForm);

    m_newPreset = new QComboBox(m_keysGroup);
    m_newPreset->setObjectName(QStringLiteral("NewProviderPreset"));
    // Data: kind, name, base URL.
    m_newPreset->addItem(tr("OpenAI-compatible"),
                         QStringList{QStringLiteral("openai-compatible"), QString(), QString()});
    m_newPreset->addItem(QStringLiteral("OpenAI"),
                         QStringList{QStringLiteral("openai-compatible"), QStringLiteral("openai"),
                                     QStringLiteral("https://api.openai.com/v1")});
    m_newPreset->addItem(QStringLiteral("Groq"),
                         QStringList{QStringLiteral("openai-compatible"), QStringLiteral("groq"),
                                     QStringLiteral("https://api.groq.com/openai/v1")});
    m_newPreset->addItem(QStringLiteral("NanoGPT"),
                         QStringList{QStringLiteral("openai-compatible"), QStringLiteral("nano-gpt"),
                                     QStringLiteral("https://nano-gpt.com/api/v1")});
    m_newPreset->addItem(tr("Claude Code"),
                         QStringList{QStringLiteral("claude-code"), QStringLiteral("claude-code"), QString()});
    m_newPreset->setItemData(4, tr("Runs `claude -p` for longer answers and tasks; uses your Claude login"),
                             Qt::ToolTipRole);
    m_newName = new QLineEdit(m_keysGroup);
    m_newName->setObjectName(QStringLiteral("NewProviderName"));
    m_newName->setPlaceholderText(tr("Name"));
    m_newName->setMaxLength(32);
    m_newUrl = new QLineEdit(m_keysGroup);
    m_newUrl->setObjectName(QStringLiteral("NewProviderUrl"));
    m_newUrl->setPlaceholderText(tr("Base URL, e.g. https://api.example.com/v1"));
    m_newUrl->setToolTip(tr("The URL its /chat/completions and /models live under"));
    m_newKey = new QLineEdit(m_keysGroup);
    m_newKey->setObjectName(QStringLiteral("NewProviderKey"));
    m_newKey->setEchoMode(QLineEdit::Password);
    m_newKey->setPlaceholderText(tr("API key (optional)"));
    m_addProvider = new QPushButton(tr("Add"), m_keysGroup);
    m_addProvider->setObjectName(QStringLiteral("AddProviderButton"));
    m_addProvider->setEnabled(false);
    m_addProvider->setSizePolicy(QSizePolicy::Fixed, QSizePolicy::Fixed);
    auto *addTop = new QHBoxLayout;
    addTop->addWidget(m_newPreset);
    addTop->addWidget(m_newName, 1);
    auto *addBottom = new QHBoxLayout;
    addBottom->addWidget(m_newKey, 1);
    addBottom->addStretch(0); // keeps Add right-aligned when the key field is hidden
    addBottom->addWidget(m_addProvider);
    auto *addBox = new QVBoxLayout;
    addBox->addLayout(addTop);
    addBox->addWidget(m_newUrl);
    addBox->addLayout(addBottom);
    auto *addForm = new QFormLayout;
    addForm->addRow(tr("Add provider"), addBox);
    providersLayout->addLayout(addForm);
    for (auto *edit : {m_newName, m_newUrl, m_newKey}) {
        connect(edit, &QLineEdit::textChanged, this, &SettingsWindow::updateAddProvider);
        connect(edit, &QLineEdit::returnPressed, this, &SettingsWindow::addProvider);
    }
    connect(m_addProvider, &QPushButton::clicked, this, &SettingsWindow::addProvider);
    connect(m_newPreset, &QComboBox::currentIndexChanged, this, &SettingsWindow::onPresetChanged);

    m_forms = {micForm, speechForm, modelsForm, m_keysForm, addForm};
    alignLabels();

    auto *formLayout = new QVBoxLayout(m_form);
    formLayout->setContentsMargins(0, 0, 0, 0);
    formLayout->addWidget(mic);
    formLayout->addWidget(speech);
    formLayout->addWidget(models);
    formLayout->addWidget(m_keysGroup);

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

    // Applies as you drag (a short pause), so you can watch the meter.
    m_gainCommit = debounce([this] { m_settings->setInputGain(m_gain->value() / 100.0); });
    m_gainCommit->setInterval(100);
    connect(m_gain, &QSlider::valueChanged, this, [this](int v) {
        m_gainLabel->setText(tr("%1%").arg(v));
        m_gainCommit->start();
    });

    // Daemon
    connect(m_settings, &SettingsClient::inputGainChanged, this, &SettingsWindow::updateGain);
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

// One label column across all groups.
void SettingsWindow::alignLabels()
{
    int labelWidth = 0;
    for (auto *f : m_forms)
        for (int row = 0; row < f->rowCount(); ++row)
            if (auto *item = f->itemAt(row, QFormLayout::LabelRole)) {
                item->widget()->setMinimumWidth(0);
                labelWidth = std::max(labelWidth, item->widget()->sizeHint().width());
            }
    for (auto *f : m_forms)
        for (int row = 0; row < f->rowCount(); ++row)
            if (auto *item = f->itemAt(row, QFormLayout::LabelRole))
                item->widget()->setMinimumWidth(labelWidth);
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
    // Lists may have changed while hidden (downloads, a provider's catalog).
    for (auto &row : m_roles) {
        row.modelsFor.clear();
        row.voicesFor.clear();
    }
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
    updateRoles();
    updateKeys();
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
        if (!m_clipHold->isActive()) {
            m_level->setProperty("clipped", true);
            repolish(m_level);
        }
        m_clipHold->start();
    }
}

// Follows changes from anywhere (a mixer, another app's auto gain), except mid-drag.
void SettingsWindow::updateGain()
{
    const double gain = m_settings->inputGain();
    m_gain->setEnabled(gain >= 0);
    if (gain < 0) {
        m_gainLabel->setText(tr("–"));
        return;
    }
    if (m_gain->isSliderDown() || m_gainCommit->isActive())
        return;
    const int v = int(std::lround(gain * 100));
    const QSignalBlocker b(m_gain);
    m_gain->setValue(v);
    m_gainLabel->setText(tr("%1%").arg(v));
}

void SettingsWindow::showError(const QString &path, const QString &message)
{
    m_status->setText(tr("Couldn't set %1: %2").arg(path, message));
    m_status->show();
}

// Object names: SttProvider, SttModel, RouterProvider, ...
QWidget *SettingsWindow::addRole(const QString &role, const QString &tooltip)
{
    const QString prefix = role.left(1).toUpper() + role.mid(1);
    RoleRow row;
    row.role = role;
    row.provider = new QComboBox(m_form);
    row.provider->setObjectName(prefix + QStringLiteral("Provider"));
    row.provider->setToolTip(tooltip);
    // One width for all, so the model pickers line up.
    row.provider->setSizeAdjustPolicy(QComboBox::AdjustToMinimumContentsLengthWithIcon);
    row.provider->setMinimumContentsLength(11);
    row.model = new QComboBox(m_form);
    row.model->setObjectName(prefix + QStringLiteral("Model"));
    row.model->setToolTip(tooltip);
    row.model->setSizeAdjustPolicy(QComboBox::AdjustToMinimumContentsLengthWithIcon);
    row.model->setMinimumContentsLength(12);
    row.model->setInsertPolicy(QComboBox::NoInsert);
    m_roles.push_back(row);

    connect(row.provider, &QComboBox::activated, this, [this, role] { onProviderActivated(*findRole(role)); });
    connect(row.model, &QComboBox::activated, this, [this, role] { commitModel(*findRole(role)); });

    auto *box = new QWidget(m_form);
    auto *layout = new QHBoxLayout(box);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->addWidget(row.provider);
    layout->addWidget(row.model, 1);
    return box;
}

SettingsWindow::RoleRow *SettingsWindow::findRole(const QString &role)
{
    for (auto &row : m_roles)
        if (row.role == role)
            return &row;
    return nullptr;
}

// roles.<role>.<key>; empty when unset.
QString SettingsWindow::roleValue(const RoleRow &row, const char *key) const
{
    return m_settings->value(QStringLiteral("roles.%1.%2").arg(row.role, QLatin1String(key))).toString();
}

static bool isSpeechRole(const QString &role)
{
    return role == QLatin1String("stt") || role == QLatin1String("tts");
}

void SettingsWindow::updateRoles()
{
    for (auto &row : m_roles) {
        const QString role = row.role;
        m_settings->listProviders(role, [this, role](const QStringList &providers, const QString &) {
            if (auto *row = findRole(role))
                fillProviders(*row, providers);
        });
        syncModels(row);
    }
}

// The first entry is the unset role: local speech for stt/tts, nothing for the rest.
void SettingsWindow::fillProviders(RoleRow &row, const QStringList &providers)
{
    const QString chosen = roleValue(row, "provider");
    const QSignalBlocker b(row.provider);
    row.provider->clear();
    row.provider->addItem(isSpeechRole(row.role) ? tr("Default (local)") : tr("None"), QVariant());
    for (const auto &p : providers)
        row.provider->addItem(providerLabel(p), p);
    if (!chosen.isEmpty() && row.provider->findData(chosen) < 0)
        row.provider->addItem(tr("%1 (can't serve this)").arg(chosen), chosen);
    row.provider->setCurrentIndex(chosen.isEmpty() ? 0 : row.provider->findData(chosen));
}

// Re-lists models only when the provider changed (or the window reopened).
void SettingsWindow::syncModels(RoleRow &row)
{
    const QString provider = roleValue(row, "provider");
    if (provider.isEmpty() && !isSpeechRole(row.role)) {
        row.modelsFor.clear();
        row.models.clear();
        row.modelsError.clear();
        fillModels(row);
        return;
    }
    // Never null once requested, so "" (the default provider) counts as listed.
    const QString key = provider.isNull() ? QStringLiteral("") : provider;
    if (row.modelsFor.isNull() || row.modelsFor != key) {
        row.modelsFor = key;
        const QString role = row.role;
        m_settings->listModels(role, key, [this, role, key](const ChoiceList &models, const QString &error) {
            auto *row = findRole(role);
            if (!row || row->modelsFor != key)
                return; // the provider changed meanwhile
            row->models = models;
            row->modelsError = error;
            fillModels(*row);
        });
    }
    fillModels(row);
}

void SettingsWindow::fillModels(RoleRow &row)
{
    const QString provider = roleValue(row, "provider");
    const QString chosen = roleValue(row, "model");
    const bool none = provider.isEmpty() && !isSpeechRole(row.role);
    // Remote catalogs are long and may lack a model you can still use: type to search or enter one.
    const QString kind = m_settings->value(QStringLiteral("providers.%1.kind").arg(provider)).toString();
    const bool editable = kind == QLatin1String("openai-compatible");

    const QSignalBlocker b(row.model);
    row.model->clear();
    row.model->setEnabled(!none);
    if (row.model->isEditable() != editable) {
        row.model->setEditable(editable);
        if (editable) {
            row.model->completer()->setFilterMode(Qt::MatchContains);
            row.model->completer()->setCompletionMode(QCompleter::PopupCompletion);
            const QString role = row.role;
            connect(row.model->lineEdit(), &QLineEdit::editingFinished, this,
                    [this, role] { commitModel(*findRole(role)); });
        }
    }
    if (none)
        return;
    for (const auto &m : row.models)
        row.model->addItem(m.label, m.id.isEmpty() ? QVariant() : QVariant(m.id));
    if (!chosen.isEmpty() && row.model->findData(chosen) < 0)
        row.model->addItem(editable ? chosen : tr("%1 (not available)").arg(chosen), chosen);
    row.model->setToolTip(row.modelsError.isEmpty() ? QString() : tr("Couldn't list models: %1").arg(row.modelsError));
    if (editable) {
        row.model->lineEdit()->setPlaceholderText(tr("Choose or type a model"));
        row.model->setCurrentIndex(chosen.isEmpty() ? -1 : row.model->findData(chosen));
        if (chosen.isEmpty())
            row.model->setEditText(QString());
        row.model->lineEdit()->setCursorPosition(0); // a long id shows its start
    } else {
        const int i = chosen.isEmpty() ? row.model->findData(QVariant()) : row.model->findData(chosen);
        row.model->setCurrentIndex(std::max(i, 0));
    }
    if (row.voice)
        syncVoices(row);
}

void SettingsWindow::syncVoices(RoleRow &row)
{
    const QString key = roleValue(row, "provider") + QLatin1Char('/') + roleValue(row, "model");
    if (row.voicesFor != key) {
        row.voicesFor = key;
        const QString role = row.role;
        m_settings->listVoices(roleValue(row, "provider"), roleValue(row, "model"),
                               [this, role, key](const ChoiceList &voices, const QString &) {
                                   auto *row = findRole(role);
                                   if (!row || row->voicesFor != key)
                                       return;
                                   row->voices = voices;
                                   fillVoices(*row);
                               });
    }
    fillVoices(row);
}

void SettingsWindow::fillVoices(RoleRow &row)
{
    const QString chosen = roleValue(row, "voice");
    const QSignalBlocker b(row.voice);
    row.voice->clear();
    for (const auto &v : row.voices)
        row.voice->addItem(v.label, v.id.isEmpty() ? QVariant() : QVariant(v.id));
    if (!chosen.isEmpty() && row.voice->findData(chosen) < 0)
        row.voice->addItem(tr("%1 (unknown)").arg(chosen), chosen);
    const int i = chosen.isEmpty() ? row.voice->findData(QVariant()) : row.voice->findData(chosen);
    row.voice->setCurrentIndex(std::max(i, 0));
}

// The old model (and voice) belong to the old provider, so they go first; each
// step leaves a config the daemon accepts.
void SettingsWindow::onProviderActivated(RoleRow &row)
{
    const QVariant provider = row.provider->currentData();
    if (provider.toString() == roleValue(row, "provider"))
        return;
    const QString table = QStringLiteral("roles.") + row.role;
    if (provider.isNull()) {
        m_settings->reset(table);
        return;
    }
    if (row.voice)
        m_settings->reset(table + QStringLiteral(".voice"));
    m_settings->reset(table + QStringLiteral(".model"));
    m_settings->set(table + QStringLiteral(".provider"), provider.toString());
}

// From the list, or typed into an editable picker.
void SettingsWindow::commitModel(RoleRow &row)
{
    QVariant model = row.model->currentData();
    if (row.model->isEditable()) {
        const QString text = row.model->currentText().trimmed();
        const int i = row.model->findText(text);
        model = i >= 0 ? row.model->itemData(i) : QVariant(text);
        if (text.isEmpty())
            return; // the role needs a model; leave it as it is
    }
    if (model.toString() == roleValue(row, "model"))
        return;
    const QString table = QStringLiteral("roles.") + row.role;
    // A voice belongs to its model.
    if (row.voice)
        m_settings->reset(table + QStringLiteral(".voice"));
    if (model.isNull())
        m_settings->reset(table + QStringLiteral(".model"));
    else
        m_settings->set(table + QStringLiteral(".model"), model.toString());
}

void SettingsWindow::onVoiceActivated(RoleRow &row)
{
    const QVariant voice = row.voice->currentData();
    if (voice.toString() == roleValue(row, "voice"))
        return;
    if (voice.isNull())
        m_settings->reset(QStringLiteral("roles.tts.voice"));
    else
        m_settings->set(QStringLiteral("roles.tts.voice"), voice.toString());
}

static QString kindOf(const QJsonObject &providers, const QString &name)
{
    return providers.value(name).toObject().value(QLatin1String("kind")).toString();
}

// Rebuilt only when the set of providers changes, so a half-typed key survives other edits.
void SettingsWindow::updateKeys()
{
    const QJsonObject all = m_settings->value(QStringLiteral("providers")).toObject();
    const QStringList providers = all.keys();
    QStringList keyed; // the ones that take a key
    for (const auto &p : providers)
        if (kindOf(all, p) == QLatin1String("openai-compatible"))
            keyed << p;

    QStringList shown, shownKeyed;
    for (const auto &k : m_keys) {
        shown << k.provider;
        if (k.edit)
            shownKeyed << k.provider;
    }
    if (providers != shown || keyed != shownKeyed) {
        while (m_keysForm->rowCount() > 0)
            m_keysForm->removeRow(0);
        m_keys.clear();
        for (const auto &provider : providers) {
            KeyRow row;
            row.provider = provider;
            row.url = new QLabel(m_keysGroup);
            row.url->setObjectName(QStringLiteral("ProviderUrl"));
            row.url->setTextInteractionFlags(Qt::TextSelectableByMouse);
            row.url->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
            row.removeProvider = new QPushButton(tr("Remove"), m_keysGroup);
            row.removeProvider->setObjectName(QStringLiteral("RemoveProviderButton"));
            row.removeProvider->setToolTip(tr("Remove this provider and its saved API key"));
            connect(row.removeProvider, &QPushButton::clicked, this, [this, provider] { removeProvider(provider); });

            auto *box = new QWidget(m_keysGroup);
            auto *layout = new QVBoxLayout(box);
            layout->setContentsMargins(0, 0, 0, 0);
            auto *top = new QHBoxLayout;
            top->addWidget(row.url, 1);
            top->addWidget(row.removeProvider);
            layout->addLayout(top);
            m_keysForm->addRow(provider, box);
            if (!keyed.contains(provider)) {
                m_keys.push_back(row);
                continue;
            }

            row.edit = new QLineEdit(m_keysGroup);
            row.edit->setObjectName(QStringLiteral("ApiKey"));
            row.edit->setEchoMode(QLineEdit::Password);
            row.edit->setClearButtonEnabled(true);
            row.save = new QPushButton(tr("Save"), m_keysGroup);
            row.save->setObjectName(QStringLiteral("SaveApiKeyButton"));
            row.save->setEnabled(false);
            row.remove = new QPushButton(tr("Remove key"), m_keysGroup);
            row.remove->setObjectName(QStringLiteral("RemoveApiKeyButton"));
            row.remove->setEnabled(false);

            const auto save = [this, provider] {
                const QString key = findKey(provider)->edit->text().trimmed();
                if (!key.isEmpty())
                    saveKey(provider, key);
            };
            connect(row.edit, &QLineEdit::returnPressed, this, save);
            connect(row.save, &QPushButton::clicked, this, save);
            connect(row.edit, &QLineEdit::textChanged, row.save,
                    [save = row.save](const QString &t) { save->setEnabled(!t.trimmed().isEmpty()); });
            connect(row.remove, &QPushButton::clicked, this, [this, provider] { saveKey(provider, QString()); });

            auto *keyLine = new QHBoxLayout;
            keyLine->addWidget(row.edit, 1);
            keyLine->addWidget(row.save);
            keyLine->addWidget(row.remove);
            layout->addLayout(keyLine);
            m_keys.push_back(row);
        }
        alignLabels();
    }
    for (auto &row : m_keys) {
        const QJsonObject p = all.value(row.provider).toObject();
        const QString kind = p.value(QLatin1String("kind")).toString();
        QString text = p.value(QLatin1String("base_url")).toString();
        if (kind == QLatin1String("claude-code"))
            text = tr("Claude Code (%1 -p)").arg(p.value(QLatin1String("command")).toString());
        else if (kind == QLatin1String("local-speech"))
            text = tr("Local speech models");
        row.url->setText(text);
        row.url->setToolTip(text);
    }
    for (const auto &provider : keyed)
        updateKeyStatus(provider);
    updateAddProvider();
}

QString SettingsWindow::providerLabel(const QString &provider) const
{
    return m_keyStatus.value(provider) == QLatin1String("missing") ? tr("%1 (no key)").arg(provider) : provider;
}

void SettingsWindow::relabelProviders()
{
    for (auto &row : m_roles)
        for (int i = 0; i < row.provider->count(); ++i) {
            const QVariant p = row.provider->itemData(i);
            if (!p.isNull() && !row.provider->itemText(i).endsWith(tr("(can't serve this)")))
                row.provider->setItemText(i, providerLabel(p.toString()));
        }
}

// The daemon refuses while a role uses it; removing also deletes its key.
void SettingsWindow::removeProvider(const QString &provider)
{
    const auto answer = QMessageBox::question(
        this, tr("Remove provider"),
        tr("Remove %1 from the config, along with its saved API key?").arg(provider));
    if (answer != QMessageBox::Yes)
        return;
    m_settings->reset(QStringLiteral("providers.") + provider, [this, provider](const QString &error) {
        if (error.isEmpty())
            m_status->hide();
        else
            showError(tr("provider %1").arg(provider), error);
    });
}

// Names as the daemon allows them (vivus_core::config::Config::validate).
static bool validProviderName(const QString &name)
{
    static const QRegularExpression re(QStringLiteral("^[A-Za-z0-9_-]+$"));
    return re.match(name).hasMatch();
}

// Fills in the name (unless one was typed) and URL; Claude Code needs neither URL nor key.
void SettingsWindow::onPresetChanged()
{
    const QStringList preset = m_newPreset->currentData().toStringList();
    const bool claude = preset[0] == QLatin1String("claude-code");
    if (m_newName->text().trimmed() == m_presetName)
        m_newName->setText(preset[1]);
    m_presetName = preset[1];
    m_newUrl->setText(preset[2]);
    m_newUrl->setVisible(!claude);
    m_newKey->setVisible(!claude);
    if (claude)
        m_newKey->clear();
    updateAddProvider();
}

void SettingsWindow::updateAddProvider()
{
    const bool claude = m_newPreset->currentData().toStringList().value(0) == QLatin1String("claude-code");
    const QString name = m_newName->text().trimmed();
    const QUrl url(m_newUrl->text().trimmed(), QUrl::StrictMode);
    const bool taken = m_settings->value(QStringLiteral("providers")).toObject().contains(name);
    const bool nameOk = validProviderName(name) && !taken;
    const bool urlOk = claude || url.isValid() && !url.host().isEmpty()
        && (url.scheme() == QLatin1String("https") || url.scheme() == QLatin1String("http"));
    m_newName->setToolTip(taken ? tr("There's already a provider called %1").arg(name)
                                : tr("Letters, digits, - and _; roles refer to it by this name"));
    m_addProvider->setEnabled(nameOk && urlOk && !m_addProvider->property("busy").toBool());
}

// One table write (kind and base_url are both required), then the key if given.
void SettingsWindow::addProvider()
{
    if (!m_addProvider->isEnabled())
        return;
    const QString name = m_newName->text().trimmed();
    const QString key = m_newKey->text().trimmed();
    const QString kind = m_newPreset->currentData().toStringList().value(0);
    QVariantMap table{{QStringLiteral("kind"), kind}};
    if (kind == QLatin1String("openai-compatible"))
        table.insert(QStringLiteral("base_url"), m_newUrl->text().trimmed());
    m_addProvider->setProperty("busy", true);
    m_addProvider->setEnabled(false);
    const auto finish = [this](const QString &what, const QString &error) {
        m_addProvider->setProperty("busy", false);
        if (error.isEmpty()) {
            m_status->hide();
            m_newKey->clear();
            m_newName->clear();
            m_newPreset->setCurrentIndex(0);
            onPresetChanged();
        } else {
            showError(what, error);
        }
        updateAddProvider();
    };
    m_settings->set(QStringLiteral("providers.") + name, table, [this, name, key, finish](const QString &error) {
        if (!error.isEmpty() || key.isEmpty()) {
            finish(tr("provider %1").arg(name), error);
            return;
        }
        m_settings->setApiKey(name, key, [this, name, finish](const QString &error) {
            finish(tr("the %1 API key").arg(name), error);
            updateKeyStatus(name);
        });
    });
}

SettingsWindow::KeyRow *SettingsWindow::findKey(const QString &provider)
{
    for (auto &row : m_keys)
        if (row.provider == provider)
            return &row;
    return nullptr;
}

// The variable a key goes by, as the daemon derives it (vivus_core::keys::key_var).
static QString keyVariable(const QString &provider, const QJsonValue &configured)
{
    if (configured.isString())
        return configured.toString();
    QString var;
    for (const QChar c : provider)
        var += (c.isLetterOrNumber() && c.unicode() < 128) ? c.toUpper() : QLatin1Char('_');
    return var + QStringLiteral("_API_KEY");
}

void SettingsWindow::updateKeyStatus(const QString &provider)
{
    m_settings->apiKeyStatus(provider, [this, provider](const QString &status, const QString &error) {
        auto *row = findKey(provider);
        if (!row || !row->edit)
            return;
        const QString var =
            keyVariable(provider, m_settings->value(QStringLiteral("providers.%1.api_key_env").arg(provider)));
        QString text;
        if (!error.isEmpty())
            text = error;
        else if (status == QLatin1String("keyring"))
            text = tr("Saved in the keyring; paste a key to replace it");
        else if (status == QLatin1String("file"))
            text = tr("Saved in ~/.config/vivus/.env; paste a key to replace it");
        else if (status == QLatin1String("environment"))
            text = tr("Set by $%1 in the environment, which wins over a saved key").arg(var);
        else
            text = tr("No key: paste one");
        // The placeholder may be cut short; the tooltip has all of it.
        row->edit->setPlaceholderText(text);
        row->edit->setToolTip(text + QLatin1Char('\n') + tr("Also read from $%1 in the environment").arg(var));
        row->remove->setEnabled(status == QLatin1String("keyring") || status == QLatin1String("file"));
        if (error.isEmpty() && m_keyStatus.value(provider) != status) {
            m_keyStatus.insert(provider, status);
            relabelProviders();
        }
    });
}

// Then re-lists models for the roles on this provider, which also tests the key.
void SettingsWindow::saveKey(const QString &provider, const QString &key)
{
    auto *row = findKey(provider);
    row->edit->setEnabled(false);
    row->save->setEnabled(false);
    row->remove->setEnabled(false);
    m_settings->setApiKey(provider, key, [this, provider](const QString &error) {
        auto *row = findKey(provider);
        if (!row || !row->edit)
            return;
        row->edit->setEnabled(true);
        if (error.isEmpty()) {
            row->edit->clear();
            m_status->hide();
        } else {
            showError(tr("the %1 API key").arg(provider), error);
            row->save->setEnabled(!row->edit->text().trimmed().isEmpty());
        }
        updateKeyStatus(provider);
        for (auto &role : m_roles)
            if (roleValue(role, "provider") == provider) {
                role.modelsFor.clear();
                syncModels(role);
            }
    });
}
