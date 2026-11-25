//! Text normalization for TTS: turns written forms (numbers, times, dates,
//! currency, units, abbreviations, acronyms, URLs, symbols) into the words a
//! lexicon-based model can say. sherpa-onnx drops anything it can't look up.
//!
//! A sequence of regex passes; order matters (URLs before numbers, dates before
//! times before plain numbers, numbers before acronyms).

use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::lexicon::Lexicon;

pub fn normalize(text: &str, lexicon: &Lexicon) -> String {
    let mut t = text.to_owned();
    for (re, f) in ADDRESSES.iter() {
        t = replace(re, &t, *f);
    }
    t = clean_symbols(&t);
    for (re, f) in PASSES.iter() {
        t = replace(re, &t, *f);
    }
    t = acronyms(&t, lexicon);
    t = respell(&t, lexicon);
    tidy(&t)
}

type Pass = fn(&Captures, &str) -> String;

/// `f` also gets the whole text, to look at what follows the match.
fn replace(re: &Regex, text: &str, f: Pass) -> String {
    re.replace_all(text, |c: &Captures| f(c, text)).into_owned()
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap()
}

fn p(pattern: &str, f: Pass) -> (Regex, Pass) {
    (re(pattern), f)
}

const MONTHS: &[&str] = &[
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const MONTH: &str = r"(Jan(?:uary)?|Feb(?:ruary)?|Mar(?:ch)?|Apr(?:il)?|May|June?|July?|Aug(?:ust)?|Sept?(?:ember)?|Oct(?:ober)?|Nov(?:ember)?|Dec(?:ember)?)";
const NUM: &str = r"(\d+(?:,\d{3})*(?:\.\d+)?)";
const YEAR: &str = r"(1[1-9]\d\d|20\d\d)";
const TLDS: &str = "com|org|net|io|dev|ai|app|edu|gov|co|uk|de|fr|eu|me|info|xyz|sh|rs|gg|tv|ly|so";
const FILE_EXTS: &str = "txt|md|toml|json|yaml|yml|rs|py|js|ts|sh|conf|cfg|ini|desktop|html|css|pdf|png|jpg|jpeg|gif|svg|wav|mp3|mp4|zip|tar|gz|log|xml|qss|cpp|h|c";

/// Before symbol cleanup, which would break them up.
static ADDRESSES: LazyLock<Vec<(Regex, Pass)>> = LazyLock::new(|| {
    vec![
        // Emails and URLs: say the address, drop scheme, path and query.
        p(r"\b([\w.+-]+)@([\w-]+(?:\.[\w-]+)+)\b", |c, _| {
            format!("{} at {}", dotted(&c[1]), dotted(&c[2]))
        }),
        p(
            &format!(
                r"(?i)\b(?:https?://)?(?:www\.)?((?:[a-z0-9-]+\.)+(?:{TLDS}))\b(?:/[^\s]*[^\s.,!?;:)])?/?"
            ),
            |c, _| dotted(&c[1]),
        ),
        p(
            r"(?i)\bhttps?://([\w.-]+)(?:[/:?#][^\s]*[^\s.,!?;:)])?",
            |c, _| dotted(&c[1]),
        ),
        // Uppercase so the acronym pass spells the ones the lexicon lacks.
        p(&format!(r"\b([\w-]+)\.({FILE_EXTS})\b"), |c, _| {
            format!("{} dot {}", &c[1], c[2].to_uppercase())
        }),
        p(r"(^|\s)~/", |c, _| format!("{}home/", &c[1])),
        p(r"\s*@\s*", |_, _| " at ".into()),
    ]
});

static PASSES: LazyLock<Vec<(Regex, Pass)>> = LazyLock::new(|| {
    vec![
        // Currency.
        p(
            &format!(r"([$€£¥₹])\s?{NUM}(?:\s?(thousand|million|billion|trillion|[kKmMB]|bn)\b)?"),
            |c, _| money(&c[1], &c[2], c.get(3).map(|m| m.as_str())),
        ),
        p(&format!(r"\b{NUM}\s?(USD|EUR|GBP)\b"), |c, _| {
            let sym = match &c[2] {
                "USD" => "$",
                "EUR" => "€",
                _ => "£",
            };
            money(sym, &c[1], None)
        }),
        // Dates.
        p(r"\b(\d{4})-(\d{2})-(\d{2})\b", |c, _| {
            date(num(&c[2]), num(&c[3]), Some(&c[1])).unwrap_or_else(|| c[0].to_owned())
        }),
        p(r"\b(\d{1,2})/(\d{1,2})/(\d{4})\b", |c, _| {
            let (a, b) = (num(&c[1]), num(&c[2]));
            // US order unless only the other one makes sense.
            let (m, d) = if a > 12 && b <= 12 { (b, a) } else { (a, b) };
            date(m, d, Some(&c[3])).unwrap_or_else(|| c[0].to_owned())
        }),
        p(
            &format!(r"\b(Mon|Tue|Tues|Wed|Thu|Thur|Thurs|Fri|Sat|Sun)\.?,(\s+(?:{MONTH}|\d))"),
            |c, _| format!("{},{}", weekday(&c[1]), &c[2]),
        ),
        p(
            &format!(r"\b{MONTH}\.?\s+(\d{{1,2}})(?:st|nd|rd|th)?\b(?:,?\s+{YEAR}\b)?"),
            |c, _| {
                let year = c.get(3).map(|y| format!(", {}", year(num(y.as_str()))));
                format!(
                    "{} {}{}",
                    month(&c[1]),
                    ordinal(num(&c[2])),
                    year.unwrap_or_default()
                )
            },
        ),
        p(
            &format!(r"\b(\d{{1,2}})(?:st|nd|rd|th)?\s+(?:of\s+)?{MONTH}\b\.?(?:,?\s+{YEAR}\b)?"),
            |c, _| {
                let year = c.get(3).map(|y| format!(", {}", year(num(y.as_str()))));
                let (day, month) = (ordinal(num(&c[1])), month(&c[2]));
                format!("the {day} of {month}{}", year.unwrap_or_default())
            },
        ),
        p(&format!(r"\b{MONTH}\.?,?\s+{YEAR}\b"), |c, _| {
            format!("{} {}", month(&c[1]), year(num(&c[2])))
        }),
        // Times.
        p(
            r"\b(\d{1,2}):(\d{2})(?::(\d{2}))?(?:\s?([AaPp])\.?[Mm]\b(\.)?)?",
            |c, text| {
                let (h, m) = (num(&c[1]), num(&c[2]));
                if h > 24 || m > 59 {
                    return c[0].to_owned();
                }
                let ampm = c.get(4).map(|x| x.as_str().to_ascii_uppercase());
                let mut s = cardinal(h);
                match m {
                    0 if ampm.is_none() && h <= 12 => s.push_str(" o'clock"),
                    0 if ampm.is_none() => s.push_str(" hundred"),
                    0 => {}
                    1..=9 => s += &format!(" oh {}", cardinal(m)),
                    _ => s += &format!(" {}", cardinal(m)),
                }
                if let Some(sec) = c.get(3) {
                    s += &format!(" and {} seconds", cardinal(num(sec.as_str())));
                }
                if let Some(x) = ampm {
                    s += &format!(" {x} M");
                }
                s + keep_stop(c.get(5).is_some(), text, c.get(0).unwrap().end())
            },
        ),
        p(r"\b(\d{1,2})\s?([AaPp])\.?[Mm]\b(\.)?", |c, text| {
            let h = num(&c[1]);
            if h > 12 {
                return c[0].to_owned();
            }
            let stop = keep_stop(c.get(3).is_some(), text, c.get(0).unwrap().end());
            format!("{} {} M{stop}", cardinal(h), c[2].to_ascii_uppercase())
        }),
        // Decades: "1990s", "the '80s".
        p(r"\b(1[1-9]|20)(\d)0s\b", |c, _| {
            plural(&year(num(&c[1]) * 100 + num(&c[2]) * 10))
        }),
        p(r"(?:\B'|\b)([2-9])0s\b", |c, _| {
            plural(&cardinal(num(&c[1]) * 10))
        }),
        // Years after words that introduce them, and year ranges.
        p(&format!(r"\b{YEAR}\s?[–-]\s?{YEAR}\b"), |c, _| {
            format!("{} to {}", year(num(&c[1])), year(num(&c[2])))
        }),
        p(
            &format!(
                r"(?i)\b(in|since|by|from|until|till|through|before|after|year|circa|during|around|of)\s+{YEAR}\b"
            ),
            |c, _| format!("{} {}", &c[1], year(num(&c[2]))),
        ),
        // Signs, ranges, percent, units, ordinals, fractions.
        p(r"(^|[\s(=:,])-(\d)", |c, _| {
            format!("{}minus {}", &c[1], &c[2])
        }),
        p(r"(^|\s)[~≈]\s?(\d)", |c, _| {
            format!("{}about {}", &c[1], &c[2])
        }),
        p(r"(\d)\s?–\s?(\d)", |c, _| {
            format!("{} to {}", &c[1], &c[2])
        }),
        p(r"\b(\d{1,4})-(\d{1,4})\b", |c, _| {
            if num(&c[2]) > num(&c[1]) {
                format!("{} to {}", &c[1], &c[2])
            } else {
                c[0].to_owned()
            }
        }),
        p(&format!(r"{NUM}\s?%"), |c, _| {
            format!("{} percent", number(&c[1]))
        }),
        p(&format!(r"{NUM}\s?(?:°\s?([CF])\b|°)"), |c, _| {
            let unit = if c[1].trim() == "1" {
                "degree"
            } else {
                "degrees"
            };
            match c.get(2).map(|m| m.as_str()) {
                Some("C") => format!("{} {unit} Celsius", number(&c[1])),
                Some(_) => format!("{} {unit} Fahrenheit", number(&c[1])),
                None => format!("{} {unit}", number(&c[1])),
            }
        }),
        p(&unit_pattern(), |c, _| {
            let (_, one, many) = UNITS.iter().find(|u| u.0 == &c[2]).unwrap();
            let unit = if &c[1] == "1" { one } else { many };
            format!("{} {unit}", number(&c[1]))
        }),
        p(r"\b(\d+)(st|nd|rd|th)\b", |c, _| ordinal(num(&c[1]))),
        p(r"\b(\d)/([2-9]|10)\b", |c, _| {
            fraction(num(&c[1]), num(&c[2]))
        }),
        p(r"#(\d)", |c, _| format!("number {}", &c[1])),
        p(r"\bNo\.\s?(\d)", |c, _| format!("number {}", &c[1])),
        // Version numbers and IP addresses, then every remaining number.
        p(r"\b\d+(?:\.\d+){2,}\b", |c, _| {
            c[0].split('.')
                .map(|p| cardinal(num(p)))
                .collect::<Vec<_>>()
                .join(" point ")
        }),
        p(NUM, |c, text| {
            let m = c.get(0).unwrap();
            let before = text[..m.start()].chars().next_back();
            let after = text[m.end()..].chars().next();
            let pad = |c: Option<char>| {
                if c.is_some_and(char::is_alphanumeric) {
                    " "
                } else {
                    ""
                }
            };
            format!("{}{}{}", pad(before), number(&c[1]), pad(after))
        }),
        // Abbreviations.
        p(r"\bDr\.(\s+[A-Z])", |c, _| format!("Doctor{}", &c[1])),
        p(r"\bSt\.(\s+[A-Z])", |c, _| format!("Saint{}", &c[1])),
        p(
            r"\b(Mr|Mrs|Ms|Prof|Jr|Sr|Mt|Ave|Dr|St|vs|approx|etc|e\.g|i\.e)\.",
            |c, text| {
                let end = c.get(0).unwrap().end();
                match &c[1] {
                    "Mr" => "Mister".into(),
                    "Mrs" => "Missus".into(),
                    "Ms" => "Miss".into(),
                    "Prof" => "Professor".into(),
                    "Jr" => "Junior".into(),
                    "Sr" => "Senior".into(),
                    "Mt" => "Mount".into(),
                    "Ave" => format!("Avenue{}", keep_stop(true, text, end)),
                    "Dr" => format!("Drive{}", keep_stop(true, text, end)),
                    "St" => format!("Street{}", keep_stop(true, text, end)),
                    "vs" => "versus".into(),
                    "approx" => "approximately".into(),
                    "etc" => format!("and so on{}", keep_stop(true, text, end)),
                    "e.g" => "for example".into(),
                    _ => "that is".into(),
                }
            },
        ),
        p(r"\bvs\b", |_, _| "versus".into()),
        p(r"\b(?i:w)/\s", |_, _| "with ".into()),
        p(r"\band/or\b", |_, _| "and or".into()),
        p(r"\s*/\s*", |_, _| " slash ".into()),
        p(r"%", |_, _| " percent ".into()),
        p(r"°", |_, _| " degrees ".into()),
        p(r"\s+[–-]+\s+", |_, _| ", ".into()),
    ]
});

const UNITS: &[(&str, &str, &str)] = &[
    ("km/h", "kilometer an hour", "kilometers an hour"),
    ("kph", "kilometer an hour", "kilometers an hour"),
    ("mph", "mile an hour", "miles an hour"),
    ("m/s", "meter a second", "meters a second"),
    ("km", "kilometer", "kilometers"),
    ("cm", "centimeter", "centimeters"),
    ("mm", "millimeter", "millimeters"),
    ("m", "meter", "meters"),
    ("mi", "mile", "miles"),
    ("ft", "foot", "feet"),
    ("kg", "kilogram", "kilograms"),
    ("mg", "milligram", "milligrams"),
    ("g", "gram", "grams"),
    ("lb", "pound", "pounds"),
    ("lbs", "pound", "pounds"),
    ("oz", "ounce", "ounces"),
    ("ml", "milliliter", "milliliters"),
    ("mL", "milliliter", "milliliters"),
    ("KB", "kilobyte", "kilobytes"),
    ("kB", "kilobyte", "kilobytes"),
    ("MB", "megabyte", "megabytes"),
    ("GB", "gigabyte", "gigabytes"),
    ("TB", "terabyte", "terabytes"),
    ("KiB", "kilobyte", "kilobytes"),
    ("MiB", "megabyte", "megabytes"),
    ("GiB", "gigabyte", "gigabytes"),
    ("TiB", "terabyte", "terabytes"),
    ("Mbps", "megabit a second", "megabits a second"),
    ("Gbps", "gigabit a second", "gigabits a second"),
    ("Hz", "hertz", "hertz"),
    ("kHz", "kilohertz", "kilohertz"),
    ("MHz", "megahertz", "megahertz"),
    ("GHz", "gigahertz", "gigahertz"),
    ("kWh", "kilowatt hour", "kilowatt hours"),
    ("kW", "kilowatt", "kilowatts"),
    ("W", "watt", "watts"),
    ("V", "volt", "volts"),
    ("mAh", "milliamp hour", "milliamp hours"),
    ("ms", "millisecond", "milliseconds"),
    ("sec", "second", "seconds"),
    ("secs", "second", "seconds"),
    ("s", "second", "seconds"),
    ("min", "minute", "minutes"),
    ("mins", "minute", "minutes"),
    ("h", "hour", "hours"),
    ("hr", "hour", "hours"),
    ("hrs", "hour", "hours"),
    ("x", "times", "times"),
];

fn unit_pattern() -> String {
    // Longest first so "km/h" wins over "km" and "ms" over "m".
    let mut units: Vec<_> = UNITS.iter().map(|u| regex::escape(u.0)).collect();
    units.sort_by_key(|u| std::cmp::Reverse(u.len()));
    format!(r"{NUM}\s?({})\b", units.join("|"))
}

/// Characters sherpa can't look up or that would glue onto a word.
fn clean_symbols(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '’' | '‘' => out.push('\''),
            '"' | '`' | '“' | '”' | '„' | '«' | '»' | '*' | '_' | '|' | '^' | '<' | '>' | '['
            | ']' | '{' | '}' | '\\' => out.push(' '),
            '(' | ')' => out.push_str(", "),
            '—' => out.push_str(", "),
            '&' => out.push_str(" and "),
            '+' => out.push_str(" plus "),
            '=' => out.push_str(" equals "),
            '×' => out.push_str(" times "),
            '\u{a0}' | '\t' => out.push(' '),
            '\u{200d}' | '\u{fe0f}' => {}
            c if (c as u32) >= 0x1F000 || ('\u{2600}'..='\u{27BF}').contains(&c) => {}
            c => out.push(c),
        }
    }
    // A leading "#" is a markdown heading; "#5" is handled later.
    out.trim_start_matches(|c: char| c == '#' || c.is_whitespace())
        .replace("# ", " ")
}

