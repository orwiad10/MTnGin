use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use rand::{rngs::StdRng, seq::SliceRandom, SeedableRng};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const SCRYFALL_BULK_ENDPOINT: &str = "https://api.scryfall.com/bulk-data";

#[derive(Parser)]
#[command(name = "mtngin")]
#[command(about = "MTG bot-vs-bot simulation engine")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Download or refresh local Scryfall card DB
    Init {
        #[arg(long, default_value = "data/scryfall-oracle-cards.json")]
        db_path: PathBuf,
    },
    /// Run bot simulations from a YAML config file
    Run {
        #[arg(long)]
        config: PathBuf,
    },
}

#[derive(Debug, Deserialize)]
struct RunConfig {
    format: String,
    iterations: usize,
    seed: Option<u64>,
    max_turns: Option<usize>,
    db_path: String,
    player1: PlayerConfig,
    player2: PlayerConfig,
    output_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlayerConfig {
    name: String,
    deck_path: String,
    starting_life: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct ScryfallBulkResponse {
    data: Vec<ScryfallBulkItem>,
}

#[derive(Debug, Deserialize)]
struct ScryfallBulkItem {
    #[serde(rename = "type")]
    data_type: String,
    download_uri: String,
}

#[derive(Debug, Deserialize)]
struct ScryfallCard {
    name: String,
    type_line: String,
    oracle_text: Option<String>,
    cmc: f32,
    power: Option<String>,
    toughness: Option<String>,
}

#[derive(Debug, Clone)]
struct CardProfile {
    name: String,
    kind: CardKind,
    cmc: u32,
    is_basic_land: bool,
}

#[derive(Debug, Clone)]
enum CardKind {
    Land,
    Creature { power: i32, toughness: i32 },
    Burn { damage: i32 },
    OtherSpell,
}

#[derive(Debug, Clone)]
struct DeckCard {
    name: String,
}

#[derive(Debug)]
struct PlayerState {
    name: String,
    life: i32,
    library: Vec<DeckCard>,
    hand: Vec<DeckCard>,
    battlefield: Vec<CreaturePermanent>,
    graveyard: Vec<DeckCard>,
    lands_in_play: u32,
    cards_seen: HashSet<String>,
}

#[derive(Debug, Clone)]
struct CreaturePermanent {
    card_name: String,
    power: i32,
    toughness: i32,
    summoning_sick: bool,
}

#[derive(Debug, Default, Serialize)]
struct SimulationReport {
    format: String,
    iterations: usize,
    player1_name: String,
    player2_name: String,
    player1_wins: usize,
    player2_wins: usize,
    draws: usize,
    avg_player1_life: f64,
    avg_player2_life: f64,
    player1_win_rate: f64,
    player2_win_rate: f64,
    draw_rate: f64,
    games: Vec<GameResult>,
}

#[derive(Debug, Serialize)]
struct GameResult {
    game_number: usize,
    winner: String,
    turns: usize,
    player1_life: i32,
    player2_life: i32,
    player1_cards_seen: Vec<String>,
    player2_cards_seen: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { db_path } => init_scryfall_db(&db_path),
        Commands::Run { config } => run_simulation(&config),
    }
}

fn init_scryfall_db(db_path: &Path) -> Result<()> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let client = reqwest::blocking::Client::new();
    let bulk: ScryfallBulkResponse = client
        .get(SCRYFALL_BULK_ENDPOINT)
        .send()
        .context("failed to fetch Scryfall bulk metadata")?
        .error_for_status()?
        .json()
        .context("failed to parse Scryfall bulk metadata JSON")?;

    let oracle_cards = bulk
        .data
        .into_iter()
        .find(|entry| entry.data_type == "oracle_cards")
        .ok_or_else(|| anyhow!("oracle_cards bulk data entry not found"))?;

    let bytes = client
        .get(oracle_cards.download_uri)
        .send()
        .context("failed to download Scryfall oracle cards")?
        .error_for_status()?
        .bytes()
        .context("failed to read downloaded bytes")?;

    fs::write(db_path, bytes)?;
    println!("Scryfall oracle DB saved to {}", db_path.display());
    Ok(())
}

