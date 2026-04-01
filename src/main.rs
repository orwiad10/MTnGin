use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use rand::{rngs::StdRng, seq::SliceRandom, SeedableRng};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SCRYFALL_BULK_ENDPOINT: &str = "https://api.scryfall.com/bulk-data";
const HTTP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
const STARTING_HAND_SIZE: usize = 7;
const MAX_HAND_SIZE: usize = 7;

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
    color_identity: Option<Vec<String>>,
    power: Option<String>,
    toughness: Option<String>,
}

#[derive(Debug, Clone)]
struct CardProfile {
    name: String,
    kind: CardKind,
    cmc: u32,
    is_basic_land: bool,
    is_mono_black_legal: bool,
}

#[derive(Debug, Clone)]
enum CardKind {
    Land,
    Creature { power: i32, toughness: i32 },
    Burn { damage: i32 },
    ManaRitual { mana: u32 },
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
    mana_pool: u32,
    cards_seen: HashSet<String>,
}

#[derive(Debug, Clone, Copy)]
enum TurnPhase {
    Untap,
    Draw,
    Main1,
    Combat,
    Main2,
    End,
}

impl TurnPhase {
    fn label(self) -> &'static str {
        match self {
            TurnPhase::Untap => "Untap",
            TurnPhase::Draw => "Draw",
            TurnPhase::Main1 => "Main Phase 1",
            TurnPhase::Combat => "Combat",
            TurnPhase::Main2 => "Main Phase 2",
            TurnPhase::End => "End Step",
        }
    }
}

#[derive(Debug, Clone)]
struct StackItem {
    card: DeckCard,
    profile: CardProfile,
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
    avg_turns: f64,
    ending_turns: Vec<usize>,
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
    turn_logs: Vec<TurnLog>,
}

#[derive(Debug, Serialize)]
struct TurnLog {
    turn: usize,
    player1: PlayerTurnLog,
    player2: Option<PlayerTurnLog>,
}

#[derive(Debug, Serialize)]
struct PlayerTurnLog {
    player_name: String,
    life_before: i32,
    life_after: i32,
    hand_before: usize,
    hand_after: usize,
    battlefield_before: usize,
    battlefield_after: usize,
    actions: Vec<String>,
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

    let client = reqwest::blocking::Client::builder()
        .user_agent(HTTP_USER_AGENT)
        .timeout(Duration::from_secs(120))
        .build()
        .context("failed to create HTTP client")?;
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
    let mut turn_sum = 0usize;