/// Some spoken-as-letters acronyms are also ordinary words in the lexicon.
const SPELL_ALWAYS: &[&str] = &["US", "UK", "IT", "OS", "ID", "UN", "IP", "PR", "HR"];

fn acronyms(text: &str, lexicon: &Lexicon) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| re(r"\b([A-Z]{2,6})('s|s)?\b"));
    RE.replace_all(text, |c: &Captures| {
        let word = &c[1];
        if !SPELL_ALWAYS.contains(&word) && (lexicon.contains(&c[0]) || lexicon.contains(word)) {
            return c[0].to_owned();
        }
        let mut letters: Vec<String> = word.chars().map(String::from).collect();
        if c.get(2).is_some() {
            // "GPUs" → "G P U's" where the lexicon has it; otherwise just the letters.
            let last = letters.last_mut().unwrap();
            let s = format!("{last}'s");
            if lexicon.contains(&s) {
                *last = s;
            }
        }
        letters.join(" ")
    })
    .into_owned()
}

/// Metric prefixes and spellings the lexicons may know them by.
const PREFIXES: &[(&str, &[&str])] = &[
    ("kilo", &["kilo"]),
    ("milli", &["milli", "milly"]),
    ("centi", &["centi", "senti"]),
    ("mega", &["mega"]),
    ("giga", &["giga", "gigga"]),
    ("tera", &["tera", "terra"]),
];

