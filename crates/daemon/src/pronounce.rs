//! LLM pronunciation lookup for words the TTS lexicon lacks. Words found in a
//! reply are requested as soon as their sentence is complete, so the answer is
//! usually back before TTS reaches it; the TTS thread waits for pending words
//! up to a deadline. Answers are cached in the learned pronunciations file.
//! See docs/speech.md#out-of-lexicon-words.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use tokio::runtime::Handle;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use vivus_core::Config;
use vivus_inference::{ChatRequest, Client, Error, Message};
use vivus_speech::SharedLexicon;

/// Background (prefetch) words per request, and the pause between such requests,
/// so prefetching stays well inside free-tier rate limits.
const IDLE_BATCH: usize = 25;
const IDLE_GAP: Duration = Duration::from_secs(10);
/// Reasoning models spend some of this before answering.
const MAX_TOKENS: u32 = 1000;

const SYSTEM: &str = "\
You supply pronunciations to a text-to-speech engine. For each word, give how \
English speakers normally say it, in CMUdict ARPAbet with a stress digit on \
every vowel (0, 1 or 2). Names of people, places, companies and software are \
pronounced the way their owners and users say them. An abbreviation is spelled \
out letter by letter only if that is how people say it.

Reply with only a JSON object mapping each word exactly as given to its \
pronunciation, for example:
{\"nginx\": \"EH1 N JH IH0 N EH2 K S\", \"Kubernetes\": \"K UW2 B ER0 N EH1 T IY0 Z\"}";

#[derive(Default)]
struct State {
    /// Lowercase words waiting for an answer → requested for a reply (not just prefetch).
    pending: HashMap<String, bool>,
    /// Every word requested this session, so a failed lookup isn't retried.
    tried: HashSet<String>,
}

struct Shared {
    state: Mutex<State>,
    answered: Condvar,
}

struct Job {
    words: Vec<String>,
    urgent: bool,
}

#[derive(Clone)]
pub struct Pronouncer {
    tx: UnboundedSender<Job>,
    shared: Arc<Shared>,
    lexicon: SharedLexicon,
}

impl std::fmt::Debug for Pronouncer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pronouncer")
    }
}

impl Pronouncer {
    /// Uses the router's provider and model. `None` (logged) if there is no router.
    pub fn from_config(config: &Config, lexicon: SharedLexicon, rt: &Handle) -> Option<Self> {
        let build = || -> anyhow::Result<(Client, String)> {
            let role = config.roles.router.as_ref().context("no [roles.router]")?;
            let model = role.model.clone().context("[roles.router] has no model")?;
            Ok((
                Client::from_provider(&config.providers[&role.provider])?,
                model,
            ))
        };
        match build() {
            Ok((client, model)) => Some(Self::spawn(
                client,
                model,
                lexicon,
                vivus_speech::learned_pronunciation_file(),
                rt,
            )),
            Err(e) => {
                tracing::warn!("pronunciation lookup off: {e:#}");
                None
            }
        }
    }

    pub fn spawn(
        client: Client,
        model: String,
        lexicon: SharedLexicon,
        learned: Option<PathBuf>,
        rt: &Handle,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            answered: Condvar::new(),
        });
        let (tx, rx) = unbounded_channel();
        let task = Task {
            client: Arc::new(client),
            model: model.into(),
            low_effort: AtomicBool::new(true),
            learned: learned.map(Arc::new),
            shared: shared.clone(),
            lexicon: lexicon.clone(),
        };
        rt.spawn(task.run(rx));
        Self {
            tx,
            shared,
            lexicon,
        }
    }

    /// Looks up `words` (as reported missing by the lexicon). `urgent` words are
    /// needed for speech now and go ahead of prefetched ones.
    pub fn request(&self, words: &[String], urgent: bool) {
        let words: Vec<String> = {
            let lexicon = self.lexicon.read().unwrap();
            words
                .iter()
                .flat_map(|w| lookup_words(w))
                .filter(|w| !lexicon.contains(w))
                .collect()
        };
        let mut batch = Vec::new();
        let mut state = self.shared.state.lock().unwrap();
        for word in words {
            let key = word.to_lowercase();
            if state.tried.insert(key.clone()) {
                state.pending.insert(key, urgent);
                batch.push(word);
            } else if let Some(u @ false) = state.pending.get_mut(&key)
                && urgent
            {
                // Queued for prefetch, needed now.
                *u = true;
                batch.push(word);
            }
        }
        drop(state);
        if !batch.is_empty() {
            let _ = self.tx.send(Job {
                words: batch,
                urgent,
            });
        }
    }

    /// Blocks until none of `words` is pending, `deadline` passes, or
    /// `cancelled` returns true. Returns whether anything was waited for.
    pub fn wait(&self, words: &[String], deadline: Instant, cancelled: impl Fn() -> bool) -> bool {
        let keys: Vec<String> = words
            .iter()
            .flat_map(|w| lookup_words(w))
            .map(|w| w.to_lowercase())
            .collect();
        let mut state = self.shared.state.lock().unwrap();
        let mut waited = false;
        while keys.iter().any(|k| state.pending.contains_key(k)) {
            let now = Instant::now();
            if now >= deadline || cancelled() {
                break;
            }
            waited = true;
            // Wake now and then to notice cancellation.
            let step = (deadline - now).min(Duration::from_millis(50));
            state = self.shared.answered.wait_timeout(state, step).unwrap().0;
        }
        waited
    }
}