fn run_simulation(config_path: &Path) -> Result<()> {
    let cfg_raw = fs::read_to_string(config_path)
        .with_context(|| format!("unable to read config file {}", config_path.display()))?;
    let cfg: RunConfig = serde_yaml::from_str(&cfg_raw).context("invalid YAML config")?;

    validate_run_config(&cfg)?;

    let card_db = load_card_db(Path::new(&cfg.db_path))?;
    let p1_deck = load_deck(Path::new(&cfg.player1.deck_path))?;
    let p2_deck = load_deck(Path::new(&cfg.player2.deck_path))?;
    validate_deck(&cfg.format, &p1_deck, &card_db)
        .with_context(|| format!("deck validation failed for player {}", cfg.player1.name))?;
    validate_deck(&cfg.format, &p2_deck, &card_db)
        .with_context(|| format!("deck validation failed for player {}", cfg.player2.name))?;

    let mut report = SimulationReport {
        format: cfg.format.clone(),
        iterations: cfg.iterations,
        player1_name: cfg.player1.name.clone(),
        player2_name: cfg.player2.name.clone(),
        ..SimulationReport::default()
    };

    let max_turns = cfg.max_turns.unwrap_or(30);
    let mut p1_life_sum = 0i64;
    let mut p2_life_sum = 0i64;

    let seed = cfg.seed.unwrap_or(42);
    let mut rng = StdRng::seed_from_u64(seed);
    for game_number in 1..=cfg.iterations {
        let result = simulate_game(
            game_number,
            &cfg,
            &p1_deck,
            &p2_deck,
            &card_db,
            max_turns,
            &mut rng,
        );

        p1_life_sum += result.player1_life as i64;
        p2_life_sum += result.player2_life as i64;

        match result.winner.as_str() {
            x if x == report.player1_name => report.player1_wins += 1,
            x if x == report.player2_name => report.player2_wins += 1,
            _ => report.draws += 1,
        }

        report.games.push(result);
    }

    report.avg_player1_life = p1_life_sum as f64 / cfg.iterations as f64;
    report.avg_player2_life = p2_life_sum as f64 / cfg.iterations as f64;
    report.player1_win_rate = report.player1_wins as f64 / cfg.iterations as f64;
    report.player2_win_rate = report.player2_wins as f64 / cfg.iterations as f64;
    report.draw_rate = report.draws as f64 / cfg.iterations as f64;

    let output = serde_json::to_string_pretty(&report)?;
    if let Some(path) = cfg.output_path {
        fs::write(&path, &output)?;
        println!("Simulation report written to {}", path);
    } else {
        println!("{output}");
    }

    Ok(())
}

#[derive(Clone, Copy)]
struct FormatRules {
    minimum_deck_size: usize,
    max_non_basic_copies: usize,
}

fn validate_run_config(cfg: &RunConfig) -> Result<()> {
    if cfg.iterations == 0 {
        return Err(anyhow!("iterations must be greater than 0"));
    }
    if cfg.player1.name.trim().is_empty() || cfg.player2.name.trim().is_empty() {
        return Err(anyhow!("player names must not be empty"));
    }
    Ok(())
}

fn rules_for_format(format: &str) -> Option<FormatRules> {
    match format.to_lowercase().as_str() {
        "standard" | "pioneer" | "modern" | "legacy" | "vintage" | "pauper" => Some(FormatRules {
            minimum_deck_size: 60,
            max_non_basic_copies: 4,
        }),
        "commander" => Some(FormatRules {
            minimum_deck_size: 100,
            max_non_basic_copies: 1,
        }),
        _ => None,
    }
}