/// Lexicon-aware fixes for words the model would drop: contractions of known
/// words (`'s` is left to the phonemizer's suffix rules), camelCase, metric prefixes ("kilometers" →
/// "kilo meters"). A word that stays unknown is left for the log.
fn respell(text: &str, lexicon: &Lexicon) -> String {
    static WORD: LazyLock<Regex> = LazyLock::new(|| re(r"\b[A-Za-z]+(?:'[A-Za-z]+)?\b"));
    if lexicon.is_empty() {
        return text.to_owned();
    }
    WORD.replace_all(text, |c: &Captures| {
        let word = &c[0];
        if lexicon.contains(word) {
            return word.to_owned();
        }
        if let Some((base, suffix)) = word.split_once('\'') {
            if !lexicon.contains(base) {
                return word.to_owned();
            }
            let lower = base.to_lowercase();
            let wh = matches!(lower.as_str(), "what" | "how" | "where" | "why" | "when");
            let full = match suffix.to_lowercase().as_str() {
                "ll" => "will",
                "d" if wh => "did",
                "d" => "would",
                "re" => "are",
                "ve" => "have",
                _ => "",
            };
            return if full.is_empty() {
                base.to_owned()
            } else {
                format!("{base} {full}")
            };
        }
        if let Some(parts) = camel_parts(word).filter(|p| p.iter().all(|p| lexicon.contains(p))) {
            return parts.join(" ");
        }
        let lower = word.to_lowercase();
        for (prefix, spellings) in PREFIXES {
            let Some(base) = lower.strip_prefix(prefix) else {
                continue;
            };
            let spelled = spellings.iter().find(|s| lexicon.contains(s));
            if let Some(spelled) = spelled.filter(|_| base.len() > 2 && lexicon.contains(base)) {
                return format!("{spelled} {base}");
            }
        }
        compound_parts(word, lexicon).unwrap_or_else(|| word.to_owned())
    })
    .into_owned()
}