    let seed = cfg.seed.unwrap_or(42);
    let mut rng = StdRng::seed_from_u64(seed);
    for game_number in 1..=cfg.iterations {
        let result = if cfg.format.eq_ignore_ascii_case("yard") {
            simulate_game_yard(game_number, &cfg, &p1_deck, &card_db, max_turns, &mut rng)
        } else {
            simulate_game(
                game_number,
                &cfg,
                &p1_deck,
                &p2_deck,
                &card_db,
                max_turns,
                &mut rng,
            )
        };

        p1_life_sum += result.player1_life as i64;
        p2_life_sum += result.player2_life as i64;
        turn_sum += result.turns;
        report.ending_turns.push(result.turns);

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
    report.avg_turns = turn_sum as f64 / cfg.iterations as f64;

    let output = serde_json::to_string_pretty(&report)?;
    if let Some(path) = cfg.output_path {
        let output_path = Path::new(&path);
        if let Some(parent) = output_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create output directory {}", parent.display())
            })?;
        }

        fs::write(output_path, &output)
            .with_context(|| format!("failed to write simulation report to {}", path))?;
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
    if cfg.format.eq_ignore_ascii_case("yard")
        && cfg.player1.deck_path.trim() != cfg.player2.deck_path.trim()
    {
        return Err(anyhow!(
            "yard format requires both players to reference the same deck_path"
        ));
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
        "yard" => Some(FormatRules {
            minimum_deck_size: 60,
            max_non_basic_copies: 0,
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
    let is_yard = format.eq_ignore_ascii_case("yard");

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
        if !is_yard && !is_basic_land && *entry > rules.max_non_basic_copies {
            return Err(anyhow!(
                "card '{}' has {} copies, max allowed in {} is {}",
                profile.name,
                entry,
                format,
                rules.max_non_basic_copies
            ));
        }
        if is_yard && !profile.is_mono_black_legal {
            return Err(anyhow!(
                "yard format only allows mono-black/colorless cards; '{}' is not legal",
                profile.name
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
        mana_pool: 0,
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
        mana_pool: 0,
        cards_seen: HashSet::new(),
    };

    p1.library.shuffle(rng);
    p2.library.shuffle(rng);

    for _ in 0..STARTING_HAND_SIZE {
        let _ = draw_card(&mut p1);
        let _ = draw_card(&mut p2);
    }

    let mut turns = 0;
    let mut turn_logs = Vec::new();
    for t in 1..=max_turns {
        turns = t;
        let p1_turn = take_turn(&mut p1, &mut p2, card_db);
        if p2.life <= 0 {
            turn_logs.push(TurnLog {
                turn: t,
                player1: p1_turn,
                player2: None,
            });
            break;
        }

        let p2_turn = take_turn(&mut p2, &mut p1, card_db);
        turn_logs.push(TurnLog {
            turn: t,
            player1: p1_turn,
            player2: Some(p2_turn),
        });
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
        turn_logs,
    }
}

fn simulate_game_yard(
    game_number: usize,
    cfg: &RunConfig,
    shared_deck: &[DeckCard],
    card_db: &HashMap<String, CardProfile>,
    max_turns: usize,
    rng: &mut StdRng,
) -> GameResult {
    let mut p1 = PlayerState {
        name: cfg.player1.name.clone(),
        life: cfg.player1.starting_life.unwrap_or(20),
        library: Vec::new(),
        hand: Vec::new(),
        battlefield: Vec::new(),
        graveyard: Vec::new(),
        lands_in_play: 1,
        mana_pool: 0,
        cards_seen: HashSet::new(),
    };
    let mut p2 = PlayerState {
        name: cfg.player2.name.clone(),
        life: cfg.player2.starting_life.unwrap_or(20),
        library: Vec::new(),
        hand: Vec::new(),
        battlefield: Vec::new(),
        graveyard: Vec::new(),
        lands_in_play: 1,
        mana_pool: 0,
        cards_seen: HashSet::new(),
    };

    let mut shared_library = shared_deck.to_vec();
    shared_library.shuffle(rng);
    let mut shared_graveyard: Vec<DeckCard> = Vec::new();

    for _ in 0..STARTING_HAND_SIZE {
        let _ = draw_card_from_shared(&mut p1, &mut shared_library);
        let _ = draw_card_from_shared(&mut p2, &mut shared_library);
    }

    let mut turns = 0;
    let mut turn_logs = Vec::new();
    for t in 1..=max_turns {
        turns = t;
        let p1_turn = take_turn_yard(
            &mut p1,
            &mut p2,
            card_db,
            &mut shared_library,
            &mut shared_graveyard,
        );
        if p2.life <= 0 {
            turn_logs.push(TurnLog {
                turn: t,
                player1: p1_turn,
                player2: None,
            });
            break;
        }
        let p2_turn = take_turn_yard(
            &mut p2,
            &mut p1,
            card_db,
            &mut shared_library,
            &mut shared_graveyard,
        );
        turn_logs.push(TurnLog {
            turn: t,
            player1: p1_turn,
            player2: Some(p2_turn),
        });
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
        turn_logs,
    }
}

fn take_turn_yard(
    active: &mut PlayerState,
    defending: &mut PlayerState,
    card_db: &HashMap<String, CardProfile>,
    shared_library: &mut Vec<DeckCard>,
    shared_graveyard: &mut Vec<DeckCard>,
) -> PlayerTurnLog {
    let mut actions = Vec::new();
    let life_before = active.life;
    let hand_before = active.hand.len();
    let battlefield_before = active.battlefield.len();

    actions.push(format!("== {} ==", TurnPhase::Untap.label()));
    active.mana_pool = 0;
    for c in &mut active.battlefield {
        c.summoning_sick = false;
    }
    actions.push("Untapped permanents and cleared summoning sickness.".to_string());

    actions.push(format!("== {} ==", TurnPhase::Draw.label()));
    actions.push(draw_card_from_shared(active, shared_library));
    if active.life <= 0 {
        return PlayerTurnLog {
            player_name: active.name.clone(),
            life_before,
            life_after: active.life,
            hand_before,
            hand_after: active.hand.len(),
            battlefield_before,
            battlefield_after: active.battlefield.len(),
            actions,
        };
    }

    actions.push(format!("== {} ==", TurnPhase::Main1.label()));
    if let Some(action) = play_land(active, card_db) {
        actions.push(action);
    }
    actions.extend(activate_mana_abilities_yard(
        active,
        card_db,
        shared_graveyard,
    ));
    actions.extend(cast_priority_spells_yard(
        active,
        defending,
        card_db,
        shared_library,
        shared_graveyard,
    ));
    actions.extend(cast_spells_yard(
        active,
        defending,
        card_db,
        shared_graveyard,
    ));

    actions.push(format!("== {} ==", TurnPhase::Combat.label()));
    actions.extend(attack_step_yard(active, defending, shared_graveyard));
    actions.push(format!("== {} ==", TurnPhase::Main2.label()));
    actions.extend(cast_spells_yard(
        active,
        defending,
        card_db,
        shared_graveyard,
    ));
    actions.push(format!("== {} ==", TurnPhase::End.label()));
    active.mana_pool = 0;
    actions.extend(discard_down_to_hand_size_yard(
        active,
        card_db,
        shared_graveyard,
        MAX_HAND_SIZE,
    ));
    PlayerTurnLog {
        player_name: active.name.clone(),
        life_before,
        life_after: active.life,
        hand_before,
        hand_after: active.hand.len(),
        battlefield_before,
        battlefield_after: active.battlefield.len(),
        actions,
    }
}

fn take_turn(
    active: &mut PlayerState,
    defending: &mut PlayerState,
    card_db: &HashMap<String, CardProfile>,
) -> PlayerTurnLog {
    let mut actions = Vec::new();
    let life_before = active.life;
    let hand_before = active.hand.len();
    let battlefield_before = active.battlefield.len();

    actions.push(format!("== {} ==", TurnPhase::Untap.label()));
    active.mana_pool = 0;
    for c in &mut active.battlefield {
        c.summoning_sick = false;
    }
    actions.push("Untapped permanents and cleared summoning sickness.".to_string());

    actions.push(format!("== {} ==", TurnPhase::Draw.label()));
    actions.push(draw_card(active));
    if active.life <= 0 {
        return PlayerTurnLog {
            player_name: active.name.clone(),
            life_before,
            life_after: active.life,
            hand_before,
            hand_after: active.hand.len(),
            battlefield_before,
            battlefield_after: active.battlefield.len(),
            actions,
        };
    }

    actions.push(format!("== {} ==", TurnPhase::Main1.label()));
    if let Some(action) = play_land(active, card_db) {
        actions.push(action);
    }
    actions.extend(cast_spells(active, defending, card_db));
    actions.push(format!("== {} ==", TurnPhase::Combat.label()));
    actions.extend(attack_step(active, defending));
    actions.push(format!("== {} ==", TurnPhase::Main2.label()));
    actions.extend(cast_spells(active, defending, card_db));
    actions.push(format!("== {} ==", TurnPhase::End.label()));
    active.mana_pool = 0;
    actions.extend(discard_down_to_hand_size(active, card_db, MAX_HAND_SIZE));
    PlayerTurnLog {
        player_name: active.name.clone(),
        life_before,
        life_after: active.life,
        hand_before,
        hand_after: active.hand.len(),
        battlefield_before,
        battlefield_after: active.battlefield.len(),
        actions,
    }
}

fn draw_card(player: &mut PlayerState) -> String {
    if let Some(card) = player.library.pop() {
        player.cards_seen.insert(card.name.clone());
        let card_name = card.name.clone();
        player.hand.push(card);
        format!("Drew card: {}", card_name)
    } else {
        player.life = 0;
        "Tried to draw from empty library and lost the game.".to_string()
    }
}

fn draw_card_from_shared(player: &mut PlayerState, shared_library: &mut Vec<DeckCard>) -> String {
    if let Some(card) = shared_library.pop() {
        player.cards_seen.insert(card.name.clone());
        let card_name = card.name.clone();
        player.hand.push(card);
        format!("Drew shared card: {}", card_name)
    } else {
        player.life = 0;
        "Tried to draw from empty shared library and lost the game.".to_string()
    }
}

fn discard_down_to_hand_size(
    player: &mut PlayerState,
    card_db: &HashMap<String, CardProfile>,
    max_hand_size: usize,
) -> Vec<String> {
    let mut actions = Vec::new();
    while player.hand.len() > max_hand_size {
        let discard_idx = choose_discard_index(player, card_db);
        let card = player.hand.remove(discard_idx);
        let card_name = card.name.clone();
        player.graveyard.push(card);
        actions.push(format!(
            "Discarded {} to hand size ({}).",
            card_name, max_hand_size
        ));
    }
    actions
}

fn discard_down_to_hand_size_yard(
    player: &mut PlayerState,
    card_db: &HashMap<String, CardProfile>,
    shared_graveyard: &mut Vec<DeckCard>,
    max_hand_size: usize,
) -> Vec<String> {
    let mut actions = Vec::new();
    while player.hand.len() > max_hand_size {
        let discard_idx = choose_discard_index(player, card_db);
        let card = player.hand.remove(discard_idx);
        let card_name = card.name.clone();
        put_in_yard_graveyard(player, card, shared_graveyard);
        actions.push(format!(
            "Discarded {} to hand size ({}).",
            card_name, max_hand_size
        ));
    }
    actions
}

fn choose_discard_index(player: &PlayerState, card_db: &HashMap<String, CardProfile>) -> usize {
    player
        .hand
        .iter()
        .enumerate()
        .max_by_key(|(_, card)| {
            card_profile_for(card, card_db)
                .map(|profile| profile.cmc)
                .unwrap_or(0)
        })
        .map(|(idx, _)| idx)
        .unwrap_or(0)
}

fn play_land(player: &mut PlayerState, card_db: &HashMap<String, CardProfile>) -> Option<String> {
    if let Some((idx, _)) = player
        .hand
        .iter()
        .enumerate()
        .find(|(_, c)| matches!(card_kind_for(c, card_db), CardKind::Land))
    {
        let land = player.hand.remove(idx);
        let land_name = land.name.clone();
        player.cards_seen.insert(land.name);
        player.lands_in_play += 1;
        return Some(format!(
            "Played land: {} (lands in play: {}).",
            land_name, player.lands_in_play
        ));
    }
    None
}

fn cast_spells(
    active: &mut PlayerState,
    defending: &mut PlayerState,
    card_db: &HashMap<String, CardProfile>,
) -> Vec<String> {
    let mut actions = Vec::new();
    let mut mana_available = active.lands_in_play + active.mana_pool;

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

        playable.sort_by_key(|(_, cmc)| Reverse(*cmc));

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
        let card_name = card.name.clone();

        if let Some(profile) = card_profile_for(&card, card_db) {
            mana_available = mana_available.saturating_sub(profile.cmc);
            active.cards_seen.insert(card.name.clone());
            let mut stack = vec![StackItem {
                card,
                profile: profile.clone(),
            }];
            actions.push(format!("Put {} on the stack.", card_name));
            actions.extend(resolve_stack(
                &mut stack,
                active,
                defending,
                card_db,
                false,
                &mut Vec::new(),
            ));
        } else {
            actions.push(format!("Cast unknown spell {}.", card_name));
            active.graveyard.push(DeckCard { name: card_name });
        }
    }
    active.mana_pool = mana_available.saturating_sub(active.lands_in_play);
    actions
}

fn cast_spells_yard(
    active: &mut PlayerState,
    defending: &mut PlayerState,
    card_db: &HashMap<String, CardProfile>,
    shared_graveyard: &mut Vec<DeckCard>,
) -> Vec<String> {
    let mut actions = Vec::new();
    let mut mana_available = active.lands_in_play + active.mana_pool;

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
        playable.sort_by_key(|(_, cmc)| Reverse(*cmc));

        let cast_idx = choose_best_yard_spell(active, defending, card_db, &playable);
        let card = active.hand.remove(cast_idx);
        let card_name = card.name.clone();

        if let Some(profile) = card_profile_for(&card, card_db) {
            let effective_cost = effective_yard_cost(&card_name, profile.cmc);
            mana_available = mana_available.saturating_sub(effective_cost);
            if let CardKind::ManaRitual { mana } = profile.kind {
                mana_available = mana_available.saturating_add(mana);
            }
            active.cards_seen.insert(card.name.clone());
            let mut stack = vec![StackItem {
                card,
                profile: profile.clone(),
            }];
            actions.push(format!("Put {} on the stack.", card_name));
            actions.extend(resolve_stack(
                &mut stack,
                active,
                defending,
                card_db,
                true,
                shared_graveyard,
            ));
        } else {
            actions.push(format!("Cast unknown shared spell {}.", card_name));
            shared_graveyard.push(DeckCard { name: card_name });
        }
    }
    active.mana_pool = mana_available.saturating_sub(active.lands_in_play);
    actions
}

fn maybe_response_spell(
    responder: &mut PlayerState,
    opponent: &PlayerState,
    card_db: &HashMap<String, CardProfile>,
    yard_mode: bool,
) -> Option<StackItem> {
    let mana_available = responder.lands_in_play + responder.mana_pool;
    let lethal_idx = responder.hand.iter().enumerate().find_map(|(i, card)| {
        let profile = card_profile_for(card, card_db)?;
        if profile.cmc > mana_available {
            return None;
        }
        if let CardKind::Burn { damage } = profile.kind {
            if damage >= opponent.life {
                return Some(i);
            }
        }
        None
    })?;

    let card = responder.hand.remove(lethal_idx);
    let profile = card_profile_for(&card, card_db)?.clone();
    let cost = if yard_mode {
        effective_yard_cost(&card.name, profile.cmc)
    } else {
        profile.cmc
    };
    responder.mana_pool = (responder.lands_in_play + responder.mana_pool).saturating_sub(cost);
    Some(StackItem { card, profile })
}

fn choose_best_yard_spell(
    active: &PlayerState,
    defending: &PlayerState,
    card_db: &HashMap<String, CardProfile>,
    playable: &[(usize, u32)],
) -> usize {
    if let Some((idx, _)) = playable.iter().find(|(i, _)| {
        card_profile_for(&active.hand[*i], card_db)
            .map(|p| matches!(p.kind, CardKind::Burn { damage } if damage >= defending.life))
            .unwrap_or(false)
    }) {
        return *idx;
    }
    if let Some((idx, _)) = playable.iter().find(|(i, _)| {
        normalize_name(&active.hand[*i].name) == "reanimate" && !defending.graveyard.is_empty()
    }) {
        return *idx;
    }
    playable[0].0
}

fn effective_yard_cost(card_name: &str, base_cmc: u32) -> u32 {
    match normalize_name(card_name).as_str() {
        "street wraith" => 0,
        "contagion" => 0,
        _ => base_cmc,
    }
}

fn cast_priority_spells_yard(
    active: &mut PlayerState,
    defending: &mut PlayerState,
    card_db: &HashMap<String, CardProfile>,
    shared_library: &mut Vec<DeckCard>,
    shared_graveyard: &mut Vec<DeckCard>,
) -> Vec<String> {
    let mut actions = Vec::new();
    loop {
        let Some(idx) = active
            .hand
            .iter()
            .position(|c| normalize_name(&c.name) == "street wraith")
        else {
            break;
        };
        if active.life <= 2 {
            break;
        }
        let wraith = active.hand.remove(idx);
        active.life -= 2;
        put_in_yard_graveyard(active, wraith, shared_graveyard);
        actions.push(format!(
            "Cycled Street Wraith by paying 2 life ({} life now {}).",
            active.name, active.life
        ));
        actions.push(draw_card_from_shared(active, shared_library));
    }

    if let Some(idx) = active
        .hand
        .iter()
        .position(|c| normalize_name(&c.name) == "reanimate")
    {
        let targets = defending
            .graveyard
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let p = card_profile_for(c, card_db)?;
                if let CardKind::Creature { power, toughness } = p.kind {
                    Some((i, p.name.clone(), power, toughness, p.cmc))
                } else {
                    None
                }
            })
            .max_by_key(|(_, _, power, _, _)| *power);
        if let Some((target_idx, target_name, power, toughness, cmc)) = targets {
            active.hand.remove(idx);
            let resurrected = defending.graveyard.remove(target_idx);
            active.life -= cmc as i32;
            active.battlefield.push(CreaturePermanent {
                card_name: target_name.clone(),
                power,
                toughness,
                summoning_sick: true,
            });
            put_in_yard_graveyard(
                active,
                DeckCard {
                    name: "Reanimate".to_string(),
                },
                shared_graveyard,
            );
            actions.push(format!(
                "Cast Reanimate targeting {} (lost {} life, {} life now).",
                target_name, cmc, active.life
            ));
            put_in_yard_graveyard(defending, resurrected, shared_graveyard);
        }
    }
    actions
}

