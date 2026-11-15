// "My Vibe": an endless stream built from the listener's taste (Deezer artist radios)
// and, when asked for, a mood / activity / language (Deezer playlists found by keyword).
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::{Ctx, ytm};

fn s<'a>(v: Option<&'a Value>) -> &'a str {
    v.and_then(Value::as_str).unwrap_or("")
}

fn norm(t: &str) -> String {
    t.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

fn has_cyrillic(t: &str) -> bool {
    t.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c))
}

/// The writing system of a text: 0 = Latin or unknown, otherwise one code per script (used to keep the stream in the listener's language).
fn script_of(t: &str) -> u8 {
    for c in t.chars() {
        let n = c as u32;
        let code = match n {
            0x0400..=0x04FF => 1,
            0xAC00..=0xD7AF | 0x1100..=0x11FF => 2,
            0x3040..=0x30FF | 0x4E00..=0x9FFF => 3,
            0x0E00..=0x0E7F => 4,
            0x0590..=0x05FF => 5,
            0x0600..=0x06FF => 6,
            0x0900..=0x097F => 7,
            0x0370..=0x03FF => 8,
            _ => 0,
        };
        if code != 0 {
            return code;
        }
    }
    0
}

struct Rng(u64);
impl Rng {
    fn new() -> Self {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
        Rng(n | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
    }
}

fn track(it: &Value) -> Option<Value> {
    let id = it.get("id").and_then(Value::as_u64)?;
    let title = s(it.get("title"));
    let artist = s(it.pointer("/artist/name"));
    if title.is_empty() || artist.is_empty() {
        return None;
    }
    let cover = ["cover_xl", "cover_big", "cover_medium"]
        .iter()
        .map(|k| s(it.pointer(&format!("/album/{k}"))))
        .find(|c| !c.is_empty())
        .unwrap_or("")
        .to_string();
    Some(json!({
        "id": format!("dz-{id}"), "title": title, "artist": artist, "album": s(it.pointer("/album/title")),
        "thumbnail": cover, "cover": cover,
        "duration": it.get("duration").and_then(Value::as_u64).unwrap_or(0),
        "platform": "ytmusic", "url": "",
        "rank": it.get("rank").and_then(Value::as_u64).unwrap_or(0)
    }))
}

async fn dz(ctx: &Ctx, path: &str) -> Vec<Value> {
    let url = format!("https://api.deezer.com/{path}");
    let Ok(resp) = ctx.client.get(url).timeout(Duration::from_secs(7)).send().await else { return Vec::new() };
    let Ok(root) = resp.json::<Value>().await else { return Vec::new() };
    root.get("data").and_then(Value::as_array).cloned().unwrap_or_default()
}

// Tracks by artists similar to `name` (and by the artist): the taste part of the stream.
async fn artist_radio(ctx: &Ctx, name: &str, popular: bool) -> Vec<Value> {
    let found = dz(ctx, &format!("search/artist?q={}&limit=1", ytm::urlencode(name))).await;
    let Some(id) = found.first().and_then(|a| a.get("id")).and_then(Value::as_u64) else { return Vec::new() };
    let mut out: Vec<Value> = dz(ctx, &format!("artist/{id}/radio")).await.iter().filter_map(track).collect();
    if popular {
        out.extend(dz(ctx, &format!("artist/{id}/top?limit=10")).await.iter().filter_map(track));
    }
    out
}

// Tracks from a public playlist that matches a mood / activity keyword.
async fn keyword_pool(ctx: &Ctx, keyword: &str, rng: &mut Rng) -> Vec<Value> {
    let lists = dz(ctx, &format!("search/playlist?q={}&limit=8", ytm::urlencode(keyword))).await;
    let usable: Vec<u64> = lists
        .iter()
        .filter(|p| p.get("nb_tracks").and_then(Value::as_u64).unwrap_or(0) >= 20)
        .filter_map(|p| p.get("id").and_then(Value::as_u64))
        .collect();
    if usable.is_empty() {
        return Vec::new();
    }
    let id = usable[(rng.next() % usable.len() as u64) as usize];
    dz(ctx, &format!("playlist/{id}/tracks?limit=80")).await.iter().filter_map(track).collect()
}

fn keyword(settings: &Value, russian: bool) -> Option<String> {
    let pick = |key: &str, table: &[(&str, &str, &str)]| -> Option<String> {
        let v = s(settings.get(key));
        table.iter().find(|(k, _, _)| *k == v).map(|(_, en, ru)| if russian { ru.to_string() } else { en.to_string() })
    };
    let activity = pick(
        "activity",
        &[
            ("wake", "morning", "доброе утро"),
            ("commute", "road trip", "в дорогу"),
            ("work", "focus", "для работы"),
            ("workout", "workout", "тренировка"),
            ("sleep", "sleep", "для сна"),
        ],
    );
    let mood = pick(
        "mood",
        &[
            ("energetic", "energy", "энергичная музыка"),
            ("happy", "happy", "весёлая музыка"),
            ("calm", "chill", "спокойная музыка"),
            ("sad", "sad", "грустная музыка"),
        ],
    );
    let mut words: Vec<String> = [activity, mood].into_iter().flatten().collect();
    if s(settings.get("language")) == "instrumental" {
        words.push("instrumental".into());
    }
    if words.is_empty() { None } else { Some(words.join(" ")) }
}

fn is_youtube_id(id: &str) -> bool {
    id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn track_key(artist: &str, title: &str) -> String {
    format!("{}::{}", norm(artist), norm(title))
}

/// Pick `n` items without repeats; heavier items are more likely.
fn pick_weighted(rng: &mut Rng, items: &[(usize, f64)], n: usize) -> Vec<usize> {
    let mut pool: Vec<(usize, f64)> = items.to_vec();
    let mut out = Vec::new();
    while out.len() < n && !pool.is_empty() {
        let total: f64 = pool.iter().map(|x| x.1).sum();
        let mut r = (rng.next() % 10_000) as f64 / 10_000.0 * total;
        let mut at = pool.len() - 1;
        for (i, x) in pool.iter().enumerate() {
            if r < x.1 {
                at = i;
                break;
            }
            r -= x.1;
        }
        out.push(pool.remove(at).0);
    }
    out
}

fn views_of(t: &Value) -> f64 {
    let p = s(t.get("playsText")).trim().to_uppercase();
    let (num, mult) = match p.chars().last() {
        Some('M') => (&p[..p.len() - 1], 1e6),
        Some('K') => (&p[..p.len() - 1], 1e3),
        Some('B') => (&p[..p.len() - 1], 1e9),
        _ => (p.as_str(), 1.0),
    };
    num.replace(',', "").parse::<f64>().unwrap_or(0.0) * mult
}

pub async fn recommend(ctx: &Arc<Ctx>, body: &Value, limit: usize) -> Vec<Value> {
    let mut rng = Rng::new();
    let settings = body.get("settings").cloned().unwrap_or(Value::Null);
    let language = s(settings.get("language")).to_string();
    let familiarity = s(settings.get("familiarity")).to_string();
    let region = s(body.get("region")).to_string();

    let list = |p: &str| -> HashSet<String> {
        body.pointer(p).and_then(Value::as_array).into_iter().flatten().map(|v| s(Some(v)).to_lowercase()).collect()
    };
    let avoid_ids = list("/avoid/ids");
    let avoid_artists: HashSet<String> = list("/avoid/artists").iter().map(|a| norm(a)).collect();
    let avoid_keys: HashSet<String> = list("/avoid/keys")
        .iter()
        .map(|k| {
            let mut it = k.splitn(2, "::");
            format!("{}::{}", norm(it.next().unwrap_or("")), norm(it.next().unwrap_or("")))
        })
        .collect();
    let known: HashSet<String> = if familiarity == "unknown" {
        list("/knownArtists").iter().map(|a| norm(a)).collect()
    } else {
        HashSet::new()
    };

    let arr = |k: &str| -> Vec<Value> { body.get(k).and_then(Value::as_array).cloned().unwrap_or_default() };
    let favorites = arr("favorites");
    let history = arr("history");
    let liked_now = arr("likedNow");
    let favorite_keys: HashSet<String> = favorites.iter().map(|t| track_key(s(t.get("artist")), s(t.get("title")))).collect();

    // Seeds: everything the listener liked (newer likes count more), what they were just given a like for, and a little of the recent history.
    let mut seeds: Vec<Value> = Vec::new();
    let mut weights: Vec<(usize, f64)> = Vec::new();
    let mut seen_seed = HashSet::new();
    let mut push_seed = |t: &Value, w: f64, seeds: &mut Vec<Value>, weights: &mut Vec<(usize, f64)>| {
        let a = norm(s(t.get("artist")).split([',', '&', ';']).next().unwrap_or(""));
        if a.is_empty() || avoid_artists.contains(&a) || s(t.get("platform")) == "local" {
            return;
        }
        if !seen_seed.insert(track_key(s(t.get("artist")), s(t.get("title")))) {
            return;
        }
        seeds.push(t.clone());
        weights.push((seeds.len() - 1, w));
    };
    for t in &liked_now {
        push_seed(t, 6.0, &mut seeds, &mut weights);
    }
    for (i, t) in favorites.iter().enumerate() {
        push_seed(t, 1.0 + 2.0 / (1.0 + i as f64 * 0.08), &mut seeds, &mut weights);
    }
    for t in history.iter().take(12) {
        push_seed(t, 0.6, &mut seeds, &mut weights);
    }

    // The language of the listener's own music: a stream for someone who listens to Russian, Korean, ... music stays mostly in it.
    let own_texts: Vec<String> = seeds.iter().take(40).map(|t| format!("{} {}", s(t.get("title")), s(t.get("artist")))).collect();
    let mut counts = [0usize; 9];
    for a in &own_texts {
        counts[script_of(a) as usize] += 1;
    }
    let own = own_texts.len();
    let dominant: u8 = (1..9u8).find(|k| own >= 3 && counts[*k as usize] * 10 >= own * 7).unwrap_or(0);

    // The radios of three seeds are the taste part of the stream.
    let picked = pick_weighted(&mut rng, &weights, 3);
    let mut radio_ids: Vec<String> = Vec::new();
    for i in &picked {
        let t = &seeds[*i];
        let id = s(t.get("id"));
        if is_youtube_id(id) {
            radio_ids.push(id.to_string());
        } else if let Some(v) = ytm::find_video_id(ctx, s(t.get("artist")), s(t.get("title"))).await {
            radio_ids.push(v);
        }
    }
    let mut radios: Vec<Vec<Value>> = Vec::new();
    {
        let (i0, i1, i2) = (
            radio_ids.first().cloned().unwrap_or_default(),
            radio_ids.get(1).cloned().unwrap_or_default(),
            radio_ids.get(2).cloned().unwrap_or_default(),
        );
        let (a, b, c) = tokio::join!(ytm::yt_radio(ctx, &i0), ytm::yt_radio(ctx, &i1), ytm::yt_radio(ctx, &i2));
        radios.extend([a, b, c]);
    }
    let popular = familiarity == "popular";

    // How many of the radios agree on an artist is the strongest sign that the artist fits.
    let mut artist_hits: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for r in &radios {
        let mut once = HashSet::new();
        for t in r {
            let a = norm(s(t.get("artist")).split([',', '&', ';']).next().unwrap_or(""));
            if once.insert(a.clone()) {
                *artist_hits.entry(a).or_insert(0) += 1;
            }
        }
    }
    let mut scored: Vec<(f64, Value)> = Vec::new();
    for r in radios {
        let n = r.len().max(1) as f64;
        for (pos, t) in r.into_iter().enumerate() {
            let a = norm(s(t.get("artist")).split([',', '&', ';']).next().unwrap_or(""));
            let key = track_key(s(t.get("artist")), s(t.get("title")));
            if avoid_ids.contains(&s(t.get("id")).to_lowercase()) || avoid_artists.contains(&a) || avoid_keys.contains(&key) || known.contains(&a) {
                continue;
            }
            // tracks the listener already liked are not "new"; the "Favourite" character mixes them in separately
            if favorite_keys.contains(&key) {
                continue;
            }
            let text_all = format!("{} {}", s(t.get("title")), s(t.get("artist")));
            let cyr = has_cyrillic(&text_all);
            if (language == "russian" && !cyr) || (language == "foreign" && cyr) {
                continue;
            }
            let mut score = 1.0 - pos as f64 / n * 0.4 + (rng.next() % 1000) as f64 / 1000.0 * 1.1;
            score += 0.9 * (*artist_hits.get(&a).unwrap_or(&1) as f64 - 1.0);
            if popular {
                score += (views_of(&t).max(1.0).log10() - 5.0).max(0.0) * 0.5;
            }
            if language.is_empty() && dominant != 0 && script_of(&text_all) != dominant {
                score -= 1.6;
            }
            scored.push((score, t));
        }
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut taste: Vec<Value> = scored.into_iter().map(|x| x.1).collect();

    // Nothing from YouTube (offline, no likes): artist radios on Deezer, then the country chart.
    let mut chart: Vec<Value> = Vec::new();
    if taste.is_empty() {
        let mut names: Vec<String> = Vec::new();
        for t in seeds.iter().take(12) {
            let first = s(t.get("artist")).split([',', '&', ';']).next().unwrap_or("").trim().to_string();
            if !first.is_empty() && !names.contains(&first) {
                names.push(first);
            }
        }
        rng.shuffle(&mut names);
        for name in names.iter().take(2) {
            taste.extend(artist_radio(ctx, name, popular).await);
        }
        if taste.is_empty() {
            chart = ytm::yt_chart(ctx, &region).await.1;
            taste = chart.clone();
            rng.shuffle(&mut taste);
        }
    } else if seeds.len() < 3 {
        // a new listener: the chart of the country is part of the stream
        chart = ytm::yt_chart(ctx, &region).await.1;
        let mut c = chart.clone();
        rng.shuffle(&mut c);
        taste.extend(c.into_iter().take(15));
    }

    let russian_words = language == "russian";
    let mut themed: Vec<Value> = match keyword(&settings, russian_words) {
        Some(k) => keyword_pool(ctx, &k, &mut rng).await,
        None => Vec::new(),
    };
    rng.shuffle(&mut themed);
    if taste.is_empty() && themed.is_empty() {
        taste = ytm::deezer_chart(ctx, 60).await;
        rng.shuffle(&mut taste);
    }
    let instrumental = language == "instrumental";

    // Optional: an AI model the listener connected themselves re-orders the best candidates.
    if let Some(ai) = body.get("ai").filter(|a| !s(a.get("key")).is_empty()) {
        let head: Vec<Value> = taste.iter().take(40).cloned().collect();
        let liked: Vec<String> = favorites.iter().take(30).map(|t| format!("{} - {}", s(t.get("artist")), s(t.get("title")))).collect();
        let disliked: Vec<String> = body
            .get("dislikedList")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(20)
            .map(|v| s(Some(v)).to_string())
            .collect();
        if let Some(order) = crate::ai::rerank(ctx, ai, &liked, &disliked, &head, &settings).await {
            let mut reordered: Vec<Value> = order.iter().filter_map(|i| head.get(*i).cloned()).collect();
            let used: HashSet<usize> = order.into_iter().collect();
            reordered.extend(head.iter().enumerate().filter(|(i, _)| !used.contains(i)).map(|(_, t)| t.clone()));
            reordered.extend(taste.iter().skip(40).cloned());
            taste = reordered;
        }
    }

    let mut out: Vec<Value> = Vec::new();
    let mut seen = HashSet::new();
    let mut per_artist: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let (mut ti, mut hi) = (taste.into_iter(), themed.into_iter());
    let themed_only = instrumental || seeds.is_empty() && chart.is_empty();
    let mut turn = 0usize;
    loop {
        if out.len() >= limit {
            break;
        }
        // With a mood or activity set, every second track comes from the themed pool.
        let from_themed = themed_only || turn % 2 == 1;
        turn += 1;
        let next = if from_themed { hi.next().or_else(|| ti.next()) } else { ti.next().or_else(|| hi.next()) };
        let Some(t) = next else { break };
        let (title, artist) = (s(t.get("title")).to_string(), s(t.get("artist")).to_string());
        let a = norm(&artist);
        if avoid_ids.contains(&s(t.get("id")).to_lowercase()) || avoid_artists.contains(&a) || known.contains(&a) || avoid_keys.contains(&track_key(&artist, &title)) {
            continue;
        }
        let cyr = has_cyrillic(&title) || has_cyrillic(&artist);
        if (language == "russian" && !cyr) || (language == "foreign" && cyr) {
            continue;
        }
        if !seen.insert(format!("{a}:{}", norm(&title))) {
            continue;
        }
        let n = per_artist.entry(a).or_insert(0);
        if *n >= 2 {
            continue;
        }
        *n += 1;
        out.push(t);
    }
    out
}