fn validate_deck(
    format: &str,
    deck: &[DeckCard],
    card_db: &HashMap<String, CardProfile>,
) -> Result<()> {
    let rules =
        rules_for_format(format).ok_or_else(|| anyhow!("unsupported format '{}'", format))?;

    if deck.len() < rules.minimum_deck_size {
        return Err(anyhow!(
            "deck has {} cards; minimum for {} is {}",
            deck.len(),
            format,
            rules.minimum_deck_size
        ));
    }

    let mut counts: HashMap<String, usize> = HashMap::new();
    for card in deck {
        let key = normalize_name(&card.name);
        let profile = card_db
            .get(&key)
            .ok_or_else(|| anyhow!("card '{}' not found in local DB", card.name))?;

        let entry = counts.entry(profile.name.clone()).or_default();
        *entry += 1;

        let is_basic_land = profile.is_basic_land;
        if !is_basic_land && *entry > rules.max_non_basic_copies {
            return Err(anyhow!(
                "card '{}' has {} copies, max allowed in {} is {}",
                profile.name,
                entry,
                format,
                rules.max_non_basic_copies
            ));
        }
    }

    Ok(())
}

fn simulate_game(
    game_number: usize,
    cfg: &RunConfig,
    p1_deck: &[DeckCard],
    p2_deck: &[DeckCard],
    card_db: &HashMap<String, CardProfile>,
    max_turns: usize,
    rng: &mut StdRng,
) -> GameResult {
    let mut p1 = PlayerState {
        name: cfg.player1.name.clone(),
        life: cfg.player1.starting_life.unwrap_or(20),
        library: p1_deck.to_vec(),
        hand: Vec::new(),
        battlefield: Vec::new(),
        graveyard: Vec::new(),
        lands_in_play: 0,
        cards_seen: HashSet::new(),
    };

    let mut p2 = PlayerState {
        name: cfg.player2.name.clone(),
        life: cfg.player2.starting_life.unwrap_or(20),
        library: p2_deck.to_vec(),
        hand: Vec::new(),
        battlefield: Vec::new(),
        graveyard: Vec::new(),
        lands_in_play: 0,
        cards_seen: HashSet::new(),
    };

    p1.library.shuffle(rng);
    p2.library.shuffle(rng);

    for _ in 0..7 {
        draw_card(&mut p1);
        draw_card(&mut p2);
    }

    let mut turns = 0;
    for t in 1..=max_turns {
        turns = t;
        take_turn(&mut p1, &mut p2, card_db);
        if p2.life <= 0 {
            break;
        }

        take_turn(&mut p2, &mut p1, card_db);
        if p1.life <= 0 {
            break;
        }
    }

    let winner = if p1.life <= 0 && p2.life <= 0 {
        "Draw".to_string()
    } else if p2.life <= 0 {
        p1.name.clone()
    } else if p1.life <= 0 {
        p2.name.clone()
    } else if p1.life > p2.life {
        p1.name.clone()
    } else if p2.life > p1.life {
        p2.name.clone()
    } else {
        "Draw".to_string()
    };

    let mut p1_seen: Vec<String> = p1.cards_seen.into_iter().collect();
    let mut p2_seen: Vec<String> = p2.cards_seen.into_iter().collect();
    p1_seen.sort();
    p2_seen.sort();

    GameResult {
        game_number,
        winner,
        turns,
        player1_life: p1.life,
        player2_life: p2.life,
        player1_cards_seen: p1_seen,
        player2_cards_seen: p2_seen,
    }
}

fn take_turn(
    active: &mut PlayerState,
    defending: &mut PlayerState,
    card_db: &HashMap<String, CardProfile>,
) {
    draw_card(active);

    for c in &mut active.battlefield {
        c.summoning_sick = false;
    }

    play_land(active, card_db);
    cast_spells(active, defending, card_db);
    attack_step(active, defending);
}

fn draw_card(player: &mut PlayerState) {
    if let Some(card) = player.library.pop() {
        player.cards_seen.insert(card.name.clone());
        player.hand.push(card);
    }
}

fn play_land(player: &mut PlayerState, card_db: &HashMap<String, CardProfile>) {
    if let Some((idx, _)) = player
        .hand
        .iter()
        .enumerate()
        .find(|(_, c)| matches!(card_kind_for(c, card_db), CardKind::Land))
    {
        let land = player.hand.remove(idx);
        player.cards_seen.insert(land.name);
        player.lands_in_play += 1;
    }
}