fn activate_mana_abilities_yard(
    active: &mut PlayerState,
    card_db: &HashMap<String, CardProfile>,
    shared_graveyard: &mut Vec<DeckCard>,
) -> Vec<String> {
    let mut actions = Vec::new();
    while let Some(idx) = active
        .battlefield
        .iter()
        .position(|c| normalize_name(&c.card_name) == "blood pet")
    {
        let need_mana = active.hand.iter().any(|c| {
            card_profile_for(c, card_db)
                .map(|p| p.cmc > active.lands_in_play + active.mana_pool)
                .unwrap_or(false)
        });
        if !need_mana {
            break;
        }
        let pet = active.battlefield.remove(idx);
        active.mana_pool += 1;
        shared_graveyard.push(DeckCard {
            name: pet.card_name.clone(),
        });
        actions.push(format!(
            "Activated Blood Pet: sacrificed for {{B}} (pool {}).",
            active.mana_pool
        ));
    }
    actions
}

fn resolve_stack(
    stack: &mut Vec<StackItem>,
    active: &mut PlayerState,
    defending: &mut PlayerState,
    card_db: &HashMap<String, CardProfile>,
    yard_mode: bool,
    shared_graveyard: &mut Vec<DeckCard>,
) -> Vec<String> {
    let mut actions = Vec::new();
    while let Some(item) = stack.pop() {
        if let Some(response) = maybe_response_spell(defending, active, card_db, yard_mode) {
            actions.push(format!(
                "{} responds with {}.",
                defending.name, response.profile.name
            ));
            stack.push(item);
            stack.push(response);
            continue;
        }

        if yard_mode {
            actions.push(resolve_spell_yard(
                active,
                defending,
                item.card,
                &item.profile,
                shared_graveyard,
            ));
        } else {
            actions.push(resolve_spell(active, defending, item.card, &item.profile));
        }
    }
    actions
}

