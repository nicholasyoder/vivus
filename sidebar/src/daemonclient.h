#pragma once

#include <QList>
#include <QObject>

class OrgVivusAssistantInterface;
class OrgVivusDocumentsInterface;

// D-Bus client for org.vivus. Mirrors the daemon's state and document history.
class DaemonClient : public QObject
{
    Q_OBJECT

public:
    struct Document {
        QString id, title, markdown;
    };

    explicit DaemonClient(QObject *parent = nullptr);

    bool connected() const { return m_connected; }
    // idle | listening | thinking | speaking
    QString state() const { return m_state; }
    // Oldest first.
    const QList<Document> &documents() const { return m_docs; }
    int currentIndex() const { return m_current; }
    QString currentMarkdown() const;

    void selectDocument(int index);
    void ask(const QString &text);
    void toggleListen();
    void cancel();

signals:
    void connectedChanged(bool connected);
    void stateChanged(const QString &state);
    void documentsChanged();
    void currentChanged();
    // show | hide | toggle
    void visibilityRequested(const QString &action);
    // role: user | assistant. User text arrives as partials, then one final.
    void transcript(const QString &role, const QString &text, bool isFinal);

private slots:
    void onPropertiesChanged(const QString &iface, const QVariantMap &changed, const QStringList &invalidated);
    void onDocumentChanged(const QString &id, const QString &title, const QString &markdown);

private:
    void setConnected(bool connected);
    void setState(const QString &state);
    void refresh();

    OrgVivusAssistantInterface *m_assistant;
    OrgVivusDocumentsInterface *m_documents;
    bool m_connected = false;
    QString m_state = QStringLiteral("idle");
    QList<Document> m_docs;
    int m_current = -1;
};
