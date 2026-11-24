#include "sidebarwindow.h"
#include "daemonclient.h"

#include <QApplication>
#include <QComboBox>
#include <QGuiApplication>
#include <QHBoxLayout>
#include <QLabel>
#include <QLineEdit>
#include <QPushButton>
#include <QStyle>
#include <QTextBrowser>
#include <QVBoxLayout>
#include <QWindow>

#include <LayerShellQt/Window>

static constexpr int SidebarWidth = 420;

SidebarWindow::SidebarWindow(DaemonClient *client, QWidget *parent)
    : QWidget(parent)
    , m_client(client)
{
    setObjectName(QStringLiteral("VivusSidebar"));
    setWindowTitle(QStringLiteral("Vivus"));
    resize(SidebarWidth, 800);

    m_stateDot = new QLabel(this);
    m_stateDot->setObjectName(QStringLiteral("StateDot"));
    m_stateDot->setFixedSize(10, 10);
    m_stateLabel = new QLabel(this);
    m_stateLabel->setObjectName(QStringLiteral("StateLabel"));

    m_history = new QComboBox(this);
    m_history->setObjectName(QStringLiteral("History"));
    m_history->setSizeAdjustPolicy(QComboBox::AdjustToMinimumContentsLengthWithIcon);

    m_document = new QTextBrowser(this);
    m_document->setObjectName(QStringLiteral("Document"));
    m_document->setOpenExternalLinks(true);
    m_document->setFrameShape(QFrame::NoFrame);

    m_transcript = new QLabel(this);
    m_transcript->setObjectName(QStringLiteral("Transcript"));
    m_transcript->setWordWrap(true);
    m_transcript->setTextFormat(Qt::PlainText);
    m_transcript->hide();

    m_input = new QLineEdit(this);
    m_input->setObjectName(QStringLiteral("Input"));
    m_input->setPlaceholderText(tr("Ask…"));
    m_talk = new QPushButton(this);
    m_talk->setObjectName(QStringLiteral("TalkButton"));

    m_quit = new QPushButton(tr("Quit"), this);
    m_quit->setObjectName(QStringLiteral("QuitButton"));
    m_quit->setToolTip(tr("Close the sidebar and stop the daemon"));

    auto *header = new QHBoxLayout;
    header->addWidget(m_stateDot);
    header->addWidget(m_stateLabel);
    header->addWidget(m_history, 1);
    header->addWidget(m_quit);

    auto *footer = new QHBoxLayout;
    footer->addWidget(m_input, 1);
    footer->addWidget(m_talk);

    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(12, 12, 12, 12);
    layout->setSpacing(8);
    layout->addLayout(header);
    layout->addWidget(m_document, 1);
    layout->addWidget(m_transcript);
    layout->addLayout(footer);

    connect(m_history, &QComboBox::activated, m_client, &DaemonClient::selectDocument);
    connect(m_input, &QLineEdit::returnPressed, this, [this] {
        m_client->ask(m_input->text());
        m_input->clear();
    });
    connect(m_talk, &QPushButton::clicked, m_client, &DaemonClient::toggleListen);
    connect(m_quit, &QPushButton::clicked, this, [this] {
        m_client->quitDaemon();
        QApplication::quit();
    });

    connect(m_client, &DaemonClient::connectedChanged, this, &SidebarWindow::updateState);
    connect(m_client, &DaemonClient::stateChanged, this, &SidebarWindow::updateState);
    connect(m_client, &DaemonClient::documentsChanged, this, &SidebarWindow::updateDocuments);
    connect(m_client, &DaemonClient::currentChanged, this, &SidebarWindow::updateCurrent);
    connect(m_client, &DaemonClient::visibilityRequested, this, &SidebarWindow::handleVisibilityRequest);
    connect(m_client, &DaemonClient::transcript, this, &SidebarWindow::showTranscript);

    updateState();
    updateDocuments();
    updateCurrent();
}

void SidebarWindow::setupLayerShell()
{
    // Without layer-shell (e.g. GNOME) it stays a normal toplevel.
    if (QGuiApplication::platformName() != QLatin1String("wayland"))
        return;
    winId(); // force native window creation so windowHandle() is valid
    auto *ls = LayerShellQt::Window::get(windowHandle());
    ls->setScope(QStringLiteral("vivus-sidebar"));
    ls->setLayer(LayerShellQt::Window::LayerTop);
    ls->setAnchors({LayerShellQt::Window::AnchorTop, LayerShellQt::Window::AnchorBottom,
                    LayerShellQt::Window::AnchorRight});
    ls->setExclusiveZone(0);
    ls->setMargins({0, 8, 8, 8});
    ls->setDesiredSize({SidebarWidth, 0}); // 0: stretch between the anchored edges
    ls->setKeyboardInteractivity(LayerShellQt::Window::KeyboardInteractivityOnDemand);
}

void SidebarWindow::updateState()
{
    const bool connected = m_client->connected();
    const QString state = connected ? m_client->state() : QStringLiteral("disconnected");

    // Dynamic property for QSS selectors like QLabel#StateDot[state="listening"].
    for (QWidget *w : {static_cast<QWidget *>(this), static_cast<QWidget *>(m_stateDot)}) {
        w->setProperty("state", state);
        w->style()->unpolish(w);
        w->style()->polish(w);
    }

    m_stateLabel->setText(connected ? state : tr("daemon not running"));
    m_talk->setText(state == QLatin1String("listening") ? tr("Stop") : tr("Talk"));
    m_input->setEnabled(connected);
    m_talk->setEnabled(connected);
}

void SidebarWindow::updateDocuments()
{
    const QSignalBlocker block(m_history);
    m_history->clear();
    for (const auto &doc : m_client->documents())
        m_history->addItem(doc.title, doc.id);
    m_history->setEnabled(m_history->count() > 0);
    if (m_history->count() == 0)
        m_history->setPlaceholderText(tr("No documents"));
    m_history->setCurrentIndex(m_client->currentIndex());
}

void SidebarWindow::updateCurrent()
{
    {
        const QSignalBlocker block(m_history);
        m_history->setCurrentIndex(m_client->currentIndex());
    }
    m_document->setMarkdown(m_client->currentMarkdown());
}

// Shows what you're saying, live. Assistant replies go to TTS and documents, not here.
void SidebarWindow::showTranscript(const QString &role, const QString &text, bool isFinal)
{
    if (role != QLatin1String("user"))
        return;
    m_transcript->setVisible(!text.isEmpty());
    m_transcript->setText(text);
    // For QSS: #Transcript[is-final="false"] styles text still being recognized.
    m_transcript->setProperty("is-final", isFinal);
    m_transcript->style()->unpolish(m_transcript);
    m_transcript->style()->polish(m_transcript);
}

void SidebarWindow::handleVisibilityRequest(const QString &action)
{
    if (action == QLatin1String("show"))
        show();
    else if (action == QLatin1String("hide"))
        hide();
    else
        setVisible(!isVisible());
}