fn resolve_spell(
    active: &mut PlayerState,
    defending: &mut PlayerState,
    card: DeckCard,
    profile: &CardProfile,
) -> String {
    match profile.kind {
        CardKind::Creature { power, toughness } => {
            active.battlefield.push(CreaturePermanent {
                card_name: profile.name.clone(),
                power,
                toughness,
                summoning_sick: true,
            });
            format!("Cast creature {} ({}/{})", profile.name, power, toughness)
        }
        CardKind::Burn { damage } => {
            defending.life -= damage;
            active.graveyard.push(card);
            format!(
                "Cast burn {} for {} damage to {} (life now {}).",
                profile.name, damage, defending.name, defending.life
            )
        }
        CardKind::ManaRitual { mana } => {
            active.mana_pool += mana;
            active.graveyard.push(card);
            format!(
                "Resolved ritual {} and added {} mana (pool {}).",
                profile.name, mana, active.mana_pool
            )
        }
        _ => {
            active.graveyard.push(card);
            format!("Cast spell {}.", profile.name)
        }
    }
}

fn resolve_spell_yard(
    active: &mut PlayerState,
    defending: &mut PlayerState,
    card: DeckCard,
    profile: &CardProfile,
    shared_graveyard: &mut Vec<DeckCard>,
) -> String {
    match profile.kind {
        CardKind::Creature { power, toughness } => {
            active.battlefield.push(CreaturePermanent {
                card_name: profile.name.clone(),
                power,
                toughness,
                summoning_sick: true,
            });
            format!("Cast creature {} ({}/{})", profile.name, power, toughness)
        }
        CardKind::Burn { damage } => {
            defending.life -= damage;
            put_in_yard_graveyard(active, card, shared_graveyard);
            format!(
                "Cast burn {} for {} damage to {} (life now {}).",
                profile.name, damage, defending.name, defending.life
            )
        }
        CardKind::ManaRitual { mana } => {
            active.mana_pool += mana;
            put_in_yard_graveyard(active, card, shared_graveyard);
            format!(
                "Resolved ritual {} and added {} mana (pool {}).",
                profile.name, mana, active.mana_pool
            )
        }
        _ => {
            put_in_yard_graveyard(active, card, shared_graveyard);
            format!("Cast spell {}.", profile.name)
        }
    }
}

