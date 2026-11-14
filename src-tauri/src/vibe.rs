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

pub async fn recommend(ctx: &Arc<Ctx>, body: &Value, limit: usize) -> Vec<Value> {
    let mut rng = Rng::new();
    let settings = body.get("settings").cloned().unwrap_or(Value::Null);
    let language = s(settings.get("language")).to_string();
    let familiarity = s(settings.get("familiarity")).to_string();

    // Who the listener likes: strongest artists first, then the artists of seed tracks and favourites.
    let mut artists: Vec<String> = Vec::new();
    let mut seen_artist = HashSet::new();
    let mut add_artist = |name: &str, artists: &mut Vec<String>| {
        let first = name.split([',', '&', ';']).next().unwrap_or("").trim().to_string();
        if !first.is_empty() && first.to_lowercase() != "unknown artist" && seen_artist.insert(norm(&first)) {
            artists.push(first);
        }
    };
    for a in body.pointer("/clientProfile/topArtists").and_then(Value::as_array).into_iter().flatten() {
        add_artist(s(Some(a)), &mut artists);
    }
    for key in ["seeds", "favorites", "history"] {
        for t in body.get(key).and_then(Value::as_array).into_iter().flatten().take(20) {
            add_artist(s(t.get("artist")), &mut artists);
        }
    }
    if language == "russian" {
        let ru: Vec<String> = artists.iter().filter(|a| has_cyrillic(a)).cloned().collect();
        if !ru.is_empty() {
            artists = ru;
        }
    } else if language == "foreign" {
        let en: Vec<String> = artists.iter().filter(|a| !has_cyrillic(a)).cloned().collect();
        if !en.is_empty() {
            artists = en;
        }
    }
    // Mostly the strongest artists, sometimes a deeper one, so the stream does not loop.
    let mut picked: Vec<String> = Vec::new();
    let head = artists.len().min(6);
    let mut top: Vec<String> = artists[..head].to_vec();
    rng.shuffle(&mut top);
    picked.extend(top.into_iter().take(2));
    if artists.len() > head {
        picked.push(artists[head + (rng.next() % (artists.len() - head) as u64) as usize].clone());
    }

    let popular = familiarity == "popular";
    let mut taste: Vec<Value> = Vec::new();
    let radios = match picked.len() {
        0 => (Vec::new(), Vec::new(), Vec::new()),
        1 => (artist_radio(ctx, &picked[0], popular).await, Vec::new(), Vec::new()),
        2 => {
            let (a, b) = tokio::join!(artist_radio(ctx, &picked[0], popular), artist_radio(ctx, &picked[1], popular));
            (a, b, Vec::new())
        }
        _ => tokio::join!(
            artist_radio(ctx, &picked[0], popular),
            artist_radio(ctx, &picked[1], popular),
            artist_radio(ctx, &picked[2], popular)
        ),
    };
    taste.extend(radios.0);
    taste.extend(radios.1);
    taste.extend(radios.2);
    if popular {
        taste.extend(ytm::deezer_chart(ctx, 40).await);
        taste.sort_by_key(|t| std::cmp::Reverse(t.get("rank").and_then(Value::as_u64).unwrap_or(0)));
    } else {
        rng.shuffle(&mut taste);
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

    // What must not come back: disliked artists and tracks, already queued tracks, known artists when asked for new music.
    let list = |p: &str| -> HashSet<String> {
        body.pointer(p).and_then(Value::as_array).into_iter().flatten().map(|v| s(Some(v)).to_lowercase()).collect()
    };
    let avoid_ids = list("/avoid/ids");
    let avoid_artists: HashSet<String> = list("/avoid/artists").iter().map(|a| norm(a)).collect();
    let known: HashSet<String> = if familiarity == "unknown" {
        list("/knownArtists").iter().map(|a| norm(a)).collect()
    } else {
        HashSet::new()
    };
    let instrumental = language == "instrumental";

    let mut out: Vec<Value> = Vec::new();
    let mut seen = HashSet::new();
    let mut per_artist: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let (mut ti, mut hi) = (taste.into_iter(), themed.into_iter());
    let themed_only = instrumental || picked.is_empty();
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
        if avoid_ids.contains(&s(t.get("id")).to_lowercase()) || avoid_artists.contains(&a) || known.contains(&a) {
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
