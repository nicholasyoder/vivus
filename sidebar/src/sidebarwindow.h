#pragma once

#include <QWidget>

class DaemonClient;
class QComboBox;
class QFrame;
class QLabel;
class QLineEdit;
class QPlainTextEdit;
class QPushButton;
class QTextBrowser;

// Object names and the `state` property are the QSS styling surface; see data/sidebar.qss.
class SidebarWindow : public QWidget
{
    Q_OBJECT

public:
    explicit SidebarWindow(DaemonClient *client, QWidget *parent = nullptr);

    // Must run before the first show().
    void setupLayerShell();

signals:
    void settingsRequested();

private:
    void updateState();
    void updateDocuments();
    void updateCurrent();
    void updatePermission();
    void handleVisibilityRequest(const QString &action);
    void showTranscript(const QString &role, const QString &text, bool isFinal);

    DaemonClient *m_client;
    QLabel *m_stateDot;
    QLabel *m_stateLabel;
    QComboBox *m_history;
    QTextBrowser *m_document;
    QFrame *m_permissionCard;
    QLabel *m_permissionSummary;
    QPlainTextEdit *m_permissionDetail;
    QPushButton *m_permissionAllow;
    QPushButton *m_permissionDeny;
    QLabel *m_transcript;
    QLineEdit *m_input;
    QPushButton *m_talk;
    QPushButton *m_settings;
    QPushButton *m_quit;
};