fn cast_spells(
    active: &mut PlayerState,
    defending: &mut PlayerState,
    card_db: &HashMap<String, CardProfile>,
) {
    let mut mana_available = active.lands_in_play;

    loop {
        let mut playable: Vec<(usize, u32)> = active
            .hand
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let profile = card_profile_for(c, card_db)?;
                if profile.cmc <= mana_available {
                    Some((i, profile.cmc))
                } else {
                    None
                }
            })
            .collect();

        if playable.is_empty() {
            break;
        }

        playable.sort_by_key(|(_, cmc)| *cmc);

        let burn_idx = playable.iter().find_map(|(i, _)| {
            let profile = card_profile_for(&active.hand[*i], card_db)?;
            if let CardKind::Burn { damage } = profile.kind {
                if damage >= defending.life {
                    return Some(*i);
                }
            }
            None
        });

        let cast_idx = burn_idx.unwrap_or(playable[0].0);
        let card = active.hand.remove(cast_idx);

        if let Some(profile) = card_profile_for(&card, card_db) {
            mana_available = mana_available.saturating_sub(profile.cmc);
            active.cards_seen.insert(card.name.clone());
            resolve_spell(active, defending, card, profile);
        } else {
            active.graveyard.push(card);
        }
    }
}

fn resolve_spell(
    active: &mut PlayerState,
    defending: &mut PlayerState,
    card: DeckCard,
    profile: &CardProfile,
) {
    match profile.kind {
        CardKind::Creature { power, toughness } => {
            active.battlefield.push(CreaturePermanent {
                card_name: profile.name.clone(),
                power,
                toughness,
                summoning_sick: true,
            });
        }
        CardKind::Burn { damage } => {
            defending.life -= damage;
            active.graveyard.push(card);
        }
        _ => {
            active.graveyard.push(card);
        }
    }
}

fn attack_step(active: &mut PlayerState, defending: &mut PlayerState) {
    let mut attackers: Vec<usize> = active
        .battlefield
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.summoning_sick)
        .map(|(i, _)| i)
        .collect();

    attackers.sort_by_key(|&i| Reverse(active.battlefield[i].power));

    let mut blockers: Vec<usize> = defending
        .battlefield
        .iter()
        .enumerate()
        .map(|(i, _)| i)
        .collect();
    blockers.sort_by_key(|&i| Reverse(defending.battlefield[i].toughness));

    let mut to_kill_attacker = HashSet::new();
    let mut to_kill_blocker = HashSet::new();

    for (att_i, blk_i) in attackers.iter().zip(blockers.iter()) {
        let attacker = &active.battlefield[*att_i];
        let blocker = &defending.battlefield[*blk_i];

        if attacker.power >= blocker.toughness {
            to_kill_blocker.insert(*blk_i);
        }
        if blocker.power >= attacker.toughness {
            to_kill_attacker.insert(*att_i);
        }
    }

    let unblocked = attackers.len().saturating_sub(blockers.len());
    for att_i in attackers.into_iter().skip(blockers.len()).take(unblocked) {
        defending.life -= active.battlefield[att_i].power;
    }

    remove_dead_creatures(
        &mut active.battlefield,
        &mut active.graveyard,
        &to_kill_attacker,
    );
    remove_dead_creatures(
        &mut defending.battlefield,
        &mut defending.graveyard,
        &to_kill_blocker,
    );
}

fn remove_dead_creatures(
    battlefield: &mut Vec<CreaturePermanent>,
    graveyard: &mut Vec<DeckCard>,
    dead_indices: &HashSet<usize>,
) {
    let mut survivors = Vec::with_capacity(battlefield.len());
    for (i, creature) in battlefield.drain(..).enumerate() {
        if dead_indices.contains(&i) {
            graveyard.push(DeckCard {
                name: creature.card_name,
            });
        } else {
            survivors.push(creature);
        }
    }
    *battlefield = survivors;
}

fn load_card_db(path: &Path) -> Result<HashMap<String, CardProfile>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read card DB from {}", path.display()))?;
    let cards: Vec<ScryfallCard> =
        serde_json::from_str(&raw).context("failed parsing card DB JSON")?;

    let mut map = HashMap::new();
    for c in cards {
        let key = normalize_name(&c.name);
        map.entry(key)
            .or_insert_with(|| CardProfile::from_scryfall(c));
    }

    Ok(map)
}