/// "Firefox" → "Fire fox": two known words of three letters or more, the most
/// even split first.
fn compound_parts(word: &str, lexicon: &Lexicon) -> Option<String> {
    if word.len() < 6 || !word.is_ascii() {
        return None;
    }
    let mut splits: Vec<usize> = (3..=word.len() - 3).collect();
    splits.sort_by_key(|&i| i.abs_diff(word.len() - i));
    splits
        .into_iter()
        .find(|&i| lexicon.contains(&word[..i]) && lexicon.contains(&word[i..]))
        .map(|i| format!("{} {}", &word[..i], &word[i..]))
}

/// "GitHub" → ["Git", "Hub"]; None unless every part has at least two letters.
fn camel_parts(word: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, c) in word.char_indices().skip(1) {
        if c.is_uppercase() && word[..i].ends_with(char::is_lowercase) {
            parts.push(&word[start..i]);
            start = i;
        }
    }
    parts.push(&word[start..]);
    (parts.len() > 1 && parts.iter().all(|p| p.len() > 1)).then_some(parts)
}

/// Collapses whitespace and stray punctuation left by the passes.
fn tidy(text: &str) -> String {
    static SPACE_BEFORE: LazyLock<Regex> = LazyLock::new(|| re(r"\s+([,.!?;:])"));
    static REPEATS: LazyLock<Regex> = LazyLock::new(|| re(r"([,;:])[\s,;:]*([,.!?;:])"));
    // Quote-like apostrophes at word edges; sherpa would glue them to the word.
    static EDGE_QUOTES: LazyLock<Regex> = LazyLock::new(|| re(r"(^|\s)'+|'+($|[\s,.!?;:])"));
    let t = EDGE_QUOTES.replace_all(text, "$1$2");
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = SPACE_BEFORE.replace_all(&t, "$1");
    let t = REPEATS.replace_all(&t, "$2");
    t.trim_start_matches([',', ';', ':', ' ']).trim().to_owned()
}