/// What to ask about for a missing word: the base of a possessive, the parts of
/// a hyphenated word; nothing for words with digits or other symbols.
fn lookup_words(word: &str) -> Vec<String> {
    let word = word.trim_end_matches("'s").trim_end_matches("’s");
    word.split('-')
        .filter(|w| {
            (2..=40).contains(&w.chars().count())
                && w.chars()
                    .all(|c| c.is_alphabetic() || c == '\'' || c == '’')
        })
        .map(str::to_owned)
        .collect()
}

struct Task {
    client: Arc<Client>,
    model: Arc<str>,
    /// Cleared once the provider rejects `reasoning_effort`.
    low_effort: AtomicBool,
    lexicon: SharedLexicon,
    learned: Option<Arc<PathBuf>>,
    shared: Arc<Shared>,
}

impl Task {
    async fn run(self, mut rx: UnboundedReceiver<Job>) {
        let this = Arc::new(self);
        let mut idle: VecDeque<String> = VecDeque::new();
        let mut last_idle: Option<Instant> = None;
        loop {
            let wait = match (idle.is_empty(), last_idle) {
                (true, _) => None,
                (false, Some(t)) => Some(IDLE_GAP.saturating_sub(t.elapsed())),
                (false, None) => Some(Duration::ZERO),
            };
            let job = match wait {
                None => rx.recv().await,
                Some(d) => match tokio::time::timeout(d, rx.recv()).await {
                    Ok(job) => job,
                    Err(_) => {
                        // Idle batch, one at a time.
                        let n = idle.len().min(IDLE_BATCH);
                        let words: Vec<String> = idle.drain(..n).collect();
                        last_idle = Some(Instant::now());
                        this.clone().lookup(words).await;
                        continue;
                    }
                },
            };
            let Some(job) = job else { return };
            if job.urgent {
                // Concurrent, so one slow answer doesn't hold up the next sentence.
                tokio::spawn(this.clone().lookup(job.words));
            } else {
                idle.extend(job.words);
            }
        }
    }

    async fn lookup(self: Arc<Self>, words: Vec<String>) {
        // A prefetched word may have been answered by an urgent request since.
        let words: Vec<String> = {
            let state = self.shared.state.lock().unwrap();
            words
                .into_iter()
                .filter(|w| state.pending.contains_key(&w.to_lowercase()))
                .collect()
        };
        if words.is_empty() {
            return;
        }
        let start = Instant::now();
        let low_effort = self.low_effort.load(Ordering::Relaxed);
        let mut result = ask(&self.client, &self.model, &words, low_effort).await;
        if let Err(e) = &result
            && let Some(Error::Status { status: 400, .. }) = e.downcast_ref()
            && self.low_effort.swap(false, Ordering::Relaxed)
        {
            tracing::info!("pronunciation lookup: retrying without reasoning_effort ({e:#})");
            result = ask(&self.client, &self.model, &words, false).await;
        }
        let answers = match result {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(
                    "pronunciation lookup failed for {}: {e:#}",
                    words.join(", ")
                );
                HashMap::new()
            }
        };
        let mut learned = Vec::new();
        {
            let mut lexicon = self.lexicon.write().unwrap();
            for word in &words {
                let Some(arpabet) = answers.get(&word.to_lowercase()) else {
                    continue;
                };
                match lexicon.learn(word, arpabet) {
                    Ok(()) => learned.push(format!("{}  {}", word.to_uppercase(), arpabet.trim())),
                    Err(e) => tracing::warn!("pronunciation for {word}: {e}"),
                }
            }
        }
        tracing::info!(
            "pronunciations in {:.2} s: {}",
            start.elapsed().as_secs_f64(),
            if learned.is_empty() {
                "none".into()
            } else {
                learned.join(", ")
            }
        );
        if let (Some(path), false) = (&self.learned, learned.is_empty())
            && let Err(e) = append(path, &learned)
        {
            tracing::warn!("saving pronunciations to {}: {e}", path.display());
        }
        let mut state = self.shared.state.lock().unwrap();
        for word in &words {
            state.pending.remove(&word.to_lowercase());
        }
        self.shared.answered.notify_all();
    }
}