impl CardProfile {
    fn from_scryfall(c: ScryfallCard) -> Self {
        let cmc = c.cmc.ceil() as u32;
        let kind = classify_card(&c);
        Self {
            name: c.name,
            kind,
            cmc,
            is_basic_land: c.type_line.to_lowercase().contains("basic land"),
        }
    }
}

fn classify_card(card: &ScryfallCard) -> CardKind {
    let type_line_l = card.type_line.to_lowercase();

    if type_line_l.contains("land") {
        return CardKind::Land;
    }

    if type_line_l.contains("creature") {
        let p = card.power.as_deref().and_then(parse_int).unwrap_or(1);
        let t = card.toughness.as_deref().and_then(parse_int).unwrap_or(1);
        return CardKind::Creature {
            power: p,
            toughness: t,
        };
    }

    if let Some(text) = &card.oracle_text {
        if let Some(dmg) = parse_damage(text) {
            return CardKind::Burn { damage: dmg };
        }
    }

    CardKind::OtherSpell
}

fn parse_int(s: &str) -> Option<i32> {
    s.parse::<i32>().ok()
}

fn parse_damage(text: &str) -> Option<i32> {
    let re = Regex::new(
        r"deals (\d+) damage to (?:any target|target creature|target player|each opponent)",
    )
    .ok()?;
    let lower = text.to_lowercase();
    let captures = re.captures(&lower)?;
    captures.get(1)?.as_str().parse::<i32>().ok()
}

fn load_deck(path: &Path) -> Result<Vec<DeckCard>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read deck list at {}", path.display()))?;

    let mut deck = Vec::new();
    let mut in_sideboard = false;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.eq_ignore_ascii_case("sideboard") {
            in_sideboard = true;
            continue;
        }
        if in_sideboard {
            continue;
        }

        let (count, card_name) = parse_deck_line(line).ok_or_else(|| {
            anyhow!(
                "invalid deck line '{}', expected '<count> <card name>'",
                line
            )
        })?;

        for _ in 0..count {
            deck.push(DeckCard {
                name: card_name.to_string(),
            });
        }
    }

    if deck.is_empty() {
        return Err(anyhow!("deck {} has zero cards", path.display()));
    }

    Ok(deck)
}

fn parse_deck_line(line: &str) -> Option<(usize, &str)> {
    let mut parts = line.splitn(2, ' ');
    let count = parts.next()?.parse::<usize>().ok()?;
    let name = parts.next()?.trim();
    if name.is_empty() {
        return None;
    }
    Some((count, name))
}

fn card_profile_for<'a>(
    card: &DeckCard,
    card_db: &'a HashMap<String, CardProfile>,
) -> Option<&'a CardProfile> {
    card_db.get(&normalize_name(&card.name))
}

fn card_kind_for(card: &DeckCard, card_db: &HashMap<String, CardProfile>) -> CardKind {
    card_profile_for(card, card_db)
        .map(|p| p.kind.clone())
        .unwrap_or(CardKind::OtherSpell)
}

fn normalize_name(name: &str) -> String {
    name.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_deck_line() {
        let parsed = parse_deck_line("4 Lightning Bolt").unwrap();
        assert_eq!(parsed.0, 4);
        assert_eq!(parsed.1, "Lightning Bolt");
    }

    #[test]
    fn test_parse_damage() {
        assert_eq!(
            parse_damage("Lightning Bolt deals 3 damage to any target."),
            Some(3)
        );
        assert_eq!(parse_damage("Draw two cards."), None);
    }

    #[test]
    fn test_parse_deck_ignores_sideboard() {
        let p = std::env::temp_dir().join("mtngin_test_deck.deck");
        fs::write(
            &p,
            "4 Lightning Bolt\n56 Mountain\nSideboard\n15 Shatterstorm\n",
        )
        .unwrap();
        let deck = load_deck(&p).unwrap();
        assert_eq!(deck.len(), 60);
    }

    #[test]
    fn test_rules_for_modern() {
        let rules = rules_for_format("modern").unwrap();
        assert_eq!(rules.minimum_deck_size, 60);
        assert_eq!(rules.max_non_basic_copies, 4);
    }
}