/// Re-emit a consumed abbreviation dot when it also ended the sentence.
fn keep_stop(consumed: bool, text: &str, end: usize) -> &'static str {
    let rest = text[end..].trim_start();
    if consumed && (rest.is_empty() || rest.starts_with(char::is_uppercase)) {
        "."
    } else {
        ""
    }
}

fn dotted(s: &str) -> String {
    s.split('.')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" dot ")
}

fn num(s: &str) -> u64 {
    s.replace(',', "").parse().unwrap_or(0)
}

fn month(s: &str) -> &'static str {
    MONTHS.iter().find(|m| m.starts_with(&s[..3])).unwrap()
}

fn weekday(s: &str) -> &'static str {
    const DAYS: &[&str] = &[
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    DAYS.iter().find(|d| d.starts_with(&s[..3])).unwrap()
}

fn date(m: u64, d: u64, y: Option<&str>) -> Option<String> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut s = format!("{} {}", MONTHS[m as usize - 1], ordinal(d));
    if let Some(y) = y {
        s += &format!(", {}", year(num(y)));
    }
    Some(s)
}

fn money(symbol: &str, amount: &str, scale: Option<&str>) -> String {
    let (one, many, sub_one, sub_many) = match symbol {
        "$" => ("dollar", "dollars", "cent", "cents"),
        "€" => ("euro", "euros", "cent", "cents"),
        "£" => ("pound", "pounds", "penny", "pence"),
        "¥" => ("yen", "yen", "", ""),
        _ => ("rupee", "rupees", "", ""),
    };
    if let Some(scale) = scale {
        let scale = match scale {
            "k" | "K" | "thousand" => "thousand",
            "m" | "M" | "million" => "million",
            "B" | "bn" | "billion" => "billion",
            _ => "trillion",
        };
        return format!("{} {scale} {many}", number(amount));
    }
    let (whole, frac) = amount.split_once('.').unwrap_or((amount, ""));
    let whole_n = num(whole);
    let unit = if whole_n == 1 { one } else { many };
    match frac.len() {
        0 => format!("{} {unit}", number(whole)),
        2 if !sub_one.is_empty() => {
            let cents = num(frac);
            let sub = if cents == 1 { sub_one } else { sub_many };
            match (whole_n, cents) {
                (_, 0) => format!("{} {unit}", number(whole)),
                (0, _) => format!("{} {sub}", cardinal(cents)),
                _ => format!("{} {unit} and {} {sub}", number(whole), cardinal(cents)),
            }
        }
        _ => format!("{} {many}", number(amount)),
    }
}