/// One request for all `words`; returns lowercase word → ARPAbet. `low_effort`
/// keeps a reasoning model from spending its token budget thinking.
async fn ask(
    client: &Client,
    model: &str,
    words: &[String],
    low_effort: bool,
) -> anyhow::Result<HashMap<String, String>> {
    let mut req = ChatRequest::new(
        model,
        vec![
            Message::System(SYSTEM.into()),
            Message::User(words.join("\n")),
        ],
    );
    req.temperature = Some(0.0);
    req.max_tokens = Some(MAX_TOKENS);
    req.reasoning_effort = low_effort.then(|| "low".into());
    let reply = client.complete(&req).await?;
    parse_answer(&reply.content).with_context(|| {
        let (finish, usage) = (&reply.finish_reason, &reply.usage);
        format!(
            "unexpected reply ({finish:?}, {usage:?}): {}",
            reply.content
        )
    })
}

/// The JSON object in `text` (models sometimes wrap it in a code fence).
fn parse_answer(text: &str) -> anyhow::Result<HashMap<String, String>> {
    let start = text.find('{').context("no JSON object")?;
    let end = text.rfind('}').context("no JSON object")?;
    let map: HashMap<String, serde_json::Value> = serde_json::from_str(&text[start..=end])?;
    Ok(map
        .into_iter()
        .filter_map(|(k, v)| Some((k.to_lowercase(), v.as_str()?.to_owned())))
        .collect())
}

fn append(path: &std::path::Path, lines: &[String]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for line in lines {
        writeln!(f, "{line}")?;
    }
    Ok(())
}

/// Names of installed applications, for idle prefetch.
pub fn app_names() -> Vec<String> {
    let mut names: Vec<String> = vivus_apps::Apps::load()
        .iter()
        .map(|a| a.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_word_forms() {
        assert_eq!(lookup_words("Hyprland's"), ["Hyprland"]);
        assert_eq!(lookup_words("wl-roots"), ["wl", "roots"]);
        assert!(lookup_words("x86").is_empty());
        assert!(lookup_words("a").is_empty());
    }

    #[test]
    fn parses_answers() {
        let text = "```json\n{\"Nginx\": \"EH1 N JH IH0 N EH2 K S\", \"x\": 3}\n```";
        let a = parse_answer(text).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a["nginx"], "EH1 N JH IH0 N EH2 K S");
        assert!(parse_answer("no idea").is_err());
    }

    #[test]
    fn request_and_wait() {
        let (tx, mut rx) = unbounded_channel();
        let p = Pronouncer {
            tx,
            lexicon: Arc::new(std::sync::RwLock::new(vivus_speech::Lexicon::from_words([
                "known",
            ]))),
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                answered: Condvar::new(),
            }),
        };
        p.request(&["known".to_owned()], true);
        assert!(rx.try_recv().is_err());
        let words = vec!["Zorb".to_owned()];
        p.request(&words, false);
        assert!(!rx.try_recv().unwrap().urgent);
        p.request(&words, false);
        assert!(rx.try_recv().is_err());
        // Needed for speech now: sent again, as urgent.
        p.request(&words, true);
        assert!(rx.try_recv().unwrap().urgent);

        let soon = Instant::now() + Duration::from_millis(20);
        assert!(p.wait(&words, soon, || false));
        p.shared.state.lock().unwrap().pending.clear();
        assert!(!p.wait(&words, Instant::now() + Duration::from_secs(5), || false));
    }

    /// Live check against the configured router: `cargo test -p vivus-daemon
    /// live_lookup -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_lookup() {
        let config = Config::load(&Config::default_path().unwrap()).unwrap();
        let role = config.roles.router.as_ref().unwrap();
        let client = Client::from_provider(&config.providers[&role.provider]).unwrap();
        let words: Vec<String> = [
            "Hyprland",
            "nginx",
            "Kubernetes",
            "Spotify",
            "LibreOffice",
            "Wayland",
            "sway",
        ]
        .map(String::from)
        .into();
        let t = Instant::now();
        let answers = ask(&client, role.model.as_ref().unwrap(), &words, true)
            .await
            .unwrap();
        println!("{:.2} s", t.elapsed().as_secs_f64());
        for w in &words {
            println!("{w}: {:?}", answers.get(&w.to_lowercase()));
        }
        assert!(
            words
                .iter()
                .all(|w| answers.contains_key(&w.to_lowercase()))
        );
    }
}