fn attack_step(active: &mut PlayerState, defending: &mut PlayerState) -> Vec<String> {
    let mut actions = Vec::new();
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
        let dmg = active.battlefield[att_i].power;
        let attacker_name = active.battlefield[att_i].card_name.clone();
        defending.life -= dmg;
        actions.push(format!(
            "{} attacked unblocked for {} damage ({} life: {}).",
            attacker_name, dmg, defending.name, defending.life
        ));
    }

    let attacker_deaths = remove_dead_creatures(
        &mut active.battlefield,
        &mut active.graveyard,
        &to_kill_attacker,
    );
    let blocker_deaths = remove_dead_creatures(
        &mut defending.battlefield,
        &mut defending.graveyard,
        &to_kill_blocker,
    );
    if attacker_deaths > 0 || blocker_deaths > 0 {
        actions.push(format!(
            "Combat trades: attackers lost {}, blockers lost {}.",
            attacker_deaths, blocker_deaths
        ));
    }
    actions
}

fn attack_step_yard(
    active: &mut PlayerState,
    defending: &mut PlayerState,
    shared_graveyard: &mut Vec<DeckCard>,
) -> Vec<String> {
    let mut actions = Vec::new();
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

    for att_i in attackers.into_iter().skip(blockers.len()) {
        let dmg = active.battlefield[att_i].power;
        let attacker_name = active.battlefield[att_i].card_name.clone();
        defending.life -= dmg;
        actions.push(format!(
            "{} attacked unblocked for {} damage ({} life: {}).",
            attacker_name, dmg, defending.name, defending.life
        ));
    }

    let active_deaths =
        remove_dead_creatures_yard(&mut active.battlefield, shared_graveyard, &to_kill_attacker);
    let defending_deaths = remove_dead_creatures_yard(
        &mut defending.battlefield,
        shared_graveyard,
        &to_kill_blocker,
    );

    apply_bridge_triggers(active_deaths, active, defending);
    apply_bridge_triggers(defending_deaths, defending, active);
    if active_deaths > 0 || defending_deaths > 0 {
        actions.push(format!(
            "Combat trades: attackers lost {}, blockers lost {}.",
            active_deaths, defending_deaths
        ));
    }
    actions
}

