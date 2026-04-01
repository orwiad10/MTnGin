# MTnGin

MTnGin is a terminal-based Magic: The Gathering simulation engine in Rust.

## Features

- Bot-vs-bot simulation with simple heuristics.
- YAML-driven runtime configuration.
- Deck import from plain text decklists (`<count> <card name>` lines).
- Deck import from plain text **or YAML** decklists.
- Sideboard sections in deck files are ignored (`Sideboard` marker and following lines).
- Local Scryfall Oracle DB bootstrap (`mtngin init`) and lookup during simulation.
- Iteration reports with winners, life totals, turn count, and cards seen by each player.
- Format-aware deck validation (minimum deck size + copy limits by format) for Standard/Pioneer/Modern/Legacy/Vintage/Pauper/Commander.
- Custom `yard` format support:
  - Mono-black/colorless card pool enforcement.
  - Shared deck and shared graveyard with poker-style dealing from one deck list.
  - Special handling for `Bridge from Below` in personal graveyards.

## Usage

### 1) Initialize local Scryfall card database

```bash
cargo run -- init --db-path data/scryfall-oracle-cards.json
```

### 2) Create decklists

Example `decks/red.deck`:

```text
24 Mountain
4 Lightning Bolt
4 Monastery Swiftspear
4 Viashino Pyromancer
24 Shock
```

Example YAML deck `decks/yard_sample.yaml`:

```yaml
cards:
  - count: 6
    name: Blood Pet
  - count: 6
    name: Bridge from Below
  # ...
```

### 3) Create run config

Example `run.yaml`:

```yaml
format: modern
iterations: 100
seed: 42
max_turns: 30
db_path: data/scryfall-oracle-cards.json
output_path: out/report.json

player1:
  name: RedBot-A
  deck_path: decks/red.deck
  starting_life: 20

player2:
  name: RedBot-B
  deck_path: decks/red.deck
  starting_life: 20
```

### 4) Run simulations

```bash
cargo run -- run --config run.yaml
```

## Notes

This engine models a pragmatic MTG subset (lands, creature combat, burn-style direct damage, basic casting heuristics) rather than full comprehensive rules enforcement.
