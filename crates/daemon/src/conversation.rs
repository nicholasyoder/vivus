//! Rolling conversation history: the last few exchanges, sent with each request
//! so follow-ups work. Bounded by turns, a character budget and an idle reset.
//! See docs/inference.md#conversation-memory.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use vivus_core::DocumentStore;
use vivus_inference::Message;

/// Request and reply text kept across all exchanges (~1k tokens), so a chatty
/// session can't crowd out the prompt or a free tier's tokens-per-minute.
const HISTORY_CHARS: usize = 4000;
/// The newest sidebar document's text, so "what's the third item?" works.
const DOCUMENT_CHARS: usize = 1500;
/// Documents listed by id and title, newest last.
const MAX_DOCUMENTS: usize = 5;

/// One request and what came of it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Exchange {
    pub user: String,
    /// What was spoken (generated, if interrupted).
    pub reply: String,
    /// Results of silent actions ("Closed Firefox."), for replies with no words.
    pub actions: Vec<String>,
    /// Sidebar documents created or changed, by id.
    pub documents: Vec<String>,
    /// Cut off by the user talking or cancelling.
    pub interrupted: bool,
}

#[derive(Default)]
pub struct History {
    exchanges: VecDeque<Exchange>,
    last: Option<Instant>,
}

impl History {
    pub fn push(&mut self, exchange: Exchange) {
        self.exchanges.push_back(exchange);
        self.last = Some(Instant::now());
    }

    /// The latest reply was cut off while it was being spoken.
    pub fn interrupt_last(&mut self) {
        if let Some(ex) = self.exchanges.back_mut() {
            ex.interrupted = true;
        }
    }

    /// The exchanges to send with a new request, oldest first. Forgets
    /// everything after `forget_after` without a request.
    pub fn recent(&mut self, max_turns: usize, forget_after: Duration) -> Vec<Exchange> {
        if self.last.is_some_and(|t| t.elapsed() > forget_after) {
            tracing::info!("conversation forgotten after {:?} idle", forget_after);
            self.exchanges.clear();
        }
        while self.exchanges.len() > max_turns {
            self.exchanges.pop_front();
        }
        let mut kept = Vec::new();
        let mut budget = HISTORY_CHARS;
        for ex in self.exchanges.iter().rev() {
            let len = ex.user.len() + ex.reply.len();
            if len > budget {
                // Always keep the latest exchange, shortened, if it's all that fits.
                if kept.is_empty() {
                    let mut ex = ex.clone();
                    ex.user = truncate(&ex.user, budget / 4);
                    ex.reply = truncate(&ex.reply, budget - ex.user.len());
                    kept.push(ex);
                }
                break;
            }
            budget -= len;
            kept.push(ex.clone());
        }
        kept.reverse();
        kept
    }
}

/// History as chat turns. Actions and documents stay out of the assistant
/// text where possible, so the model doesn't learn to say them aloud.
pub fn messages(history: &[Exchange]) -> Vec<Message> {
    let mut out = Vec::new();
    for ex in history {
        let mut reply = ex.reply.trim().to_owned();
        if reply.is_empty() {
            reply = ex.actions.join(" ");
        }
        if ex.interrupted {
            reply = if reply.is_empty() {
                "(interrupted before answering)".into()
            } else {
                format!("{reply}… (interrupted)")
            };
        }
        if reply.is_empty() {
            reply = "(no reply)".into();
        }
        out.push(Message::User(ex.user.clone()));
        out.push(Message::Assistant {
            content: Some(reply),
            tool_calls: Vec::new(),
        });
    }
    out
}

/// A system-prompt section listing the conversation's sidebar documents, with
/// the newest one's text.
pub fn documents_context(history: &[Exchange], docs: &DocumentStore) -> Option<String> {
    let mut ids: Vec<&str> = Vec::new();
    for id in history.iter().flat_map(|ex| &ex.documents) {
        ids.retain(|i| i != id);
        ids.push(id);
    }
    let docs: Vec<_> = ids.iter().filter_map(|id| docs.get(id)).collect();
    let skip = docs.len().saturating_sub(MAX_DOCUMENTS);
    let docs = &docs[skip..];
    let newest = docs.last()?;
    let mut out = String::from(
        "Sidebar documents from this conversation (pass document_id to `show` to change one):",
    );
    for d in docs {
        out.push_str(&format!("\n- {} \"{}\"", d.id, d.title));
    }
    out.push_str(&format!(
        "\n\nText of {}:\n{}",
        newest.id,
        truncate(&newest.markdown, DOCUMENT_CHARS)
    ));
    Some(out)
}

/// History as plain text, for Claude Code's single prompt.
pub fn transcript(history: &[Exchange]) -> String {
    let mut out = String::new();
    for m in messages(history) {
        match m {
            Message::User(t) => out.push_str(&format!("User: {t}\n")),
            Message::Assistant {
                content: Some(t), ..
            } => out.push_str(&format!("Vivus: {t}\n")),
            _ => {}
        }
    }
    out
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.into();
    }
    let mut end = max.saturating_sub(3);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(user: &str, reply: &str) -> Exchange {
        Exchange {
            user: user.into(),
            reply: reply.into(),
            ..Default::default()
        }
    }

    #[test]
    fn keeps_last_turns_within_budget() {
        let mut h = History::default();
        for i in 0..5 {
            h.push(ex(&format!("q{i}"), "a"));
        }
        let r = h.recent(3, Duration::from_secs(60));
        assert_eq!(
            r.iter().map(|e| e.user.as_str()).collect::<Vec<_>>(),
            ["q2", "q3", "q4"]
        );

        h.push(ex("big", &"x".repeat(HISTORY_CHARS - 4)));
        let r = h.recent(10, Duration::from_secs(60));
        assert_eq!(r.len(), 1);
        h.push(ex("huge", &"y".repeat(HISTORY_CHARS * 2)));
        let r = h.recent(10, Duration::from_secs(60));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].user, "huge");
        assert!(r[0].user.len() + r[0].reply.len() <= HISTORY_CHARS + 3);
    }

    #[test]
    fn forgets_when_idle() {
        let mut h = History::default();
        h.push(ex("q", "a"));
        h.last = Some(Instant::now() - Duration::from_secs(120));
        assert!(h.recent(10, Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn silent_and_interrupted_replies() {
        let mut closed = ex("close this", "");
        closed.actions.push("Closed Firefox.".into());
        let mut cut = ex("tell me a story", "Once upon a time");
        cut.interrupted = true;
        let text = transcript(&[closed, cut, ex("stop", "")]);
        assert_eq!(
            text,
            "User: close this\nVivus: Closed Firefox.\n\
             User: tell me a story\nVivus: Once upon a time… (interrupted)\n\
             User: stop\nVivus: (no reply)\n"
        );
    }

    #[test]
    fn lists_documents_with_newest_text() {
        let mut store = DocumentStore::default();
        let a = store.create("Packing list", "- tent").id.clone();
        let b = store.create("Recipe", "1. flour").id.clone();
        let mut first = ex("packing list", "Done.");
        first.documents.push(a.clone());
        let mut second = ex("recipe", "Here.");
        second.documents.push(b.clone());
        let mut third = ex("add a stove", "Added.");
        third.documents.push(a.clone());
        let ctx = documents_context(&[first, second, third], &store).unwrap();
        assert!(ctx.contains("- doc-2 \"Recipe\"\n- doc-1 \"Packing list\""));
        assert!(ctx.ends_with("Text of doc-1:\n- tent"));
        assert!(documents_context(&[ex("hi", "hello")], &store).is_none());
    }
}