fn fraction(n: u64, d: u64) -> String {
    let name = match d {
        2 => "half",
        4 => "quarter",
        _ => return format!("{} {}", cardinal(n), plural_if(&ordinal(d), n != 1)),
    };
    format!("{} {}", cardinal(n), plural_if(name, n != 1))
}

fn plural_if(word: &str, plural: bool) -> String {
    if !plural {
        word.to_owned()
    } else if word == "half" {
        "halves".into()
    } else {
        format!("{word}s")
    }
}

/// "twenty" → "twenties", "nineteen ninety" → "nineteen nineties".
fn plural(words: &str) -> String {
    match words.strip_suffix('y') {
        Some(stem) => format!("{stem}ies"),
        None => format!("{words}s"),
    }
}

const ONES: &[&str] = &[
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: &[&str] = &[
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];
const SCALES: &[&str] = &[
    "",
    "thousand",
    "million",
    "billion",
    "trillion",
    "quadrillion",
];

pub fn cardinal(n: u64) -> String {
    if n == 0 {
        return "zero".into();
    }
    let mut groups = Vec::new();
    let mut rest = n;
    let mut scale = 0;
    while rest > 0 {
        let g = rest % 1000;
        if g > 0 {
            let mut s = below_thousand(g);
            if scale > 0 {
                s += " ";
                s += SCALES[scale];
            }
            groups.push(s);
        }
        rest /= 1000;
        scale += 1;
    }
    groups.reverse();
    groups.join(" ")
}

fn below_thousand(n: u64) -> String {
    let (h, r) = (n / 100, n % 100);
    let tail = match r {
        0 => String::new(),
        1..=19 => ONES[r as usize].into(),
        _ if r % 10 == 0 => TENS[(r / 10) as usize].into(),
        _ => format!("{}-{}", TENS[(r / 10) as usize], ONES[(r % 10) as usize]),
    };
    match (h, tail.is_empty()) {
        (0, _) => tail,
        (h, true) => format!("{} hundred", ONES[h as usize]),
        (h, false) => format!("{} hundred {tail}", ONES[h as usize]),
    }
}

pub fn ordinal(n: u64) -> String {
    let words = cardinal(n);
    let split = words.rfind([' ', '-']).map_or(0, |i| i + 1);
    let (head, last) = words.split_at(split);
    let last = match last {
        "one" => "first".into(),
        "two" => "second".into(),
        "three" => "third".into(),
        "five" => "fifth".into(),
        "eight" => "eighth".into(),
        "nine" => "ninth".into(),
        "twelve" => "twelfth".into(),
        w if w.ends_with('y') => format!("{}ieth", &w[..w.len() - 1]),
        w => format!("{w}th"),
    };
    format!("{head}{last}")
}

/// "1999" → "nineteen ninety-nine", "2005" → "two thousand five", "1900" → "nineteen hundred".
pub fn year(n: u64) -> String {
    if !(1100..=2099).contains(&n) || (2000..=2009).contains(&n) {
        return cardinal(n);
    }
    let (hi, lo) = (n / 100, n % 100);
    match lo {
        0 => format!("{} hundred", cardinal(hi)),
        1..=9 => format!("{} oh {}", cardinal(hi), cardinal(lo)),
        _ => format!("{} {}", cardinal(hi), cardinal(lo)),
    }
}

/// A written number: "1,234", "12.5", "007", "4155550100".
fn number(s: &str) -> String {
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    let digits_only = whole.replace(',', "");
    let mut out = if (digits_only.len() > 1 && digits_only.starts_with('0') && !whole.contains(','))
        || digits_only.len() > 15
        || (digits_only.len() > 9 && !whole.contains(','))
    {
        digit_words(&digits_only)
    } else {
        cardinal(num(&digits_only))
    };
    if !frac.is_empty() {
        out += " point ";
        out += &digit_words(frac);
    }
    out
}

fn digit_words(s: &str) -> String {
    s.chars()
        .filter_map(|c| c.to_digit(10))
        .map(|d| ONES[d as usize])
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(text: &str) -> String {
        let lex = Lexicon::from_words(["ok", "nasa", "linux", "us", "it", "it's", "u's", "very"]);
        normalize(text, &lex)
    }

    #[test]
    fn numbers() {
        assert_eq!(cardinal(0), "zero");
        assert_eq!(cardinal(21), "twenty-one");
        assert_eq!(cardinal(100), "one hundred");
        assert_eq!(
            cardinal(1_234_567),
            "one million two hundred thirty-four thousand five hundred sixty-seven"
        );
        assert_eq!(ordinal(23), "twenty-third");
        assert_eq!(ordinal(40), "fortieth");
        assert_eq!(ordinal(112), "one hundred twelfth");
        assert_eq!(year(1999), "nineteen ninety-nine");
        assert_eq!(year(2026), "twenty twenty-six");
        assert_eq!(year(2005), "two thousand five");
        assert_eq!(year(1905), "nineteen oh five");
        assert_eq!(
            n("I have 3 cats and 1,500 fish."),
            "I have three cats and one thousand five hundred fish."
        );
        assert_eq!(
            n("It's 12.5 or -3 or 0.05."),
            "It's twelve point five or minus three or zero point zero five."
        );
        assert_eq!(
            n("Call 4155550100 or 007."),
            "Call four one five five five five zero one zero zero or zero zero seven."
        );
        assert_eq!(
            n("Version 3.10.2 is out."),
            "Version three point ten point two is out."
        );
    }

    #[test]
    fn times() {
        assert_eq!(n("It is 3:45 PM."), "It is three forty-five P M.");
        assert_eq!(
            n("At 3:05 p.m. we leave."),
            "At three oh five P M we leave."
        );
        assert_eq!(n("Wake at 7am."), "Wake at seven A M.");
        assert_eq!(
            n("By 9:00 or 15:30 or 18:00."),
            "By nine o'clock or fifteen thirty or eighteen hundred."
        );
        assert_eq!(n("Meet at 5 p.m. Then eat."), "Meet at five P M. Then eat.");
    }

    #[test]
    fn dates() {
        assert_eq!(
            n("on Nov 23, 2026."),
            "on November twenty-third, twenty twenty-six."
        );
        assert_eq!(
            n("on 2026-11-23."),
            "on November twenty-third, twenty twenty-six."
        );
        assert_eq!(n("on 23rd March"), "on the twenty-third of March");
        assert_eq!(n("Mon, Nov. 24"), "Monday, November twenty-fourth");
        assert_eq!(
            n("since 1999 and in the 1990s and the '80s"),
            "since nineteen ninety-nine and in the nineteen nineties and the eighties"
        );
        assert_eq!(
            n("from 2020-2026"),
            "from twenty twenty to twenty twenty-six"
        );
        assert_eq!(n("May 2026"), "May twenty twenty-six");
        assert_eq!(n("12/25/2026"), "December twenty-fifth, twenty twenty-six");
    }

    #[test]
    fn money_units_percent() {
        assert_eq!(
            n("$5.99 and $1 and €20"),
            "five dollars and ninety-nine cents and one dollar and twenty euros"
        );
        assert_eq!(
            n("$1.5 million or $5K"),
            "one point five million dollars or five thousand dollars"
        );
        assert_eq!(n("£3.50"), "three pounds and fifty pence");
        assert_eq!(
            n("about 40% of 12.5 GB"),
            "about forty percent of twelve point five gigabytes"
        );
        assert_eq!(
            n("1 km at 60 km/h, 3ms, 2x"),
            "one kilometer at sixty kilometers an hour, three milliseconds, two times"
        );
        assert_eq!(
            n("It's 21°C or 70 °F"),
            "It's twenty-one degrees Celsius or seventy degrees Fahrenheit"
        );
        assert_eq!(
            n("the 3rd of 1/2 and 3/4"),
            "the third of one half and three quarters"
        );
        assert_eq!(
            n("3-5 days, 10–20 min"),
            "three to five days, ten to twenty minutes"
        );
    }

    #[test]
    fn abbreviations_and_acronyms() {
        assert_eq!(
            n("Dr. Smith vs. Mr. Jones, e.g. here, etc."),
            "Doctor Smith versus Mister Jones, for example here, and so on."
        );
        assert_eq!(n("Your GPU uses the API."), "Your G P U uses the A P I.");
        assert_eq!(
            n("NASA is OK. The US. IT works. VERY good."),
            "NASA is OK. The U S. I T works. VERY good."
        );
        assert_eq!(n("GPUs and CPUs"), "G P U's and C P U's");
        assert_eq!(n("an MP3 in 4K"), "an M P three in four K");
    }

    #[test]
    fn respells_unknown_words() {
        let lex = Lexicon::from_words([
            "kilo",
            "meters",
            "milly",
            "seconds",
            "terabytes",
            "five",
            "three",
            "two",
            "and",
            "what",
            "that",
            "today",
            "weather",
            "should",
            "git",
            "hub",
            "on",
            "nvidia",
            "is",
            "fire",
            "fox",
            "uses",
            "we",
            "go",
        ]);
        assert_eq!(
            normalize("5 km and 3 ms, 2 TB", &lex),
            "five kilo meters and three milly seconds, two terabytes"
        );
        assert_eq!(
            normalize(
                "What's that? Today's weather. Should've. We'd go. GitHub is on.",
                &lex
            ),
            "What's that? Today's weather. Should have. We would go. Git Hub is on."
        );
        assert_eq!(normalize("Firefox uses", &lex), "Fire fox uses");
    }

    #[test]
    fn urls_and_symbols() {
        assert_eq!(n("See https://vivus.org/docs/x?y=1."), "See vivus dot org.");
        assert_eq!(
            n("Mail me@example.com or www.github.com"),
            "Mail me at example dot com or github dot com"
        );
        assert_eq!(n("Edit config.toml"), "Edit config dot T O M L");
        assert_eq!(n("Open ~/notes.md"), "Open home slash notes dot M D");
        assert_eq!(
            n("uses ~2 GB at -5°C"),
            "uses about two gigabytes at minus five degrees Celsius"
        );
        assert_eq!(
            n("He said \"hello\" (loudly) & left 😀."),
            "He said hello, loudly, and left."
        );
        assert_eq!(n("## Title"), "Title");
        assert_eq!(
            n("C++ and/or Rust, w/ #1 fan"),
            "C plus plus and or Rust, with number one fan"
        );
        assert_eq!(n("it’s fine"), "it's fine");
        assert_eq!(n("foo/bar"), "foo slash bar");
    }
}