fn put_in_yard_graveyard(
    owner: &mut PlayerState,
    card: DeckCard,
    shared_graveyard: &mut Vec<DeckCard>,
) {
    if normalize_name(&card.name) == "bridge from below" {
        owner.graveyard.push(card);
    } else {
        shared_graveyard.push(card);
    }
}

fn remove_dead_creatures(
    battlefield: &mut Vec<CreaturePermanent>,
    graveyard: &mut Vec<DeckCard>,
    dead_indices: &HashSet<usize>,
) -> usize {
    let mut survivors = Vec::with_capacity(battlefield.len());
    let mut deaths = 0usize;
    for (i, creature) in battlefield.drain(..).enumerate() {
        if dead_indices.contains(&i) {
            deaths += 1;
            graveyard.push(DeckCard {
                name: creature.card_name,
            });
        } else {
            survivors.push(creature);
        }
    }
    *battlefield = survivors;
    deaths
}

fn remove_dead_creatures_yard(
    battlefield: &mut Vec<CreaturePermanent>,
    shared_graveyard: &mut Vec<DeckCard>,
    dead_indices: &HashSet<usize>,
) -> usize {
    let mut survivors = Vec::with_capacity(battlefield.len());
    let mut deaths = 0usize;
    for (i, creature) in battlefield.drain(..).enumerate() {
        if dead_indices.contains(&i) {
            deaths += 1;
            shared_graveyard.push(DeckCard {
                name: creature.card_name,
            });
        } else {
            survivors.push(creature);
        }
    }
    *battlefield = survivors;
    deaths
}

fn apply_bridge_triggers(deaths: usize, dead_owner: &mut PlayerState, opponent: &mut PlayerState) {
    for _ in 0..deaths {
        trigger_bridge_from_below(&dead_owner.graveyard, &mut dead_owner.battlefield, 1);
        trigger_bridge_from_below(&opponent.graveyard, &mut opponent.battlefield, 1);
    }
}

fn trigger_bridge_from_below(
    personal_graveyard: &[DeckCard],
    battlefield: &mut Vec<CreaturePermanent>,
    tokens: usize,
) {
    let bridge_triggers = personal_graveyard
        .iter()
        .filter(|card| normalize_name(&card.name) == "bridge from below")
        .count();
    for _ in 0..(bridge_triggers * tokens) {
        battlefield.push(CreaturePermanent {
            card_name: "Zombie Token".to_string(),
            power: 2,
            toughness: 2,
            summoning_sick: true,
        });
    }
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
        let is_mono_black_legal = c
            .color_identity
            .as_ref()
            .map(|ids| ids.iter().all(|id| id == "B"))
            .unwrap_or(true);
        Self {
            name: c.name,
            kind,
            cmc,
            is_basic_land: c.type_line.to_lowercase().contains("basic land"),
            is_mono_black_legal,
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
        if let Some(mana) = parse_mana_ritual(text) {
            return CardKind::ManaRitual { mana };
        }
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

fn parse_mana_ritual(text: &str) -> Option<u32> {
    let lower = text.to_lowercase();
    if !lower.contains("add") {
        return None;
    }
    let black_count = lower.matches("{b}").count() as u32;
    if black_count > 0 {
        return Some(black_count);
    }
    None
}

fn load_deck(path: &Path) -> Result<Vec<DeckCard>> {
    if matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yaml") | Some("yml")
    ) {
        return load_deck_from_yaml(path);
    }

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

#[derive(Debug, Deserialize)]
struct DeckListYaml {
    cards: Vec<DeckListYamlEntry>,
}

#[derive(Debug, Deserialize)]
struct DeckListYamlEntry {
    count: usize,
    name: String,
}

fn load_deck_from_yaml(path: &Path) -> Result<Vec<DeckCard>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read deck list at {}", path.display()))?;
    let parsed: DeckListYaml = serde_yaml::from_str(&raw)
        .with_context(|| format!("invalid deck YAML at {}", path.display()))?;

    let mut deck = Vec::new();
    for entry in parsed.cards {
        if entry.name.trim().is_empty() {
            return Err(anyhow!(
                "deck {} contains an empty card name",
                path.display()
            ));
        }
        for _ in 0..entry.count {
            deck.push(DeckCard {
                name: entry.name.clone(),
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

    #[test]
    fn test_rules_for_yard() {
        let rules = rules_for_format("yard").unwrap();
        assert_eq!(rules.minimum_deck_size, 60);
    }

    #[test]
    fn test_load_deck_from_yaml() {
        let p = std::env::temp_dir().join("mtngin_test_deck.yaml");
        fs::write(
            &p,
            "cards:\n  - count: 2\n    name: Swamp\n  - count: 1\n    name: Dark Ritual\n",
        )
        .unwrap();
        let deck = load_deck(&p).unwrap();
        assert_eq!(deck.len(), 3);
        assert_eq!(deck[0].name, "Swamp");
    }

    #[test]
    fn test_draw_from_empty_library_loses_game() {
        let mut player = PlayerState {
            name: "P1".to_string(),
            life: 20,
            library: Vec::new(),
            hand: Vec::new(),
            battlefield: Vec::new(),
            graveyard: Vec::new(),
            lands_in_play: 0,
            mana_pool: 0,
            cards_seen: HashSet::new(),
        };
        let action = draw_card(&mut player);
        assert!(action.contains("lost the game"));
        assert_eq!(player.life, 0);
    }

    #[test]
    fn test_attack_step_reports_unblocked_damage() {
        let mut active = PlayerState {
            name: "Atk".to_string(),
            life: 20,
            library: Vec::new(),
            hand: Vec::new(),
            battlefield: vec![CreaturePermanent {
                card_name: "Bear".to_string(),
                power: 2,
                toughness: 2,
                summoning_sick: false,
            }],
            graveyard: Vec::new(),
            lands_in_play: 0,
            mana_pool: 0,
            cards_seen: HashSet::new(),
        };
        let mut defending = PlayerState {
            name: "Def".to_string(),
            life: 20,
            library: Vec::new(),
            hand: Vec::new(),
            battlefield: Vec::new(),
            graveyard: Vec::new(),
            lands_in_play: 0,
            mana_pool: 0,
            cards_seen: HashSet::new(),
        };
        let actions = attack_step(&mut active, &mut defending);
        assert_eq!(defending.life, 18);
        assert!(actions.iter().any(|a| a.contains("unblocked for 2 damage")));
    }

    #[test]
    fn test_discard_down_to_hand_size() {
        let mut card_db = HashMap::new();
        card_db.insert(
            "dark ritual".to_string(),
            CardProfile {
                name: "Dark Ritual".to_string(),
                kind: CardKind::ManaRitual { mana: 3 },
                cmc: 1,
                is_basic_land: false,
                is_mono_black_legal: true,
            },
        );
        card_db.insert(
            "griselbrand".to_string(),
            CardProfile {
                name: "Griselbrand".to_string(),
                kind: CardKind::Creature {
                    power: 7,
                    toughness: 7,
                },
                cmc: 8,
                is_basic_land: false,
                is_mono_black_legal: true,
            },
        );
        let mut player = PlayerState {
            name: "P1".to_string(),
            life: 20,
            library: Vec::new(),
            hand: vec![
                DeckCard {
                    name: "Dark Ritual".to_string(),
                },
                DeckCard {
                    name: "Griselbrand".to_string(),
                },
            ],
            battlefield: Vec::new(),
            graveyard: Vec::new(),
            lands_in_play: 0,
            mana_pool: 0,
            cards_seen: HashSet::new(),
        };

        let actions = discard_down_to_hand_size(&mut player, &card_db, 1);
        assert_eq!(player.hand.len(), 1);
        assert_eq!(player.graveyard.len(), 1);
        assert_eq!(player.graveyard[0].name, "Griselbrand");
        assert!(actions[0].contains("Discarded Griselbrand"));
    }
}
